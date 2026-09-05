//! decl-lsp (docs/tooling/03_lsp.md): the language server over stdio — a
//! port of the reference implementation's lsp.ts. Every answer comes from
//! the same checker, inference, and engine as the command line, driven
//! through the session object (session.rs) with the open buffers
//! overriding the disk; positions come from the source ranges every AST
//! node carries, and the types and resolutions recorded while the checker
//! runs (infer.rs hooks). Messages are handled strictly in order, and the
//! server exits when its input closes.
use crate::ast::*;
use crate::checker::{check_module, CheckHooks};
use crate::fmt::{format, u16len};
use crate::infer::{resolve_in, std_path, type_text, Target, Ty, STD};
use crate::module::Module;
use crate::parse::parse_source;
use crate::semantics::{
    is_rec, json_str, parse_path, read_json, rec_members, seg_text, Diag, MKind, RTk, Seg,
    SlotState, Value, RT,
};
use crate::session::{fmt_diag, BindSource, Mode, Op, Run, Session};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

// ---------------- JSON out (key order kept) ----------------
#[derive(Clone)]
enum J {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}
impl J {
    fn obj(pairs: Vec<(&str, J)>) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }
    fn text(&self) -> String {
        match self {
            J::Null => "null".into(),
            J::Bool(b) => b.to_string(),
            J::Num(n) => n.to_string(),
            J::Str(s) => json_str(s),
            J::Arr(items) => format!(
                "[{}]",
                items.iter().map(J::text).collect::<Vec<_>>().join(",")
            ),
            J::Obj(es) => format!(
                "{{{}}}",
                es.iter()
                    .map(|(k, v)| format!("{}:{}", json_str(k), v.text()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

// ---------------- JSON in (over the runtime's Value) ----------------
fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::JObj(es) => es.iter().find(|(k, _)| k == key).map(|(_, x)| x),
        _ => None,
    }
}
fn as_str(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}
fn as_usize(v: Option<&Value>) -> Option<usize> {
    match v {
        Some(Value::Int(i)) => i.to_string().parse().ok(),
        Some(Value::Float(f)) => Some(*f as usize),
        _ => None,
    }
}
fn as_bool(v: Option<&Value>) -> Option<bool> {
    match v {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}
fn json_of(v: &Value) -> String {
    match v {
        Value::Null | Value::Undef | Value::Absent => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => crate::semantics::js_num_str(*f),
        Value::Str(s) => json_str(s),
        Value::JArr(items) => format!(
            "[{}]",
            items.iter().map(json_of).collect::<Vec<_>>().join(",")
        ),
        Value::JObj(es) => format!(
            "{{{}}}",
            es.iter()
                .map(|(k, x)| format!("{}:{}", json_str(k), json_of(x)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => json_str(&format!("{other:?}")),
    }
}

// ---------------- transport ----------------
fn send(body: &str) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = out.flush();
}
fn reply(id: Option<&Value>, result: J) {
    if let Some(id) = id {
        send(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}",
            json_of(id),
            result.text()
        ));
    }
}
fn notify(method: &str, params: J) {
    send(&format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":{},\"params\":{}}}",
        json_str(method),
        params.text()
    ));
}

// ---------------- documents ----------------
pub fn path_of(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let raw = raw.split(['?', '#']).next().unwrap_or(raw);
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).to_string())
}
pub fn uri_of(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[derive(Clone, Copy)]
struct Pos {
    line: usize,
    character: usize,
}
fn range_json(l: Loc) -> J {
    J::obj(vec![
        (
            "start",
            J::obj(vec![
                ("line", J::Num(l.sl as i64)),
                ("character", J::Num(l.sc as i64)),
            ]),
        ),
        (
            "end",
            J::obj(vec![
                ("line", J::Num(l.el as i64)),
                ("character", J::Num(l.ec as i64)),
            ]),
        ),
    ])
}
fn contains(l: Loc, p: Pos) -> bool {
    (l.sl < p.line || (l.sl == p.line && l.sc <= p.character))
        && (p.line < l.el || (p.line == l.el && p.character <= l.ec))
}
fn span(l: Loc) -> usize {
    (l.el - l.sl) * 100000 + l.ec.saturating_sub(l.sc)
}

// ---- columns: UTF-16 units (the protocol's, and the reference's) <-> bytes ----
fn u16_col(line: &str, byte: usize) -> usize {
    u16len(line.get(..byte).unwrap_or(line))
}
fn byte_col(line: &str, col: usize) -> usize {
    let mut units = 0;
    for (i, ch) in line.char_indices() {
        if units >= col {
            return i;
        }
        units += ch.len_utf16();
    }
    line.len()
}
/// JS `line.indexOf(needle, from)` in UTF-16 columns
fn find16(line: &str, needle: &str, from: usize) -> Option<usize> {
    let b = byte_col(line, from);
    line.get(b..)?.find(needle).map(|i| u16_col(line, b + i))
}
/// JS `line.lastIndexOf(needle, from)` in UTF-16 columns
fn rfind16(line: &str, needle: &str, from: usize) -> Option<usize> {
    let b = byte_col(line, from);
    let mut end = (b + needle.len()).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].rfind(needle).map(|i| u16_col(line, i))
}
fn slice16(line: &str, a: usize, b: usize) -> &str {
    let (ba, bb) = (byte_col(line, a), byte_col(line, b));
    line.get(ba..bb.max(ba)).unwrap_or("")
}

// ---------------- analysis ----------------
// one analysis per open document: its universe (the document as entry),
// and for every module the checker's tables — the type of every
// expression and what every name denotes
struct Tables {
    types: HashMap<usize, Ty>,
    res: HashMap<usize, Option<Target>>,
}
fn key_of(e: &Rc<Expr>) -> usize {
    Rc::as_ptr(e) as *const u8 as usize
}
struct Analysis {
    text: String,
    session: Session,
    run: Run,
    tables: RefCell<HashMap<PathBuf, Rc<Tables>>>,
}

struct State {
    /// uri -> text, in open order
    docs: Vec<(String, String)>,
    /// path -> text (open buffers override the disk)
    overlay: HashMap<PathBuf, String>,
    analyses: HashMap<String, Rc<Analysis>>,
    /// the last analysis of a document that parsed (completion while typing)
    last_good: HashMap<String, Rc<Analysis>>,
    /// `decl.inputs`: input name -> document file
    inputs: Vec<(String, String)>,
    /// `decl.inlayHints.*`
    hint_types: bool,
    hint_parameter_names: bool,
    hint_values: bool,
    hint_units: bool,
    hint_context_variables: bool,
    /// a client that accepts work-done progress sees an analysis as a progress item (03_lsp.md §14)
    progress_supported: bool,
    progress_seq: usize,
}
impl Default for State {
    fn default() -> Self {
        State {
            docs: vec![],
            overlay: HashMap::new(),
            analyses: HashMap::new(),
            last_good: HashMap::new(),
            inputs: vec![],
            hint_types: true,
            hint_parameter_names: true,
            hint_values: false,
            hint_units: true,
            hint_context_variables: false,
            progress_supported: false,
            progress_seq: 0,
        }
    }
}
impl State {
    fn set(&mut self, uri: &str, text: String) {
        if let Some(d) = self.docs.iter_mut().find(|(u, _)| u == uri) {
            d.1 = text.clone();
        } else {
            self.docs.push((uri.to_string(), text.clone()));
        }
        self.overlay.insert(path_of(uri), text);
    }
    fn text(&self, uri: &str) -> Option<&String> {
        self.docs.iter().find(|(u, _)| u == uri).map(|(_, t)| t)
    }
    fn analysis_of(&mut self, uri: &str) -> Option<Rc<Analysis>> {
        let text = self.text(uri)?.clone();
        if let Some(a) = self.analyses.get(uri) {
            if a.text == text {
                return Some(a.clone());
            }
        }
        let path = path_of(uri);
        if !parse_source(&text).errors.is_empty() {
            return None;
        }
        let session = Session::with_overlay(Some(&path.to_string_lossy()), Some(&self.overlay));
        let title = format!(
            "Decl: evaluating {}",
            path.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default()
        );
        let token = if self.progress_supported {
            self.progress_seq += 1;
            let token = format!("decl-{}", self.progress_seq);
            // the request's id is the sequence number: an integer, which every client accepts (Neovim rejects a string)
            send(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"window/workDoneProgress/create\",\"params\":{{\"token\":{}}}}}", self.progress_seq, json_str(&token)));
            notify(
                "$/progress",
                J::obj(vec![
                    ("token", J::s(token.clone())),
                    (
                        "value",
                        J::obj(vec![
                            ("kind", J::s("begin")),
                            ("title", J::s(title)),
                            ("cancellable", J::Bool(false)),
                        ]),
                    ),
                ]),
            );
            Some(token)
        } else {
            None
        };
        let run = session.run(Mode::Full);
        if let Some(token) = token {
            notify(
                "$/progress",
                J::obj(vec![
                    ("token", J::s(token)),
                    ("value", J::obj(vec![("kind", J::s("end"))])),
                ]),
            );
        }
        let a = Rc::new(Analysis {
            text,
            session,
            run,
            tables: RefCell::new(HashMap::new()),
        });
        self.analyses.insert(uri.to_string(), a.clone());
        self.last_good.insert(uri.to_string(), a.clone());
        Some(a)
    }
}
fn tables_of(a: &Analysis, m: &Rc<Module>) -> Rc<Tables> {
    if let Some(t) = a.tables.borrow().get(&m.path) {
        return t.clone();
    }
    let types: Rc<RefCell<HashMap<usize, Ty>>> = Rc::new(RefCell::new(HashMap::new()));
    let res: Rc<RefCell<HashMap<usize, Option<Target>>>> = Rc::new(RefCell::new(HashMap::new()));
    let hooks = CheckHooks {
        record: Some({
            let types = types.clone();
            Rc::new(move |e: &Rc<Expr>, ty: &Ty| {
                types.borrow_mut().insert(key_of(e), ty.clone());
            })
        }),
        resolve_hook: Some({
            let res = res.clone();
            Rc::new(move |e: &Rc<Expr>, target: Option<Target>| {
                res.borrow_mut().insert(key_of(e), target);
            })
        }),
    };
    check_module(&m.decls, Some(m.env.clone()), Some(&hooks));
    let t = Rc::new(Tables {
        types: types.borrow().clone(),
        res: res.borrow().clone(),
    });
    a.tables.borrow_mut().insert(m.path.clone(), t.clone());
    t
}
fn module_of(a: &Analysis, path: &Path) -> Option<Rc<Module>> {
    a.run.modules.iter().find(|m| m.path == path).cloned()
}
fn text_of(st: &State, m: &Module) -> String {
    st.overlay
        .get(&m.path)
        .cloned()
        .unwrap_or_else(|| read_text(&m.path))
}
fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

// ---------------- diagnostics ----------------
fn anchor_for(src: &str, message: &str) -> Loc {
    let names = Regex::new(r"[A-Za-z_][A-Za-z0-9_.]*").unwrap();
    let lines: Vec<&str> = src.split('\n').collect();
    for m in names.find_iter(message) {
        let n = m.as_str();
        if [
            "error", "in", "the", "a", "is", "not", "std", "module", "import", "type", "name",
        ]
        .contains(&n)
        {
            continue;
        }
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(n))).unwrap();
        for (i, line) in lines.iter().enumerate() {
            if let Some(mm) = re.find(line) {
                let a = u16_col(line, mm.start());
                return Loc {
                    sl: i,
                    sc: a,
                    el: i,
                    ec: a + u16len(n),
                };
            }
        }
    }
    Loc {
        sl: 0,
        sc: 0,
        el: 0,
        ec: u16len(lines.first().copied().unwrap_or("")).max(1),
    }
}
// the source position of a document path: the literal the path leads to
// in the root's declaration, or the deepest literal on the way
fn loc_of_path(decls: &[Decl], segs: &[Seg]) -> Option<Loc> {
    let root = seg_text(segs.first()?);
    let decl = decls.iter().find(|d| matches!(&d.body, DeclBody::Output { name, .. } | DeclBody::Input { name, .. } if *name == root))?;
    let mut best = decl.loc?;
    let mut e: Option<Rc<Expr>> = match &decl.body {
        DeclBody::Output { expr, .. } => Some(expr.clone()),
        DeclBody::Input { fallback, .. } => fallback.clone(),
        _ => None,
    };
    for s in &segs[1..] {
        let Some(mut cur) = e.clone() else { break };
        if let Expr::Paren(x) = &*cur {
            cur = x.clone();
        }
        let next: Option<Rc<Expr>> = match (&*cur, s) {
            (Expr::Obj(entries), Seg::Name(k)) | (Expr::Obj(entries), Seg::Key(k)) => entries
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.clone()),
            (Expr::Arr(items), Seg::Idx(i)) => items.get(*i).map(|(_, v)| v.clone()),
            (Expr::With { base, .. }, _) => {
                e = Some(base.clone());
                continue;
            }
            _ => None,
        };
        let Some(next) = next else { break };
        if let Some(l) = expr_loc(&next) {
            best = l;
        }
        e = Some(next);
    }
    Some(best)
}
fn severity_of(s: &str) -> i64 {
    match s {
        "error" => 1,
        "warning" => 2,
        _ => 3,
    }
}
fn diag_json(loc: Loc, d: &Diag) -> J {
    let mut item = vec![
        ("range", range_json(loc)),
        ("severity", J::Num(severity_of(&d.severity))),
        ("source", J::s("decl")),
    ];
    if d.code.is_some() || d.id.is_some() {
        item.push((
            "code",
            J::s(d.id.clone().or_else(|| d.code.clone()).unwrap_or_default()),
        ));
    }
    item.push((
        "message",
        J::s(if d.path.is_empty() {
            d.message.clone()
        } else {
            format!("{} (at {})", d.message, d.path)
        }),
    ));
    J::obj(item)
}

fn analyze(st: &mut State, uri: &str) {
    let src = st.text(uri).cloned().unwrap_or_default();
    let path = path_of(uri);
    let mut out: Vec<J> = vec![];
    let parsed = parse_source(&src);
    if !parsed.errors.is_empty() {
        for (row, col) in &parsed.errors {
            out.push(J::obj(vec![
                (
                    "range",
                    range_json(Loc {
                        sl: *row,
                        sc: *col,
                        el: *row,
                        ec: col + 1,
                    }),
                ),
                ("severity", J::Num(1)),
                ("source", J::s("decl")),
                ("code", J::s("E2001")),
                ("message", J::s("syntax error")),
            ]));
        }
    } else if let Some(a) = st.analysis_of(uri) {
        let r = &a.run;
        for d in &r.load_diags {
            // a loading problem is anchored to the import it concerns when one is named
            let imp = parsed.decls.iter().find(|x| match &x.body {
                DeclBody::Import { from, .. } | DeclBody::ReExport { from, .. } => {
                    x.loc.is_some()
                        && d.message.contains(
                            from.strip_prefix("./")
                                .unwrap_or(from)
                                .strip_suffix(".decl")
                                .unwrap_or(from.strip_prefix("./").unwrap_or(from)),
                        )
                }
                _ => false,
            });
            out.push(diag_json(
                imp.and_then(|x| x.loc)
                    .unwrap_or_else(|| anchor_for(&src, &d.message)),
                d,
            ));
        }
        for (file, d) in &r.checks {
            // a borrowed Path against the PathBuf: no allocation, and std's oldest comparison impl
            if std::path::Path::new(file) != path {
                continue;
            }
            out.push(diag_json(
                d.loc.unwrap_or_else(|| anchor_for(&src, &d.message)),
                d,
            ));
        }
        for d in &r.diags {
            if d.severity == "information" {
                continue;
            }
            let segs = if d.path.is_empty() {
                None
            } else {
                parse_path(&d.path, "").ok()
            };
            let Some(loc) = segs.and_then(|s| loc_of_path(&parsed.decls, &s)) else {
                continue;
            }; // a root declared elsewhere: its own module's business
            out.push(diag_json(loc, d));
        }
    }
    notify(
        "textDocument/publishDiagnostics",
        J::obj(vec![("uri", J::s(uri)), ("diagnostics", J::Arr(out))]),
    );
}

// ---------------- positions -> nodes ----------------
#[derive(Clone)]
enum NodeRef<'a> {
    Decl(&'a Decl),
    Member(&'a MemberAst),
    Type(&'a TypeAst),
    Expr(&'a Rc<Expr>),
}
impl<'a> NodeRef<'a> {
    fn loc(&self) -> Option<Loc> {
        match self {
            NodeRef::Decl(d) => d.loc,
            NodeRef::Member(m) => m.loc(),
            NodeRef::Type(t) => t.loc(),
            NodeRef::Expr(e) => expr_loc(e),
        }
    }
}
struct Hit<'a> {
    node: NodeRef<'a>,
    loc: Loc,
    parents: Vec<NodeRef<'a>>,
}
struct Finder<'a> {
    pos: Pos,
    best: Option<Hit<'a>>,
}
impl<'a> Finder<'a> {
    // the reference visits object values in key order and keeps the
    // innermost node containing the position (a later equal span wins)
    fn enter(&mut self, node: NodeRef<'a>, parents: &[NodeRef<'a>]) -> Vec<NodeRef<'a>> {
        let own = node.loc().filter(|l| contains(*l, self.pos));
        if let Some(l) = own {
            if self
                .best
                .as_ref()
                .map(|b| span(l) <= span(b.loc))
                .unwrap_or(true)
            {
                self.best = Some(Hit {
                    node: node.clone(),
                    loc: l,
                    parents: parents.to_vec(),
                });
            }
            let mut p = parents.to_vec();
            p.push(node);
            p
        } else {
            parents.to_vec()
        }
    }
    fn decls(&mut self, decls: &'a [Decl]) {
        for d in decls {
            self.decl(d, &[]);
        }
    }
    fn decl(&mut self, d: &'a Decl, parents: &[NodeRef<'a>]) {
        let p = self.enter(NodeRef::Decl(d), parents);
        match &d.body {
            DeclBody::Type {
                params, ty, tail, ..
            } => {
                for pr in params {
                    if let Some(t) = &pr.ty {
                        self.ty(t, &p);
                    }
                }
                self.ty(ty, &p);
                if let Some(t) = tail {
                    self.tail(t, &p);
                }
            }
            DeclBody::Const { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t, &p);
                }
                self.expr(expr, &p);
            }
            DeclBody::Func {
                params, ret, body, ..
            } => {
                for pr in params {
                    if let Some(t) = &pr.ty {
                        self.ty(t, &p);
                    }
                }
                if let Some(t) = ret {
                    self.ty(t, &p);
                }
                self.expr(body, &p);
            }
            DeclBody::Output { ty, expr, .. } => {
                self.ty(ty, &p);
                self.expr(expr, &p);
            }
            DeclBody::Input { ty, fallback, .. } => {
                self.ty(ty, &p);
                if let Some(f) = fallback {
                    self.expr(f, &p);
                }
            }
            DeclBody::Diagnostic {
                params, template, ..
            } => {
                for pr in params {
                    if let Some(t) = &pr.ty {
                        self.ty(t, &p);
                    }
                }
                self.template(template, &p);
            }
            DeclBody::Unit {
                factor: Some(f), ..
            } => {
                self.expr(f, &p);
            }
            DeclBody::Unit { factor: None, .. } => {}
            DeclBody::Dimension { .. } | DeclBody::Import { .. } | DeclBody::ReExport { .. } => {}
        }
    }
    fn tail(&mut self, t: &'a Tail, p: &Vec<NodeRef<'a>>) {
        match t {
            Tail::Inline { template, .. } => self.template(template, p),
            Tail::Ref { args, .. } => {
                for a in args {
                    self.expr(a, p);
                }
            }
        }
    }
    fn template(&mut self, parts: &'a [TPart], p: &Vec<NodeRef<'a>>) {
        for part in parts {
            if let TPart::Expr(x) = part {
                self.expr(x, p);
            }
        }
    }
    fn ty(&mut self, t: &'a TypeAst, parents: &[NodeRef<'a>]) {
        let p = self.enter(NodeRef::Type(t), parents);
        match t {
            TypeAst::Record { members, .. } => {
                for m in members {
                    self.member(m, &p);
                }
            }
            TypeAst::Map { key, val, .. } => {
                self.ty(key, &p);
                self.ty(val, &p);
            }
            TypeAst::Array { elem, .. } => self.ty(elem, &p),
            TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
                for a in arms {
                    self.ty(a, &p);
                }
            }
            TypeAst::Func { params, ret, .. } => {
                for a in params {
                    self.ty(a, &p);
                }
                self.ty(ret, &p);
            }
            TypeAst::Named {
                args, preds, ext, ..
            } => {
                for a in args {
                    self.ty(a, &p);
                }
                for x in preds.iter().flatten() {
                    self.expr(x, &p);
                }
                if let Some(x) = ext {
                    self.ty(x, &p);
                }
            }
            TypeAst::Prim { .. }
            | TypeAst::Lit { .. }
            | TypeAst::Range { .. }
            | TypeAst::Pattern { .. } => {}
        }
    }
    fn member(&mut self, m: &'a MemberAst, parents: &[NodeRef<'a>]) {
        let p = self.enter(NodeRef::Member(m), parents);
        match m {
            MemberAst::Value { ty, dflt, .. } => {
                self.ty(ty, &p);
                if let Some(d) = dflt {
                    self.expr(d, &p);
                }
            }
            MemberAst::Derived { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t, &p);
                }
                self.expr(expr, &p);
            }
            MemberAst::Context { ty, .. } => self.ty(ty, &p),
            MemberAst::Assert { cond, tail, .. } => {
                self.expr(cond, &p);
                if let Some(t) = tail {
                    self.tail(t, &p);
                }
            }
            MemberAst::When { cond, body, .. } => {
                self.expr(cond, &p);
                for b in body {
                    self.member(b, &p);
                }
            }
        }
    }
    fn expr(&mut self, e: &'a Rc<Expr>, parents: &[NodeRef<'a>]) {
        let p = self.enter(NodeRef::Expr(e), parents);
        match &**e {
            Expr::Template(parts) => self.template(parts, &p),
            Expr::Obj(entries) => {
                for (_, v) in entries {
                    self.expr(v, &p);
                }
            }
            Expr::Arr(items) => {
                for (_, v) in items {
                    self.expr(v, &p);
                }
            }
            Expr::Comp { head, clauses } => {
                self.expr(head, &p);
                self.clauses(clauses, &p);
            }
            Expr::MapComp { key, val, clauses } => {
                self.expr(key, &p);
                self.expr(val, &p);
                self.clauses(clauses, &p);
            }
            Expr::Bin { l, r, .. } => {
                self.expr(l, &p);
                self.expr(r, &p);
            }
            Expr::Un { x, .. } | Expr::Paren(x) => self.expr(x, &p),
            Expr::If { c, t, f } => {
                self.expr(c, &p);
                self.expr(t, &p);
                self.expr(f, &p);
            }
            Expr::Lambda { body, .. } => self.expr(body, &p),
            Expr::Call { fun, args } => {
                self.expr(fun, &p);
                for a in args {
                    self.expr(a, &p);
                }
            }
            Expr::Member { x, .. } => self.expr(x, &p),
            Expr::Index { x, i } => {
                self.expr(x, &p);
                self.expr(i, &p);
            }
            Expr::With { base, patch } => {
                self.expr(base, &p);
                self.expr(patch, &p);
            }
            Expr::Match { subject, arms } => {
                self.expr(subject, &p);
                for a in arms {
                    if let Some(t) = &a.ty {
                        self.ty(t, &p);
                    }
                    self.expr(&a.body, &p);
                }
            }
            Expr::Lit(_)
            | Expr::UnitLit { .. }
            | Expr::Name(_)
            | Expr::Ctx(_)
            | Expr::Referrers { .. }
            | Expr::Pattern(_) => {}
        }
    }
    fn clauses(&mut self, clauses: &'a [ForClause], p: &Vec<NodeRef<'a>>) {
        for c in clauses {
            self.expr(&c.iter, p);
            for f in &c.filters {
                self.expr(f, p);
            }
        }
    }
}
fn node_at<'a>(decls: &'a [Decl], pos: Pos) -> Option<Hit<'a>> {
    let mut f = Finder { pos, best: None };
    f.decls(decls);
    f.best
}

fn decl_kind(d: &Decl) -> &'static str {
    match &d.body {
        DeclBody::Type { .. } => "type",
        DeclBody::Const { .. } => "const",
        DeclBody::Func { .. } => "func",
        DeclBody::Output { .. } => "output",
        DeclBody::Input { .. } => "input",
        DeclBody::Diagnostic { .. } => "diagnostic",
        DeclBody::Dimension { .. } => "dimension",
        DeclBody::Unit { .. } => "unit",
        DeclBody::Import { .. } => "import",
        DeclBody::ReExport { .. } => "re_export",
    }
}

// the range of a declaration's name token (the declaration site)
fn name_range(text: &str, decl: &Decl, name: &str) -> Loc {
    let loc = decl.loc.unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    let re = Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap();
    let mut i = loc.sl;
    while i <= loc.el && i < lines.len() {
        let from = if i == loc.sl {
            byte_col(lines[i], loc.sc)
        } else {
            0
        };
        if let Some(m) = re.find_at(lines[i], from) {
            let a = u16_col(lines[i], m.start());
            return Loc {
                sl: i,
                sc: a,
                el: i,
                ec: a + u16len(name),
            };
        }
        i += 1;
    }
    loc
}
fn member_range(text: &str, member: &MemberAst, name: &str) -> Loc {
    let loc = member.loc().unwrap();
    let line = text.split('\n').nth(loc.sl).unwrap_or("");
    match find16(line, name, loc.sc) {
        Some(i) => Loc {
            sl: loc.sl,
            sc: i,
            el: loc.sl,
            ec: i + u16len(name),
        },
        None => loc,
    }
}

// ---------------- what is under the cursor ----------------
#[derive(Clone)]
struct Site {
    kind: String,
    module: Rc<Module>,
    decl: Option<usize>, // the declaration's identity (its address)
    decl_loc: Option<Loc>,
    member_loc: Option<Loc>,
    range: Loc,
    name: String,
}
fn decl_id(d: &Decl) -> usize {
    d as *const Decl as usize
}

// the declaration a target denotes, as a site in its module
fn site_of_target(st: &State, a: &Analysis, t: Option<&Target>) -> Option<Site> {
    let t = t?;
    let env = t.env.as_ref()?;
    let m = a
        .run
        .modules
        .iter()
        .find(|x| Rc::ptr_eq(&x.env, env))?
        .clone();
    let text = text_of(st, &m);
    let decl = m.decls.iter().find(|d| {
        d.name() == Some(t.name.as_str())
            && d.loc.is_some()
            && !matches!(d.body, DeclBody::Import { .. })
    })?;
    Some(Site {
        kind: decl_kind(decl).to_string(),
        module: m.clone(),
        decl: Some(decl_id(decl)),
        decl_loc: decl.loc,
        member_loc: None,
        range: name_range(&text, decl, &t.name),
        name: t.name.clone(),
    })
}
fn rec_name(rt: Option<&RT>) -> Option<String> {
    let rt = rt?;
    match &rt.k {
        RTk::Rec(_) => rt.name.borrow().clone(),
        RTk::Pred { base, .. } => base.name.borrow().clone(),
        _ => None,
    }
}
fn record_members(ty: &TypeAst) -> &[MemberAst] {
    match ty {
        TypeAst::Record { members, .. } => members,
        TypeAst::Named { ext: Some(x), .. } => match &**x {
            TypeAst::Record { members, .. } => members,
            _ => &[],
        },
        _ => &[],
    }
}
fn member_site(
    st: &State,
    a: &Analysis,
    m: &Rc<Module>,
    rt: Option<&RT>,
    member: &str,
) -> Option<Site> {
    // the member's declaring type, extension chains followed (§4)
    let mut seen: Vec<String> = vec![];
    let mut type_name = rec_name(rt);
    while let Some(tn) = type_name.clone() {
        if seen.contains(&tn) {
            break;
        }
        seen.push(tn.clone());
        let target = resolve_in(&m.env, &tn);
        let site = site_of_target(st, a, target.as_ref())?;
        let sm = site.module.clone();
        let decl = sm
            .decls
            .iter()
            .find(|d| decl_id(d) == site.decl.unwrap_or(0))?;
        let DeclBody::Type { ty, .. } = &decl.body else {
            return None;
        };
        let members = record_members(ty);
        if let Some(mem) = members.iter().find(|x| x.name() == Some(member)) {
            if mem.loc().is_some() {
                return Some(Site {
                    kind: "member".into(),
                    module: sm.clone(),
                    decl: Some(decl_id(decl)),
                    decl_loc: decl.loc,
                    member_loc: mem.loc(),
                    range: member_range(&text_of(st, &sm), mem, member),
                    name: member.to_string(),
                });
            }
        }
        type_name = match ty {
            TypeAst::Named { name, .. } => Some(name.clone()),
            _ => None,
        };
    }
    None
}

struct SiteAt<'a> {
    site: Option<Site>,
    ty: Option<Ty>,
    hit: Option<Hit<'a>>,
    module: Rc<Module>,
}
fn ns_export_site(st: &State, a: &Analysis, m: &Module, ns: &str, name: &str) -> Option<Site> {
    let nss = m.env.namespaces.borrow();
    let (_, exports) = nss.get(ns)?;
    let ex = exports.borrow().get(name).cloned()?;
    site_of_target(st, a, resolve_in(&ex.env, &ex.name).as_ref())
}
fn site_at<'a>(st: &State, a: &'a Analysis, uri: &str, pos: Pos) -> Option<SiteAt<'a>> {
    let m = module_of(a, &path_of(uri))?;
    let mi = a.run.modules.iter().position(|x| Rc::ptr_eq(x, &m))?;
    let decls: &'a [Decl] = &a.run.modules[mi].decls;
    let Some(hit) = node_at(decls, pos) else {
        return Some(SiteAt {
            site: None,
            ty: None,
            hit: None,
            module: m,
        });
    };
    let t = tables_of(a, &m);
    match &hit.node {
        NodeRef::Expr(e) => {
            let ty = t.types.get(&key_of(e)).cloned();
            match &***e {
                Expr::Name(n) => {
                    let target = match t.res.get(&key_of(e)) {
                        Some(r) => r.clone(),
                        None => resolve_in(&m.env, n),
                    };
                    Some(SiteAt {
                        site: site_of_target(st, a, target.as_ref()),
                        ty,
                        hit: Some(hit),
                        module: m,
                    })
                }
                Expr::Member { x, name, .. } => {
                    if let Expr::Name(xn) = &**x {
                        if m.env.namespaces.borrow().contains_key(xn) {
                            let site = ns_export_site(st, a, &m, xn, name);
                            return Some(SiteAt {
                                site,
                                ty,
                                hit: Some(hit),
                                module: m,
                            });
                        }
                    }
                    let xt = t.types.get(&key_of(x)).cloned();
                    let site =
                        member_site(st, a, &m, xt.as_ref().and_then(|t| t.rt.as_ref()), name);
                    Some(SiteAt {
                        site,
                        ty,
                        hit: Some(hit),
                        module: m,
                    })
                }
                _ => Some(SiteAt {
                    site: None,
                    ty,
                    hit: Some(hit),
                    module: m,
                }),
            }
        }
        NodeRef::Type(TypeAst::Named { name, .. }) => {
            let mut parts = name.splitn(2, '.');
            let head = parts.next().unwrap_or("");
            let tail = parts.next();
            let target = match tail {
                Some(tail) if m.env.namespaces.borrow().contains_key(head) => {
                    let site = ns_export_site(st, a, &m, head, tail);
                    return Some(SiteAt {
                        site,
                        ty: None,
                        hit: Some(hit),
                        module: m,
                    });
                }
                _ => resolve_in(&m.env, head),
            };
            Some(SiteAt {
                site: site_of_target(st, a, target.as_ref()),
                ty: None,
                hit: Some(hit),
                module: m,
            })
        }
        NodeRef::Member(mem) if mem.name().is_some() => {
            // the member's own declaration
            let name = mem.name().unwrap().to_string();
            let decl = hit.parents.iter().find_map(|p| {
                if let NodeRef::Decl(d) = p {
                    Some(*d)
                } else {
                    None
                }
            });
            let site = decl.map(|d| Site {
                kind: "member".into(),
                module: m.clone(),
                decl: Some(decl_id(d)),
                decl_loc: d.loc,
                member_loc: mem.loc(),
                range: member_range(&text_of(st, &m), mem, &name),
                name,
            });
            Some(SiteAt {
                site,
                ty: None,
                hit: Some(hit),
                module: m,
            })
        }
        NodeRef::Decl(d) if d.name().is_some() => {
            let name = d.name().unwrap().to_string();
            let r = name_range(&text_of(st, &m), d, &name);
            if contains(r, pos) {
                let site = Site {
                    kind: decl_kind(d).to_string(),
                    module: m.clone(),
                    decl: Some(decl_id(d)),
                    decl_loc: d.loc,
                    member_loc: None,
                    range: r,
                    name,
                };
                return Some(SiteAt {
                    site: Some(site),
                    ty: None,
                    hit: Some(hit),
                    module: m,
                });
            }
            Some(SiteAt {
                site: None,
                ty: None,
                hit: Some(hit),
                module: m,
            })
        }
        _ => Some(SiteAt {
            site: None,
            ty: None,
            hit: Some(hit),
            module: m,
        }),
    }
}

// ---------------- hover ----------------
fn decl_text(st: &State, site: &Site) -> Vec<String> {
    let text = text_of(st, &site.module);
    let lines: Vec<&str> = text.split('\n').collect();
    let doc_re = Regex::new(r"^\s*///").unwrap();
    let doc_above = |sl: usize| -> Vec<String> {
        let mut from = sl;
        let mut out: Vec<String> = vec![];
        while from > 0 && doc_re.is_match(lines[from - 1]) {
            from -= 1;
            out.insert(0, lines[from].trim().to_string());
        }
        out
    };
    if let Some(l) = site.member_loc {
        let mut out = doc_above(l.sl);
        let body: Vec<String> = if l.sl == l.el {
            vec![slice16(lines.get(l.sl).copied().unwrap_or(""), l.sc, l.ec).to_string()]
        } else {
            let mut b = vec![slice16(lines[l.sl], l.sc, u16len(lines[l.sl])).to_string()];
            b.extend(lines[l.sl + 1..l.el].iter().map(|x| x.to_string()));
            b.push(slice16(lines[l.el], 0, l.ec).to_string());
            b
        };
        out.extend(
            body.iter()
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty()),
        );
        return out;
    }
    let l = site.decl_loc.unwrap();
    let mut out = doc_above(l.sl);
    let body: Vec<String> = lines[l.sl..=l.el.min(lines.len() - 1)]
        .iter()
        .map(|x| x.to_string())
        .collect();
    if body.len() > 12 {
        out.extend(body[..11].iter().cloned());
        out.push("    …".into());
        out.push(body[body.len() - 1].clone());
    } else {
        out.extend(body);
    }
    out
}
fn hover(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else {
        return J::Null;
    };
    let Some(s) = site_at(st, &a, uri, pos) else {
        return J::Null;
    };
    let mut parts: Vec<String> = vec![];
    if let Some(site) = &s.site {
        let lines = decl_text(st, site);
        let doc: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("///"))
            .map(|l| {
                l.strip_prefix("///")
                    .map(|r| r.strip_prefix(' ').unwrap_or(r))
                    .unwrap_or(l)
                    .to_string()
            })
            .collect();
        let code: Vec<&String> = lines.iter().filter(|l| !l.starts_with("///")).collect();
        if !doc.is_empty() {
            parts.push(doc.join("\n"));
        }
        parts.push(format!(
            "```decl\n{}\n```",
            code.iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(ty) = &s.ty {
        parts.push(format!(
            "`{}{}`",
            type_text(ty.rt.as_ref()),
            if ty.abs { "?" } else { "" }
        ));
    }
    if parts.is_empty() {
        return J::Null;
    }
    let contents = J::obj(vec![
        ("kind", J::s("markdown")),
        ("value", J::s(parts.join("\n\n"))),
    ]);
    match s.hit.as_ref().map(|h| h.loc) {
        Some(l) => J::obj(vec![("contents", contents), ("range", range_json(l))]),
        None => J::obj(vec![("contents", contents)]),
    }
}

// ---------------- navigation ----------------
fn location(m: &Module, loc: Loc) -> J {
    J::obj(vec![
        ("uri", J::s(uri_of(&m.path))),
        ("range", range_json(loc)),
    ])
}
fn definition(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else {
        return J::Null;
    };
    match site_at(st, &a, uri, pos).and_then(|s| s.site) {
        Some(site) => location(&site.module, site.range),
        None => J::Null,
    }
}
// the named types a declared type spells (`T`, `T[]`, `A | B`, `T?`, `ref<T>`, `map<K, V>`): whatever they resolve to
fn named_types_of(ast: &TypeAst, out: &mut Vec<String>) {
    match ast {
        TypeAst::Named { name, args, .. } => {
            out.push(name.clone());
            for arg in args {
                named_types_of(arg, out);
            }
        }
        TypeAst::Array { elem, .. } => named_types_of(elem, out),
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
            for arm in arms {
                named_types_of(arm, out);
            }
        }
        TypeAst::Map { val, .. } => named_types_of(val, out),
        _ => {}
    }
}
// the same over a resolved type: record names, through refs, arrays, maps, unions
fn named_types_of_rt(rt: Option<&RT>, out: &mut Vec<String>) {
    let Some(rt) = rt else { return };
    match &rt.k {
        RTk::Rec(_) => {
            if let Some(n) = rt.name.borrow().clone() {
                if !n.starts_with('{') {
                    out.push(n);
                }
            }
        }
        RTk::Pred { base, .. } => named_types_of_rt(Some(base), out),
        RTk::Ref(target) => named_types_of_rt(Some(target), out),
        RTk::Arr { elem, .. } => named_types_of_rt(Some(elem), out),
        RTk::Map { val, .. } => named_types_of_rt(Some(val), out),
        RTk::Union(arms) => {
            for arm in arms {
                named_types_of_rt(Some(arm), out);
            }
        }
        _ => {}
    }
}
// the type a member declares, found again by name in its declaring type's body
fn member_type_ast(m: &Module, site: &Site) -> Option<TypeAst> {
    if site.kind != "member" {
        return None;
    }
    let d = decl_by_id(m, site.decl)?;
    record_body_of(d).map(record_members).and_then(|members| {
        members
            .iter()
            .find(|x| x.name() == Some(site.name.as_str()))
            .and_then(|x| match x {
                MemberAst::Value { ty, .. } | MemberAst::Context { ty, .. } => Some(ty.clone()),
                MemberAst::Derived { ty, .. } => ty.clone(),
                _ => None,
            })
    })
}
fn type_definition(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else {
        return J::Null;
    };
    let Some(s) = site_at(st, &a, uri, pos) else {
        return J::Null;
    };
    // the declared type first (a member, an output or input, a constant's
    // annotation): the named types it spells, whatever they resolve to —
    // an alias of a literal union has a declaration too; else the
    // expression's inferred type
    let mut names: Vec<String> = vec![];
    let mut env = s.module.env.clone();
    if let Some(site) = &s.site {
        let sm = site.module.clone();
        let d = decl_by_id(&sm, site.decl);
        let ast: Option<TypeAst> = member_type_ast(&sm, site).or_else(|| {
            d.and_then(|d| match &d.body {
                DeclBody::Output { ty, .. } | DeclBody::Input { ty, .. } => Some(ty.clone()),
                DeclBody::Const { ty: Some(ty), .. } => Some(ty.clone()),
                _ => None,
            })
        });
        if let Some(ast) = ast {
            named_types_of(&ast, &mut names);
            env = sm.env.clone();
        } else if let Some(DeclBody::Const { expr, .. }) = d.map(|d| &d.body) {
            let rt = tables_of(&a, &sm)
                .types
                .get(&key_of(expr))
                .and_then(|x| x.rt.clone());
            named_types_of_rt(rt.as_ref(), &mut names);
        }
    }
    if names.is_empty() {
        if let Some(NodeRef::Expr(e)) = s.hit.as_ref().map(|h| h.node.clone()) {
            if let Expr::Member { x, name, .. } = &**e {
                // a member access: the member's declared type, where it is declared
                let xt = tables_of(&a, &s.module)
                    .types
                    .get(&key_of(x))
                    .and_then(|t| t.rt.clone());
                if let Some(ms) = member_site(st, &a, &s.module, xt.as_ref(), name) {
                    if let Some(ast) = member_type_ast(&ms.module, &ms) {
                        named_types_of(&ast, &mut names);
                        env = ms.module.env.clone();
                    }
                }
            }
        }
    }
    if names.is_empty() {
        named_types_of_rt(s.ty.as_ref().and_then(|t| t.rt.as_ref()), &mut names);
    }
    let mut seen: Vec<(PathBuf, usize, usize)> = vec![];
    let mut locs: Vec<J> = vec![];
    for n in names {
        let (head, tail) = match n.split_once('.') {
            Some((h, t)) => (h.to_string(), Some(t.to_string())),
            None => (n.clone(), None),
        };
        let target = match &tail {
            Some(t) if env.namespaces.borrow().contains_key(&head) => {
                let ex = env
                    .namespaces
                    .borrow()
                    .get(&head)
                    .and_then(|(_, exports)| exports.borrow().get(t).cloned());
                ex.and_then(|ex| resolve_in(&ex.env, &ex.name))
            }
            _ => resolve_in(&env, &head),
        };
        let Some(site) = site_of_target(st, &a, target.as_ref()) else {
            continue;
        };
        let key = (site.module.path.clone(), site.range.sl, site.range.sc);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        locs.push(location(&site.module, site.range));
    }
    match locs.len() {
        0 => J::Null,
        1 => locs.pop().unwrap(),
        _ => J::Arr(locs),
    }
}
fn same(x: Option<&Site>, target: &Site) -> bool {
    match x {
        Some(x) => {
            Rc::ptr_eq(&x.module, &target.module)
                && x.name == target.name
                && x.kind == target.kind
                && (x.kind != "member" || x.decl == target.decl)
        }
        None => false,
    }
}
fn member_token_loc(text: &str, e: &Rc<Expr>, name: &str) -> Loc {
    let l = expr_loc(e).unwrap();
    let line = text.split('\n').nth(l.el).unwrap_or("");
    match rfind16(line, name, l.ec) {
        Some(i) => Loc {
            sl: l.el,
            sc: i,
            el: l.el,
            ec: i + u16len(name),
        },
        None => l,
    }
}
fn type_name_loc(l: Loc, offset: usize, name: &str) -> Loc {
    Loc {
        sl: l.sl,
        sc: l.sc + offset,
        el: l.sl,
        ec: l.sc + offset + u16len(name),
    }
}
fn import_item_loc(text: &str, d: &Decl, name: &str) -> Loc {
    let l = d.loc.unwrap();
    let line = text.split('\n').nth(l.sl).unwrap_or("");
    match find16(line, name, l.sc) {
        Some(i) => Loc {
            sl: l.sl,
            sc: i,
            el: l.sl,
            ec: i + u16len(name),
        },
        None => l,
    }
}
// every reference to a site across the universe: name and member nodes
// that resolve to the same declaration, plus the declaration itself
fn references(
    st: &mut State,
    uri: &str,
    pos: Pos,
    include_declaration: bool,
) -> Vec<(Rc<Module>, Loc)> {
    let Some(a) = st.analysis_of(uri) else {
        return vec![];
    };
    let Some(target) = site_at(st, &a, uri, pos).and_then(|s| s.site) else {
        return vec![];
    };
    let mut out: Vec<(Rc<Module>, Loc)> = vec![];
    for m in a.run.modules.clone() {
        let t = tables_of(&a, &m);
        let text = text_of(st, &m);
        // every expression node, in the reference's traversal order
        let mut exprs: Vec<Rc<Expr>> = vec![];
        let mut types: Vec<&TypeAst> = vec![];
        for d in &m.decls {
            collect_decl(d, &mut exprs, &mut types);
        }
        for x in &exprs {
            if expr_loc(x).is_none() {
                continue;
            }
            match &**x {
                Expr::Name(n) => {
                    let tg = match t.res.get(&key_of(x)) {
                        Some(r) => r.clone(),
                        None => resolve_in(&m.env, n),
                    };
                    if same(site_of_target(st, &a, tg.as_ref()).as_ref(), &target) {
                        out.push((m.clone(), expr_loc(x).unwrap()));
                    }
                }
                Expr::Member { x: xx, name, .. } => {
                    let site = match &**xx {
                        Expr::Name(xn) if m.env.namespaces.borrow().contains_key(xn) => {
                            ns_export_site(st, &a, &m, xn, name)
                        }
                        _ => member_site(
                            st,
                            &a,
                            &m,
                            t.types.get(&key_of(xx)).and_then(|t| t.rt.as_ref()),
                            name,
                        ),
                    };
                    if same(site.as_ref(), &target) {
                        out.push((m.clone(), member_token_loc(&text, x, name)));
                    }
                }
                _ => {}
            }
        }
        for ty in types {
            let TypeAst::Named {
                name, loc: Some(l), ..
            } = ty
            else {
                continue;
            };
            let mut parts = name.splitn(2, '.');
            let head = parts.next().unwrap_or("");
            let tail = parts.next();
            let site = match tail {
                Some(tail) if m.env.namespaces.borrow().contains_key(head) => {
                    ns_export_site(st, &a, &m, head, tail)
                }
                _ => site_of_target(st, &a, resolve_in(&m.env, head).as_ref()),
            };
            if same(site.as_ref(), &target) {
                out.push((
                    m.clone(),
                    type_name_loc(
                        *l,
                        if tail.is_some() { u16len(head) + 1 } else { 0 },
                        tail.unwrap_or(head),
                    ),
                ));
            }
        }
        // import items naming the declaration
        for d in &m.decls {
            let (names, has_loc) = match &d.body {
                DeclBody::Import {
                    names: Some(names), ..
                } => (names, d.loc.is_some()),
                DeclBody::ReExport { names, .. } => (names, d.loc.is_some()),
                _ => continue,
            };
            if !has_loc {
                continue;
            }
            for it in names {
                let local = it.alias.clone().unwrap_or_else(|| it.name.clone());
                let im = m.env.imports.borrow().get(&local).cloned();
                if let Some(im) = im {
                    if same(
                        site_of_target(st, &a, resolve_in(&im.env, &im.name).as_ref()).as_ref(),
                        &target,
                    ) {
                        out.push((m.clone(), import_item_loc(&text, d, &it.name)));
                    }
                }
            }
        }
    }
    if include_declaration {
        out.insert(0, (target.module.clone(), target.range));
    }
    let mut seen: Vec<(PathBuf, usize, usize)> = vec![];
    let mut kept: Vec<(Rc<Module>, Loc)> = vec![];
    for (m, l) in out {
        let k = (m.path.clone(), l.sl, l.sc);
        if seen.contains(&k) {
            continue;
        }
        seen.push(k);
        kept.push((m, l));
    }
    kept.sort_by(|p, q| {
        let (pp, qp) = (
            p.0.path.to_string_lossy().to_string(),
            q.0.path.to_string_lossy().to_string(),
        );
        pp.cmp(&qp)
            .then(p.1.sl.cmp(&q.1.sl))
            .then(p.1.sc.cmp(&q.1.sc))
    });
    kept
}
// the expression and type nodes of a declaration, in the reference's traversal order
fn collect_decl<'a>(d: &'a Decl, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    let mut ty = |t: &'a TypeAst| collect_type(t, exprs, types);
    match &d.body {
        DeclBody::Type {
            params,
            ty: t,
            tail,
            ..
        } => {
            for p in params {
                if let Some(pt) = &p.ty {
                    collect_type(pt, exprs, types);
                }
            }
            collect_type(t, exprs, types);
            if let Some(tl) = tail {
                collect_tail(tl, exprs, types);
            }
        }
        DeclBody::Const { ty: t, expr, .. } => {
            if let Some(t) = t {
                ty(t);
            }
            collect_expr(expr, exprs, types);
        }
        DeclBody::Func {
            params, ret, body, ..
        } => {
            for p in params {
                if let Some(pt) = &p.ty {
                    collect_type(pt, exprs, types);
                }
            }
            if let Some(r) = ret {
                collect_type(r, exprs, types);
            }
            collect_expr(body, exprs, types);
        }
        DeclBody::Output { ty: t, expr, .. } => {
            collect_type(t, exprs, types);
            collect_expr(expr, exprs, types);
        }
        DeclBody::Input {
            ty: t, fallback, ..
        } => {
            collect_type(t, exprs, types);
            if let Some(f) = fallback {
                collect_expr(f, exprs, types);
            }
        }
        DeclBody::Diagnostic {
            params, template, ..
        } => {
            for p in params {
                if let Some(pt) = &p.ty {
                    collect_type(pt, exprs, types);
                }
            }
            collect_template(template, exprs, types);
        }
        DeclBody::Unit {
            factor: Some(f), ..
        } => {
            collect_expr(f, exprs, types);
        }
        _ => {}
    }
}
fn collect_tail<'a>(t: &'a Tail, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    match t {
        Tail::Inline { template, .. } => collect_template(template, exprs, types),
        Tail::Ref { args, .. } => args.iter().for_each(|a| collect_expr(a, exprs, types)),
    }
}
fn collect_template<'a>(
    parts: &'a [TPart],
    exprs: &mut Vec<Rc<Expr>>,
    types: &mut Vec<&'a TypeAst>,
) {
    for p in parts {
        if let TPart::Expr(x) = p {
            collect_expr(x, exprs, types);
        }
    }
}
fn collect_type<'a>(t: &'a TypeAst, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    types.push(t);
    match t {
        TypeAst::Record { members, .. } => {
            members.iter().for_each(|m| collect_member(m, exprs, types))
        }
        TypeAst::Map { key, val, .. } => {
            collect_type(key, exprs, types);
            collect_type(val, exprs, types);
        }
        TypeAst::Array { elem, .. } => collect_type(elem, exprs, types),
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
            arms.iter().for_each(|a| collect_type(a, exprs, types))
        }
        TypeAst::Func { params, ret, .. } => {
            params.iter().for_each(|a| collect_type(a, exprs, types));
            collect_type(ret, exprs, types);
        }
        TypeAst::Named {
            args, preds, ext, ..
        } => {
            args.iter().for_each(|a| collect_type(a, exprs, types));
            preds
                .iter()
                .flatten()
                .for_each(|x| collect_expr(x, exprs, types));
            if let Some(x) = ext {
                collect_type(x, exprs, types);
            }
        }
        _ => {}
    }
}
fn collect_member<'a>(m: &'a MemberAst, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    match m {
        MemberAst::Value { ty, dflt, .. } => {
            collect_type(ty, exprs, types);
            if let Some(d) = dflt {
                collect_expr(d, exprs, types);
            }
        }
        MemberAst::Derived { ty, expr, .. } => {
            if let Some(t) = ty {
                collect_type(t, exprs, types);
            }
            collect_expr(expr, exprs, types);
        }
        MemberAst::Context { ty, .. } => collect_type(ty, exprs, types),
        MemberAst::Assert { cond, tail, .. } => {
            collect_expr(cond, exprs, types);
            if let Some(t) = tail {
                collect_tail(t, exprs, types);
            }
        }
        MemberAst::When { cond, body, .. } => {
            collect_expr(cond, exprs, types);
            body.iter().for_each(|b| collect_member(b, exprs, types));
        }
    }
}
fn collect_expr<'a>(e: &'a Rc<Expr>, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    exprs.push(e.clone());
    let mut go = |x: &'a Rc<Expr>| collect_expr(x, exprs, types);
    match &**e {
        Expr::Template(parts) => {
            for p in parts {
                if let TPart::Expr(x) = p {
                    go(x);
                }
            }
        }
        Expr::Obj(entries) => entries.iter().for_each(|(_, v)| go(v)),
        Expr::Arr(items) => items.iter().for_each(|(_, v)| go(v)),
        Expr::Comp { head, clauses } => {
            go(head);
            for c in clauses {
                go(&c.iter);
                c.filters.iter().for_each(&mut go);
            }
        }
        Expr::MapComp { key, val, clauses } => {
            go(key);
            go(val);
            for c in clauses {
                go(&c.iter);
                c.filters.iter().for_each(&mut go);
            }
        }
        Expr::Bin { l, r, .. } => {
            go(l);
            go(r);
        }
        Expr::Un { x, .. } | Expr::Paren(x) => go(x),
        Expr::If { c, t, f } => {
            go(c);
            go(t);
            go(f);
        }
        Expr::Lambda { body, .. } => go(body),
        Expr::Call { fun, args } => {
            go(fun);
            args.iter().for_each(&mut go);
        }
        Expr::Member { x, .. } => go(x),
        Expr::Index { x, i } => {
            go(x);
            go(i);
        }
        Expr::With { base, patch } => {
            go(base);
            go(patch);
        }
        Expr::Match { subject, arms } => {
            go(subject);
            for a in arms {
                if let Some(t) = &a.ty {
                    collect_type(t, exprs, types);
                }
                collect_expr(&a.body, exprs, types);
            }
        }
        _ => {}
    }
}

// ---------------- completion ----------------
fn completion(st: &mut State, uri: &str, pos: Pos) -> J {
    let a = st.analysis_of(uri);
    let Some(text) = st.text(uri).cloned() else {
        return J::obj(vec![
            ("isIncomplete", J::Bool(false)),
            ("items", J::Arr(vec![])),
        ]);
    };
    let line = text.split('\n').nth(pos.line).unwrap_or("");
    let prefix = &line[..byte_col(line, pos.character)];
    // while the text does not parse, the scope is the last one that did
    let fresh;
    let session: &Session = match a.as_ref().or_else(|| st.last_good.get(uri)) {
        Some(x) => &x.session,
        None => {
            fresh = Session::with_overlay(Some(&path_of(uri).to_string_lossy()), Some(&st.overlay));
            &fresh
        }
    };
    let items: Vec<J> = session
        .complete(prefix, &[])
        .iter()
        .map(|c| {
            let (label, detail) = match c.find("  ") {
                Some(i) => (&c[..i], Some(&c[i + 2..])),
                None => (c.as_str(), None),
            };
            let kind = match detail {
                Some(d) => {
                    if d.starts_with("derived")
                        || d.starts_with("required")
                        || d.starts_with("optional")
                        || d.starts_with("defaulted")
                    {
                        5
                    } else {
                        6
                    }
                }
                None => {
                    if label
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                    {
                        7
                    } else if label.starts_with('$') {
                        14
                    } else {
                        6
                    }
                }
            };
            let mut item = vec![("label", J::s(label)), ("kind", J::Num(kind))];
            if let Some(d) = detail {
                item.push(("detail", J::s(d)));
            }
            J::obj(item)
        })
        .collect();
    J::obj(vec![
        ("isIncomplete", J::Bool(false)),
        ("items", J::Arr(items)),
    ])
}

// ---------------- symbols, folding, formatting ----------------
fn symbol_kind(d: &Decl) -> Option<i64> {
    Some(match &d.body {
        DeclBody::Type { .. } => 5,
        DeclBody::Const { .. } => 14,
        DeclBody::Func { .. } => 12,
        DeclBody::Output { .. }
        | DeclBody::Input { .. }
        | DeclBody::Dimension { .. }
        | DeclBody::Unit { .. } => 13,
        DeclBody::Diagnostic { .. } => 24,
        _ => return None,
    })
}
fn document_symbols(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let parsed = parse_source(text);
    if !parsed.errors.is_empty() {
        return J::Arr(vec![]);
    }
    let mut out: Vec<J> = vec![];
    for d in &parsed.decls {
        let (Some(loc), Some(name), Some(kind)) = (d.loc, d.name(), symbol_kind(d)) else {
            continue;
        };
        let mut sym = vec![
            ("name", J::s(name)),
            ("kind", J::Num(kind)),
            ("range", range_json(loc)),
            ("selectionRange", range_json(name_range(text, d, name))),
        ];
        if let DeclBody::Type { ty, .. } = &d.body {
            let children: Vec<J> = record_members(ty)
                .iter()
                .filter(|m| m.loc().is_some() && m.name().is_some())
                .map(|m| {
                    let n = m.name().unwrap();
                    let (label, kind) = match m {
                        MemberAst::Assert { .. } => (format!("assert {n}"), 24),
                        MemberAst::Derived { hidden: true, .. } => (format!("{n}$"), 7),
                        _ => (n.to_string(), 7),
                    };
                    J::obj(vec![
                        ("name", J::s(label)),
                        ("kind", J::Num(kind)),
                        ("range", range_json(m.loc().unwrap())),
                        ("selectionRange", range_json(member_range(text, m, n))),
                    ])
                })
                .collect();
            if !children.is_empty() {
                sym.push(("children", J::Arr(children)));
            }
        }
        out.push(J::obj(sym));
    }
    J::Arr(out)
}
fn folding_ranges(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let parsed = parse_source(text);
    if !parsed.errors.is_empty() {
        return J::Arr(vec![]);
    }
    let mut ranges: Vec<(usize, usize)> = vec![];
    for d in &parsed.decls {
        fold_decl(d, &mut ranges);
    }
    let mut seen: Vec<(usize, usize)> = vec![];
    let mut out: Vec<J> = vec![];
    for r in ranges {
        if seen.contains(&r) {
            continue;
        }
        seen.push(r);
        out.push(J::obj(vec![
            ("startLine", J::Num(r.0 as i64)),
            ("endLine", J::Num(r.1 as i64)),
            ("kind", J::s("region")),
        ]));
    }
    J::Arr(out)
}
fn fold_push(loc: Option<Loc>, out: &mut Vec<(usize, usize)>) {
    if let Some(l) = loc {
        if l.el > l.sl {
            out.push((l.sl, l.el));
        }
    }
}
fn fold_decl(d: &Decl, out: &mut Vec<(usize, usize)>) {
    fold_push(d.loc, out);
    let mut exprs: Vec<Rc<Expr>> = vec![];
    let mut types: Vec<&TypeAst> = vec![];
    // the reference's generic pre-order visit: the same order collect_* keep,
    // interleaving members (visited through their types) where they occur
    fold_walk_decl(d, out);
    let _ = (&mut exprs, &mut types);
}
fn fold_walk_decl(d: &Decl, out: &mut Vec<(usize, usize)>) {
    match &d.body {
        DeclBody::Type {
            params, ty, tail, ..
        } => {
            for p in params {
                if let Some(t) = &p.ty {
                    fold_type(t, out);
                }
            }
            fold_type(ty, out);
            if let Some(t) = tail {
                fold_tail(t, out);
            }
        }
        DeclBody::Const { ty, expr, .. } => {
            if let Some(t) = ty {
                fold_type(t, out);
            }
            fold_expr(expr, out);
        }
        DeclBody::Func {
            params, ret, body, ..
        } => {
            for p in params {
                if let Some(t) = &p.ty {
                    fold_type(t, out);
                }
            }
            if let Some(t) = ret {
                fold_type(t, out);
            }
            fold_expr(body, out);
        }
        DeclBody::Output { ty, expr, .. } => {
            fold_type(ty, out);
            fold_expr(expr, out);
        }
        DeclBody::Input { ty, fallback, .. } => {
            fold_type(ty, out);
            if let Some(f) = fallback {
                fold_expr(f, out);
            }
        }
        DeclBody::Diagnostic {
            params, template, ..
        } => {
            for p in params {
                if let Some(t) = &p.ty {
                    fold_type(t, out);
                }
            }
            fold_template(template, out);
        }
        DeclBody::Unit {
            factor: Some(f), ..
        } => {
            fold_expr(f, out);
        }
        _ => {}
    }
}
fn fold_tail(t: &Tail, out: &mut Vec<(usize, usize)>) {
    match t {
        Tail::Inline { template, .. } => fold_template(template, out),
        Tail::Ref { args, .. } => args.iter().for_each(|a| fold_expr(a, out)),
    }
}
fn fold_template(parts: &[TPart], out: &mut Vec<(usize, usize)>) {
    for p in parts {
        if let TPart::Expr(x) = p {
            fold_expr(x, out);
        }
    }
}
fn fold_type(t: &TypeAst, out: &mut Vec<(usize, usize)>) {
    if let TypeAst::Record { .. } = t {
        fold_push(t.loc(), out);
    }
    match t {
        TypeAst::Record { members, .. } => members.iter().for_each(|m| fold_member(m, out)),
        TypeAst::Map { key, val, .. } => {
            fold_type(key, out);
            fold_type(val, out);
        }
        TypeAst::Array { elem, .. } => fold_type(elem, out),
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
            arms.iter().for_each(|a| fold_type(a, out))
        }
        TypeAst::Func { params, ret, .. } => {
            params.iter().for_each(|a| fold_type(a, out));
            fold_type(ret, out);
        }
        TypeAst::Named {
            args, preds, ext, ..
        } => {
            args.iter().for_each(|a| fold_type(a, out));
            preds.iter().flatten().for_each(|x| fold_expr(x, out));
            if let Some(x) = ext {
                fold_type(x, out);
            }
        }
        _ => {}
    }
}
fn fold_member(m: &MemberAst, out: &mut Vec<(usize, usize)>) {
    if let MemberAst::When { .. } = m {
        fold_push(m.loc(), out);
    }
    match m {
        MemberAst::Value { ty, dflt, .. } => {
            fold_type(ty, out);
            if let Some(d) = dflt {
                fold_expr(d, out);
            }
        }
        MemberAst::Derived { ty, expr, .. } => {
            if let Some(t) = ty {
                fold_type(t, out);
            }
            fold_expr(expr, out);
        }
        MemberAst::Context { ty, .. } => fold_type(ty, out),
        MemberAst::Assert { cond, tail, .. } => {
            fold_expr(cond, out);
            if let Some(t) = tail {
                fold_tail(t, out);
            }
        }
        MemberAst::When { cond, body, .. } => {
            fold_expr(cond, out);
            body.iter().for_each(|b| fold_member(b, out));
        }
    }
}
fn fold_expr(e: &Rc<Expr>, out: &mut Vec<(usize, usize)>) {
    if matches!(&**e, Expr::Obj(_) | Expr::Arr(_) | Expr::Match { .. }) {
        fold_push(expr_loc(e), out);
    }
    match &**e {
        Expr::Template(parts) => fold_template(parts, out),
        Expr::Obj(entries) => entries.iter().for_each(|(_, v)| fold_expr(v, out)),
        Expr::Arr(items) => items.iter().for_each(|(_, v)| fold_expr(v, out)),
        Expr::Comp { head, clauses } => {
            fold_expr(head, out);
            for c in clauses {
                fold_expr(&c.iter, out);
                c.filters.iter().for_each(|f| fold_expr(f, out));
            }
        }
        Expr::MapComp { key, val, clauses } => {
            fold_expr(key, out);
            fold_expr(val, out);
            for c in clauses {
                fold_expr(&c.iter, out);
                c.filters.iter().for_each(|f| fold_expr(f, out));
            }
        }
        Expr::Bin { l, r, .. } => {
            fold_expr(l, out);
            fold_expr(r, out);
        }
        Expr::Un { x, .. } | Expr::Paren(x) => fold_expr(x, out),
        Expr::If { c, t, f } => {
            fold_expr(c, out);
            fold_expr(t, out);
            fold_expr(f, out);
        }
        Expr::Lambda { body, .. } => fold_expr(body, out),
        Expr::Call { fun, args } => {
            fold_expr(fun, out);
            args.iter().for_each(|a| fold_expr(a, out));
        }
        Expr::Member { x, .. } => fold_expr(x, out),
        Expr::Index { x, i } => {
            fold_expr(x, out);
            fold_expr(i, out);
        }
        Expr::With { base, patch } => {
            fold_expr(base, out);
            fold_expr(patch, out);
        }
        Expr::Match { subject, arms } => {
            fold_expr(subject, out);
            for a in arms {
                if let Some(t) = &a.ty {
                    fold_type(t, out);
                }
                fold_expr(&a.body, out);
            }
        }
        _ => {}
    }
}
fn formatting(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let Ok(out) = format(text) else {
        return J::Arr(vec![]);
    };
    if &out == text {
        return J::Arr(vec![]);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    J::Arr(vec![J::obj(vec![
        (
            "range",
            range_json(Loc {
                sl: 0,
                sc: 0,
                el: last,
                ec: u16len(lines[last]),
            }),
        ),
        ("newText", J::s(out)),
    ])])
}

// ---------------- rename ----------------
fn prepare_rename(st: &mut State, uri: &str, pos: Pos) -> J {
    let a = st.analysis_of(uri);
    let s = a.as_ref().and_then(|a| site_at(st, a, uri, pos));
    if s.as_ref().map(|s| s.site.is_none()).unwrap_or(true) {
        // a local variable: its binding and uses
        let Some(locs) = local_ranges(st, uri, pos) else {
            return J::Null;
        };
        let Some(here) = locs.iter().find(|l| contains(**l, pos)) else {
            return J::Null;
        };
        let text = st.text(uri).cloned().unwrap_or_default();
        let line = text.split('\n').nth(here.sl).unwrap_or("");
        return J::obj(vec![
            ("range", range_json(*here)),
            ("placeholder", J::s(slice16(line, here.sc, here.ec))),
        ]);
    }
    let s = s.unwrap();
    let (Some(site), Some(hit)) = (&s.site, &s.hit) else {
        return J::Null;
    };
    let loc = match &hit.node {
        NodeRef::Expr(e) => match &***e {
            Expr::Member { name, .. } => member_token_loc(&text_of(st, &s.module), e, name),
            _ => hit.loc,
        },
        NodeRef::Type(t) => {
            let name = match t {
                TypeAst::Named { name, .. } => name.as_str(),
                _ => "",
            };
            let offset = name.find('.').map(|i| i + 1).unwrap_or(0);
            type_name_loc(hit.loc, offset, name.rsplit('.').next().unwrap_or(name))
        }
        NodeRef::Decl(_) | NodeRef::Member(_) => site.range,
    };
    J::obj(vec![
        ("range", range_json(loc)),
        ("placeholder", J::s(site.name.clone())),
    ])
}
fn rename(st: &mut State, uri: &str, pos: Pos, new_name: &str) -> J {
    let refs = references(st, uri, pos, true);
    if refs.is_empty() {
        let Some(locs) = local_ranges(st, uri, pos) else {
            return J::Null;
        };
        if locs.is_empty() {
            return J::Null;
        }
        let edits: Vec<J> = locs
            .iter()
            .map(|l| J::obj(vec![("range", range_json(*l)), ("newText", J::s(new_name))]))
            .collect();
        return J::obj(vec![(
            "changes",
            J::Obj(vec![(uri.to_string(), J::Arr(edits))]),
        )]);
    }
    let mut changes: Vec<(String, Vec<J>)> = vec![];
    for (m, l) in refs {
        let u = uri_of(&m.path);
        let edit = J::obj(vec![("range", range_json(l)), ("newText", J::s(new_name))]);
        match changes.iter_mut().find(|(k, _)| *k == u) {
            Some(e) => e.1.push(edit),
            None => changes.push((u, vec![edit])),
        }
    }
    J::obj(vec![(
        "changes",
        J::Obj(changes.into_iter().map(|(k, v)| (k, J::Arr(v))).collect()),
    )])
}

// ---------------- lenses and commands ----------------
fn code_lenses(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let parsed = parse_source(text);
    if !parsed.errors.is_empty() {
        return J::Arr(vec![]);
    }
    let mut out: Vec<J> = vec![];
    for d in &parsed.decls {
        let Some(l) = d.loc else { continue };
        let (title, command, name) = match &d.body {
            DeclBody::Output { name, .. } => ("evaluate", "decl.evaluate", name),
            DeclBody::Input { name, .. } => ("validate", "decl.validate", name),
            _ => continue,
        };
        out.push(J::obj(vec![
            (
                "range",
                range_json(Loc {
                    sl: l.sl,
                    sc: l.sc,
                    el: l.sl,
                    ec: l.sc,
                }),
            ),
            (
                "command",
                J::obj(vec![
                    ("title", J::s(title)),
                    ("command", J::s(command)),
                    ("arguments", J::Arr(vec![J::s(uri), J::s(name.clone())])),
                ]),
            ),
        ]));
    }
    J::Arr(out)
}
fn execute_command(st: &mut State, command: &str, args: Option<&Value>) -> J {
    let items: Vec<&Value> = match args {
        Some(Value::JArr(a)) => a.iter().collect(),
        _ => vec![],
    };
    let Some(uri) = as_str(items.first().copied()) else {
        return J::Null;
    };
    let root = as_str(items.get(1).copied()).map(|s| s.to_string());
    let path = path_of(uri);
    let mut session = Session::with_overlay(Some(&path.to_string_lossy()), Some(&st.overlay));
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for (name, file) in st.inputs.clone() {
        let abs = std::path::absolute(dir.join(&file)).unwrap_or_else(|_| dir.join(&file));
        let _ = session.apply(Op::Bind {
            name,
            src: BindSource::File {
                file: file.clone(),
                text: read_text(&abs),
            },
        });
    }
    let names: Vec<String> = root.iter().cloned().collect();
    let diags_of = |run: &Run, extra: &[Diag]| -> J {
        let mut all: Vec<&Diag> = run.load_diags.iter().collect();
        all.extend(run.checks.iter().map(|(_, d)| d));
        all.extend(extra.iter());
        J::Arr(all.iter().map(|d| J::s(fmt_diag(d, None))).collect())
    };
    match command {
        "decl.evaluate" => {
            let Ok((run, ds, _exported)) = session.evaluate(&names) else {
                return J::Null;
            };
            let diagnostics = diags_of(&run, &run.diags);
            if let Some(r) = root {
                let doc = ds.first().and_then(|(_, j)| j.clone());
                return J::obj(vec![
                    ("root", J::s(r)),
                    ("document", doc.map(J::s).unwrap_or(J::Null)),
                    ("diagnostics", diagnostics),
                ]);
            }
            let all = if run.eng.is_some() && ds.iter().all(|(_, j)| j.is_some()) {
                Some(format!(
                    "{{{}}}",
                    ds.iter()
                        .map(|(n, j)| format!("{}:{}", json_str(n), j.clone().unwrap()))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            } else {
                None
            };
            J::obj(vec![
                ("root", J::Null),
                ("document", all.map(J::s).unwrap_or(J::Null)),
                ("diagnostics", diagnostics),
            ])
        }
        "decl.validate" => {
            let Ok((run, verdicts, diags)) = session.validate(&names) else {
                return J::Null;
            };
            let vs: Vec<J> = verdicts
                .iter()
                .map(|(n, e, w)| {
                    J::obj(vec![
                        ("name", J::s(n.clone())),
                        ("errors", J::Num(*e as i64)),
                        ("warnings", J::Num(*w as i64)),
                    ])
                })
                .collect();
            J::obj(vec![
                ("verdicts", J::Arr(vs)),
                ("diagnostics", diags_of(&run, &diags)),
            ])
        }
        "decl.trace" => match root {
            Some(r) => match session.trace(&r) {
                Ok(lines) => J::obj(vec![(
                    "lines",
                    J::Arr(lines.into_iter().map(J::s).collect()),
                )]),
                Err(_) => J::Null,
            },
            None => J::Null,
        },
        "decl.showSyntaxTree" => syntax_tree(st, uri),
        "decl.reloadWorkspace" => {
            st.analyses.clear();
            let uris: Vec<String> = st.docs.iter().map(|(u, _)| u.clone()).collect();
            for u in uris {
                analyze(st, &u);
            }
            J::Null
        }
        _ => J::Null,
    }
}

// ---------------- signature help ----------------
fn after(l: Loc, p: Pos) -> bool {
    p.line > l.el || (p.line == l.el && p.character >= l.ec)
}
fn src_of(text: &str, l: Loc) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = |i: usize| lines.get(i).copied().unwrap_or("");
    if l.sl == l.el {
        return slice16(line(l.sl), l.sc, l.ec).to_string();
    }
    let mut parts = vec![slice16(line(l.sl), l.sc, u16len(line(l.sl))).to_string()];
    parts.extend(lines[l.sl + 1..l.el].iter().map(|x| x.to_string()));
    parts.push(slice16(line(l.el), 0, l.ec).to_string());
    parts.join("\n")
}
fn sig_json(label: String, params: Vec<String>, active: usize) -> J {
    J::obj(vec![
        (
            "signatures",
            J::Arr(vec![J::obj(vec![
                ("label", J::s(label)),
                (
                    "parameters",
                    J::Arr(
                        params
                            .into_iter()
                            .map(|p| J::obj(vec![("label", J::s(p))]))
                            .collect(),
                    ),
                ),
            ])]),
        ),
        ("activeSignature", J::Num(0)),
        ("activeParameter", J::Num(active as i64)),
    ])
}
fn decl_by_id(m: &Module, id: Option<usize>) -> Option<&Decl> {
    m.decls.iter().find(|d| decl_id(d) == id.unwrap_or(0))
}
fn signature_help(st: &mut State, uri: &str, pos: Pos) -> J {
    let a = match st.analysis_of(uri) {
        Some(a) => a,
        None => match st.last_good.get(uri) {
            Some(a) => a.clone(),
            None => return J::Null,
        },
    };
    let Some(m) = module_of(&a, &path_of(uri)) else {
        return J::Null;
    };
    let Some(mi) = a.run.modules.iter().position(|x| Rc::ptr_eq(x, &m)) else {
        return J::Null;
    };
    let Some(hit) = node_at(&a.run.modules[mi].decls, pos) else {
        return J::Null;
    };
    let mut chain: Vec<NodeRef> = hit.parents.clone();
    chain.push(hit.node.clone());
    let calls: Vec<&Rc<Expr>> = chain
        .iter()
        .rev()
        .filter_map(|n| match n {
            NodeRef::Expr(e) if matches!(&***e, Expr::Call { .. }) => Some(*e),
            _ => None,
        })
        .collect();
    for c in calls {
        let Expr::Call { fun, args } = &**c else {
            continue;
        };
        let Some(fl) = expr_loc(fun) else { continue };
        if !after(fl, pos) {
            continue;
        }
        let mut active = 0usize;
        for (i, arg) in args.iter().enumerate() {
            if let Some(al) = expr_loc(arg) {
                if after(al, pos) {
                    active = i + 1;
                } else if contains(al, pos) {
                    active = i;
                }
            }
        }
        if let Expr::Name(n) = &**fun {
            let target = resolve_in(&m.env, n);
            let Some(site) = site_of_target(st, &a, target.as_ref()) else {
                return J::Null;
            };
            let sm = site.module.clone();
            let Some(decl) = decl_by_id(&sm, site.decl) else {
                return J::Null;
            };
            let DeclBody::Func {
                name, params, ret, ..
            } = &decl.body
            else {
                return J::Null;
            };
            let text = text_of(st, &sm);
            let ps: Vec<String> = params
                .iter()
                .map(|p| {
                    format!(
                        "{}: {}",
                        p.name,
                        p.ty.as_ref()
                            .and_then(|t| t.loc())
                            .map(|l| src_of(&text, l))
                            .unwrap_or_else(|| "…".into())
                    )
                })
                .collect();
            let r = ret
                .as_ref()
                .and_then(|t| t.loc())
                .map(|l| format!(": {}", src_of(&text, l)))
                .unwrap_or_default();
            let n = ps.len();
            return sig_json(
                format!("{name}({}){r}", ps.join(", ")),
                ps,
                active.min(n.saturating_sub(1)),
            );
        }
        if let Some(sp) = std_path(fun) {
            if let Some(e) = STD.iter().find(|e| e.0 == sp) {
                let ps: Vec<String> = (1..=e.1).map(|i| format!("a{i}")).collect();
                let n = ps.len();
                return sig_json(
                    format!("std.{sp}({})", ps.join(", ")),
                    ps,
                    active.min(n.saturating_sub(1)),
                );
            }
        }
        return J::Null;
    }
    J::Null
}

// ---------------- workspace symbols, selection ranges ----------------
fn workspace_symbols(st: &State, query: &str) -> J {
    let q = query.to_lowercase();
    let mut out: Vec<(String, String, J)> = vec![];
    let mut seen: Vec<PathBuf> = vec![];
    for a in st.last_good.values() {
        for m in &a.run.modules {
            if seen.contains(&m.path) {
                continue;
            }
            seen.push(m.path.clone());
            let text = text_of(st, m);
            for d in &m.decls {
                let (Some(_), Some(name), Some(kind)) = (d.loc, d.name(), symbol_kind(d)) else {
                    continue;
                };
                if !name.to_lowercase().contains(&q) {
                    continue;
                }
                out.push((
                    name.to_string(),
                    uri_of(&m.path),
                    J::obj(vec![
                        ("name", J::s(name)),
                        ("kind", J::Num(kind)),
                        ("location", location(m, name_range(&text, d, name))),
                    ]),
                ));
            }
        }
    }
    out.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
    J::Arr(out.into_iter().map(|x| x.2).collect())
}
fn pos_json(p: Pos) -> J {
    J::obj(vec![
        ("line", J::Num(p.line as i64)),
        ("character", J::Num(p.character as i64)),
    ])
}
fn selection_ranges(st: &State, uri: &str, positions: &[Pos]) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let parsed = parse_source(text);
    let point = |p: Pos| {
        J::obj(vec![(
            "range",
            J::obj(vec![("start", pos_json(p)), ("end", pos_json(p))]),
        )])
    };
    if !parsed.errors.is_empty() {
        return J::Arr(positions.iter().map(|p| point(*p)).collect());
    }
    J::Arr(
        positions
            .iter()
            .map(|p| {
                let Some(hit) = node_at(&parsed.decls, *p) else {
                    return point(*p);
                };
                // the chain, innermost first; rebuilt from the outside in
                let mut chain: Vec<Loc> = vec![hit.loc];
                chain.extend(hit.parents.iter().rev().filter_map(|n| n.loc()));
                let mut sel: Option<J> = None;
                for l in chain.iter().rev() {
                    sel = Some(match sel {
                        Some(s) => J::obj(vec![("range", range_json(*l)), ("parent", s)]),
                        None => J::obj(vec![("range", range_json(*l))]),
                    });
                }
                sel.unwrap_or_else(|| point(*p))
            })
            .collect(),
    )
}

// ---------------- semantic tokens ----------------
const TOKEN_TYPES: [&str; 6] = [
    "type",
    "property",
    "function",
    "variable",
    "namespace",
    "parameter",
];
const TOKEN_MODS: [&str; 8] = [
    "declaration",
    "required",
    "optional",
    "defaulted",
    "derived",
    "hidden",
    "unresolved",
    "readonly",
];
const T_TYPE: i64 = 0;
const T_PROPERTY: i64 = 1;
const T_FUNCTION: i64 = 2;
const T_VARIABLE: i64 = 3;
const T_NAMESPACE: i64 = 4;
const T_PARAMETER: i64 = 5;
const M_DECLARATION: i64 = 1;
const M_REQUIRED: i64 = 2;
const M_OPTIONAL: i64 = 4;
const M_DEFAULTED: i64 = 8;
const M_DERIVED: i64 = 16;
const M_HIDDEN: i64 = 32;
const M_UNRESOLVED: i64 = 64;
const M_READONLY: i64 = 128;
fn member_mods(kind: MKind, hidden: bool) -> i64 {
    (match kind {
        MKind::Der => M_DERIVED,
        MKind::Dflt => M_DEFAULTED,
        MKind::Opt => M_OPTIONAL,
        MKind::Req => M_REQUIRED,
    }) | if hidden { M_HIDDEN } else { 0 }
}
fn member_kind_of(rt: Option<&RT>, name: &str) -> Option<(MKind, bool)> {
    let rt = rt?;
    let r: RT = match &rt.k {
        RTk::Pred { base, .. } => base.clone(),
        RTk::Ref(t) => t.clone(),
        _ => rt.clone(),
    };
    rec_members(&r)
        .into_iter()
        .find(|m| m.name == name)
        .map(|m| (m.kind, m.hidden))
}
fn param_loc(text: &str, decl: &Decl, name: &str) -> Option<Loc> {
    let l = decl.loc?;
    let line = text.split('\n').nth(l.sl).unwrap_or("");
    let open = find16(line, "(", l.sc)?;
    let re = Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap();
    let m = re.find_at(line, byte_col(line, open))?;
    let a = u16_col(line, m.start());
    Some(Loc {
        sl: l.sl,
        sc: a,
        el: l.sl,
        ec: a + u16len(name),
    })
}
struct TokenWalk<'a> {
    st: &'a State,
    a: &'a Analysis,
    m: &'a Rc<Module>,
    text: String,
    t: Rc<Tables>,
    toks: Vec<(Loc, i64, i64)>,
}
impl<'a> TokenWalk<'a> {
    fn push(&mut self, l: Loc, ty: i64, mods: i64) {
        if l.sl == l.el && l.ec > l.sc {
            self.toks.push((l, ty, mods));
        }
    }
    fn decl(&mut self, d: &Decl) {
        let mut in_func: Vec<String> = vec![];
        if let (Some(_), Some(name)) = (d.loc, d.name()) {
            let r = name_range(&self.text, d, name);
            let ty = match &d.body {
                DeclBody::Type { .. } | DeclBody::Dimension { .. } | DeclBody::Unit { .. } => {
                    T_TYPE
                }
                DeclBody::Func { .. } | DeclBody::Diagnostic { .. } => T_FUNCTION,
                _ => T_VARIABLE,
            };
            self.push(
                r,
                ty,
                M_DECLARATION
                    | if matches!(d.body, DeclBody::Const { .. }) {
                        M_READONLY
                    } else {
                        0
                    },
            );
            if let DeclBody::Func { params, .. } = &d.body {
                in_func = params.iter().map(|p| p.name.clone()).collect();
                for p in params {
                    if let Some(pl) = param_loc(&self.text, d, &p.name) {
                        self.push(pl, T_PARAMETER, M_DECLARATION);
                    }
                }
            }
        }
        match &d.body {
            DeclBody::Type {
                params, ty, tail, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t, &in_func);
                    }
                }
                self.ty(ty, &in_func);
                if let Some(t) = tail {
                    self.tail(t, &in_func);
                }
            }
            DeclBody::Const { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t, &in_func);
                }
                self.expr(expr, &in_func);
            }
            DeclBody::Func {
                params, ret, body, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t, &in_func);
                    }
                }
                if let Some(t) = ret {
                    self.ty(t, &in_func);
                }
                self.expr(body, &in_func);
            }
            DeclBody::Output { ty, expr, .. } => {
                self.ty(ty, &in_func);
                self.expr(expr, &in_func);
            }
            DeclBody::Input { ty, fallback, .. } => {
                self.ty(ty, &in_func);
                if let Some(f) = fallback {
                    self.expr(f, &in_func);
                }
            }
            DeclBody::Diagnostic {
                params, template, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t, &in_func);
                    }
                }
                self.template(template, &in_func);
            }
            DeclBody::Unit {
                factor: Some(f), ..
            } => {
                self.expr(f, &in_func);
            }
            _ => {}
        }
    }
    fn tail(&mut self, t: &Tail, in_func: &[String]) {
        match t {
            Tail::Inline { template, .. } => self.template(template, in_func),
            Tail::Ref { args, .. } => {
                for a in args {
                    self.expr(a, in_func);
                }
            }
        }
    }
    fn template(&mut self, parts: &[TPart], in_func: &[String]) {
        for p in parts {
            if let TPart::Expr(x) = p {
                self.expr(x, in_func);
            }
        }
    }
    fn ty(&mut self, t: &TypeAst, in_func: &[String]) {
        if let TypeAst::Named {
            name, loc: Some(l), ..
        } = t
        {
            let mut parts = name.splitn(2, '.');
            let head = parts.next().unwrap_or("");
            match parts.next() {
                Some(tail) => {
                    self.push(type_name_loc(*l, 0, head), T_NAMESPACE, 0);
                    self.push(type_name_loc(*l, u16len(head) + 1, tail), T_TYPE, 0);
                }
                None => {
                    if !["map", "ref", "quantity"].contains(&head) {
                        let mods = if resolve_in(&self.m.env, head).is_some() {
                            0
                        } else {
                            M_UNRESOLVED
                        };
                        self.push(type_name_loc(*l, 0, head), T_TYPE, mods);
                    }
                }
            }
        }
        match t {
            TypeAst::Record { members, .. } => {
                for m in members {
                    self.member(m, in_func);
                }
            }
            TypeAst::Map { key, val, .. } => {
                self.ty(key, in_func);
                self.ty(val, in_func);
            }
            TypeAst::Array { elem, .. } => self.ty(elem, in_func),
            TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
                for a in arms {
                    self.ty(a, in_func);
                }
            }
            TypeAst::Func { params, ret, .. } => {
                for a in params {
                    self.ty(a, in_func);
                }
                self.ty(ret, in_func);
            }
            TypeAst::Named {
                args, preds, ext, ..
            } => {
                for a in args {
                    self.ty(a, in_func);
                }
                for x in preds.iter().flatten() {
                    self.expr(x, in_func);
                }
                if let Some(x) = ext {
                    self.ty(x, in_func);
                }
            }
            _ => {}
        }
    }
    fn member(&mut self, m: &MemberAst, in_func: &[String]) {
        match m {
            MemberAst::Value {
                name,
                opt,
                dflt,
                loc: Some(_),
                ..
            } => {
                let kind = if dflt.is_some() {
                    MKind::Dflt
                } else if *opt {
                    MKind::Opt
                } else {
                    MKind::Req
                };
                let r = member_range(&self.text, m, name);
                self.push(r, T_PROPERTY, M_DECLARATION | member_mods(kind, false));
            }
            MemberAst::Derived {
                name,
                hidden,
                loc: Some(_),
                ..
            } => {
                let r = member_range(&self.text, m, name);
                self.push(
                    r,
                    T_PROPERTY,
                    M_DECLARATION | member_mods(MKind::Der, *hidden),
                );
            }
            _ => {}
        }
        match m {
            MemberAst::Value { ty, dflt, .. } => {
                self.ty(ty, in_func);
                if let Some(d) = dflt {
                    self.expr(d, in_func);
                }
            }
            MemberAst::Derived { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t, in_func);
                }
                self.expr(expr, in_func);
            }
            MemberAst::Context { ty, .. } => self.ty(ty, in_func),
            MemberAst::Assert { cond, tail, .. } => {
                self.expr(cond, in_func);
                if let Some(t) = tail {
                    self.tail(t, in_func);
                }
            }
            MemberAst::When { cond, body, .. } => {
                self.expr(cond, in_func);
                for b in body {
                    self.member(b, in_func);
                }
            }
        }
    }
    fn expr(&mut self, e: &Rc<Expr>, in_func: &[String]) {
        let mut scope: Vec<String> = in_func.to_vec();
        if let Some(l) = expr_loc(e) {
            match &**e {
                Expr::Name(n) => {
                    let target = match self.t.res.get(&key_of(e)) {
                        Some(r) => r.clone(),
                        None => resolve_in(&self.m.env, n),
                    };
                    if n == "std" {
                        self.push(l, T_NAMESPACE, 0);
                    } else if in_func.contains(n)
                        || target.as_ref().map(|t| t.kind == "var").unwrap_or(false)
                    {
                        self.push(l, T_PARAMETER, 0);
                    } else {
                        match target {
                            None => self.push(l, T_VARIABLE, M_UNRESOLVED),
                            Some(t) => {
                                let ty = match t.kind {
                                    "func" => T_FUNCTION,
                                    "namespace" => T_NAMESPACE,
                                    "type" => T_TYPE,
                                    _ => T_VARIABLE,
                                };
                                self.push(l, ty, if t.kind == "const" { M_READONLY } else { 0 });
                            }
                        }
                    }
                }
                Expr::Member { x, name, .. } => {
                    let ml = member_token_loc(&self.text, e, name);
                    if let Some(sp) = std_path(e) {
                        self.push(
                            ml,
                            if STD.iter().any(|s| s.0 == sp) {
                                T_FUNCTION
                            } else {
                                T_NAMESPACE
                            },
                            0,
                        );
                    } else if let Expr::Name(xn) = &**x {
                        if self.m.env.namespaces.borrow().contains_key(xn) {
                            let nss = self.m.env.namespaces.borrow();
                            let ex = nss
                                .get(xn)
                                .and_then(|(_, exports)| exports.borrow().get(name).cloned());
                            let tg = ex.and_then(|ex| resolve_in(&ex.env, &ex.name));
                            let ty = match tg.as_ref().map(|t| t.kind) {
                                Some("func") => T_FUNCTION,
                                Some("type") => T_TYPE,
                                _ => T_VARIABLE,
                            };
                            self.push(ml, ty, if tg.is_some() { 0 } else { M_UNRESOLVED });
                        } else {
                            let mk = member_kind_of(
                                self.t.types.get(&key_of(x)).and_then(|t| t.rt.as_ref()),
                                name,
                            );
                            self.push(
                                ml,
                                T_PROPERTY,
                                mk.map(|(k, h)| member_mods(k, h)).unwrap_or(0),
                            );
                        }
                    } else {
                        let mk = member_kind_of(
                            self.t.types.get(&key_of(x)).and_then(|t| t.rt.as_ref()),
                            name,
                        );
                        self.push(
                            ml,
                            T_PROPERTY,
                            mk.map(|(k, h)| member_mods(k, h)).unwrap_or(0),
                        );
                    }
                }
                Expr::Lambda { params, .. } => scope.extend(params.iter().cloned()),
                Expr::Comp { clauses, .. } | Expr::MapComp { clauses, .. } => {
                    scope.extend(clauses.iter().map(|c| c.v.clone()))
                }
                _ => {}
            }
        }
        let s = &scope;
        match &**e {
            Expr::Template(parts) => self.template(parts, s),
            Expr::Obj(entries) => {
                for (_, v) in entries {
                    self.expr(v, s);
                }
            }
            Expr::Arr(items) => {
                for (_, v) in items {
                    self.expr(v, s);
                }
            }
            Expr::Comp { head, clauses } => {
                self.expr(head, s);
                for c in clauses {
                    self.expr(&c.iter, s);
                    for f in &c.filters {
                        self.expr(f, s);
                    }
                }
            }
            Expr::MapComp { key, val, clauses } => {
                self.expr(key, s);
                self.expr(val, s);
                for c in clauses {
                    self.expr(&c.iter, s);
                    for f in &c.filters {
                        self.expr(f, s);
                    }
                }
            }
            Expr::Bin { l, r, .. } => {
                self.expr(l, s);
                self.expr(r, s);
            }
            Expr::Un { x, .. } | Expr::Paren(x) => self.expr(x, s),
            Expr::If { c, t, f } => {
                self.expr(c, s);
                self.expr(t, s);
                self.expr(f, s);
            }
            Expr::Lambda { body, .. } => self.expr(body, s),
            Expr::Call { fun, args } => {
                self.expr(fun, s);
                for a in args {
                    self.expr(a, s);
                }
            }
            Expr::Member { x, .. } => self.expr(x, s),
            Expr::Index { x, i } => {
                self.expr(x, s);
                self.expr(i, s);
            }
            Expr::With { base, patch } => {
                self.expr(base, s);
                self.expr(patch, s);
            }
            Expr::Match { subject, arms } => {
                self.expr(subject, s);
                for a in arms {
                    if let Some(t) = &a.ty {
                        self.ty(t, s);
                    }
                    self.expr(&a.body, s);
                }
            }
            _ => {}
        }
    }
}
fn semantic_tokens(st: &mut State, uri: &str) -> J {
    let empty = J::obj(vec![("data", J::Arr(vec![]))]);
    let Some(a) = st.analysis_of(uri) else {
        return empty;
    };
    let Some(m) = module_of(&a, &path_of(uri)) else {
        return empty;
    };
    let t = tables_of(&a, &m);
    let text = text_of(st, &m);
    let mut w = TokenWalk {
        st,
        a: &a,
        m: &m,
        text,
        t,
        toks: vec![],
    };
    let _ = w.st;
    let _ = w.a;
    for d in &m.decls {
        w.decl(d);
    }
    let mut toks = w.toks;
    toks.sort_by(|p, q| p.0.sl.cmp(&q.0.sl).then(p.0.sc.cmp(&q.0.sc)));
    let mut data: Vec<J> = vec![];
    let (mut pl, mut pc) = (0usize, 0usize);
    for (l, ty, mods) in toks {
        let dl = l.sl - pl;
        let dc: i64 = if dl == 0 {
            l.sc as i64 - pc as i64
        } else {
            l.sc as i64
        };
        if dl == 0 && dc < 0 {
            continue; // overlapping tokens: the first wins
        }
        data.extend([
            J::Num(dl as i64),
            J::Num(dc),
            J::Num((l.ec - l.sc) as i64),
            J::Num(ty),
            J::Num(mods),
        ]);
        pl = l.sl;
        pc = l.sc;
    }
    J::obj(vec![("data", J::Arr(data))])
}

// ---------------- inlay hints ----------------
struct HintWalk<'a> {
    st: &'a State,
    a: &'a Analysis,
    m: &'a Rc<Module>,
    text: String,
    t: Rc<Tables>,
    range: (Pos, Pos),
    /// types, parameter names, units, values, context variables
    hints: (bool, bool, bool, bool, bool),
    out: Vec<(Pos, J)>,
}
impl<'a> HintWalk<'a> {
    fn in_range(&self, p: Pos) -> bool {
        p.line >= self.range.0.line && p.line <= self.range.1.line
    }
    fn hint(&mut self, p: Pos, label: String, kind: Option<i64>, pad_left: bool, pad_right: bool) {
        let mut item = vec![("position", pos_json(p)), ("label", J::s(label))];
        if let Some(k) = kind {
            item.push(("kind", J::Num(k)));
        }
        if pad_right {
            item.push(("paddingRight", J::Bool(true)));
        }
        if pad_left {
            item.push(("paddingLeft", J::Bool(true)));
        }
        self.out.push((p, J::obj(item)));
    }
    fn decl(&mut self, d: &Decl) {
        if let DeclBody::Const {
            name,
            ty: None,
            expr,
        } = &d.body
        {
            if self.hints.0 && d.loc.is_some() {
                if let Some(rt) = self.t.types.get(&key_of(expr)).and_then(|t| t.rt.clone()) {
                    let r = name_range(&self.text, d, name);
                    let p = Pos {
                        line: r.el,
                        character: r.ec,
                    };
                    if self.in_range(p) {
                        self.hint(
                            p,
                            format!(": {}", type_text(Some(&rt))),
                            Some(1),
                            false,
                            false,
                        );
                    }
                }
            }
        }
        if let DeclBody::Output { name, expr, .. } = &d.body {
            if self.hints.3 && d.loc.is_some() {
                if let (Some(eng), Some(entry)) = (self.a.run.eng.clone(), self.a.run.entry.clone())
                {
                    let root = entry
                        .env
                        .roots
                        .borrow()
                        .borrow()
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| v.clone());
                    if let Some(v) = root {
                        self.values(&v, expr, name, &eng);
                    }
                }
            }
        }
        match &d.body {
            DeclBody::Type {
                params, ty, tail, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t);
                    }
                }
                self.ty(ty);
                if let Some(t) = tail {
                    self.tail(t);
                }
            }
            DeclBody::Const { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t);
                }
                self.expr(expr);
            }
            DeclBody::Func {
                params, ret, body, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t);
                    }
                }
                if let Some(t) = ret {
                    self.ty(t);
                }
                self.expr(body);
            }
            DeclBody::Output { ty, expr, .. } => {
                self.ty(ty);
                self.expr(expr);
            }
            DeclBody::Input { ty, fallback, .. } => {
                self.ty(ty);
                if let Some(f) = fallback {
                    self.expr(f);
                }
            }
            DeclBody::Diagnostic {
                params, template, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t);
                    }
                }
                self.template(template);
            }
            DeclBody::Unit {
                factor: Some(f), ..
            } => {
                self.expr(f);
            }
            _ => {}
        }
    }
    // the evaluated derived members of a literal, at the end of the literal
    fn values(&mut self, v: &Value, e: &Rc<Expr>, root: &str, eng: &crate::engine::Engine) {
        match (v, &**e) {
            (Value::Rec(inst), Expr::Obj(entries)) => {
                let inst = inst.borrow();
                let mut parts: Vec<String> = vec![];
                for mem in rec_members(&inst.rt) {
                    let Some((_, s)) = inst.slots.iter().find(|(n, _)| *n == mem.name) else {
                        continue;
                    };
                    if mem.kind != MKind::Der || s.hidden || s.state != SlotState::Ok {
                        continue;
                    }
                    let txt = eng.serialize(&s.value, root, false);
                    let shown = if u16len(&txt) > 40 {
                        format!("{}…", slice16(&txt, 0, 37))
                    } else {
                        txt
                    };
                    parts.push(format!("{} = {}", mem.name, shown));
                }
                if let Some(l) = expr_loc(e) {
                    let p = Pos {
                        line: l.el,
                        character: l.ec,
                    };
                    if !parts.is_empty() && self.in_range(p) {
                        self.hint(p, format!("// {}", parts.join(", ")), None, true, false);
                    }
                }
                let children: Vec<(Value, Rc<Expr>)> = entries
                    .iter()
                    .filter_map(|(k, val)| {
                        inst.slots
                            .iter()
                            .find(|(n, _)| n == k)
                            .filter(|(_, s)| s.state == SlotState::Ok)
                            .map(|(_, s)| (s.value.clone(), val.clone()))
                    })
                    .collect();
                drop(inst);
                for (cv, ce) in children {
                    self.values(&cv, &ce, root, eng);
                }
            }
            (Value::Arr(arr), Expr::Arr(items)) => {
                let vals: Vec<Value> = arr.borrow().items.clone();
                for (i, it) in vals.iter().enumerate() {
                    if let Some((_, ce)) = items.get(i) {
                        self.values(it, ce, root, eng);
                    }
                }
            }
            (Value::Map(map), Expr::Obj(entries)) => {
                for (k, val) in entries {
                    let cv = map.borrow().get(k).cloned();
                    if let Some(cv) = cv {
                        self.values(&cv, val, root, eng);
                    }
                }
            }
            _ => {}
        }
    }
    fn tail(&mut self, t: &Tail) {
        match t {
            Tail::Inline { template, .. } => self.template(template),
            Tail::Ref { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
        }
    }
    fn template(&mut self, parts: &[TPart]) {
        for p in parts {
            if let TPart::Expr(x) = p {
                self.expr(x);
            }
        }
    }
    fn ty(&mut self, t: &TypeAst) {
        match t {
            TypeAst::Record { members, .. } => {
                for m in members {
                    self.member(m);
                }
            }
            TypeAst::Map { key, val, .. } => {
                self.ty(key);
                self.ty(val);
            }
            TypeAst::Array { elem, .. } => self.ty(elem),
            TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
                for a in arms {
                    self.ty(a);
                }
            }
            TypeAst::Func { params, ret, .. } => {
                for a in params {
                    self.ty(a);
                }
                self.ty(ret);
            }
            TypeAst::Named {
                args, preds, ext, ..
            } => {
                for a in args {
                    self.ty(a);
                }
                for x in preds.iter().flatten() {
                    self.expr(x);
                }
                if let Some(x) = ext {
                    self.ty(x);
                }
            }
            _ => {}
        }
    }
    fn member(&mut self, m: &MemberAst) {
        if let MemberAst::Derived {
            name,
            ty: None,
            expr,
            hidden,
            loc: Some(_),
        } = m
        {
            if self.hints.0 {
                if let Some(rt) = self.t.types.get(&key_of(expr)).and_then(|t| t.rt.clone()) {
                    let r = member_range(&self.text, m, name);
                    let p = Pos {
                        line: r.el,
                        character: r.ec + if *hidden { 1 } else { 0 },
                    };
                    if self.in_range(p) {
                        self.hint(
                            p,
                            format!(": {}", type_text(Some(&rt))),
                            Some(1),
                            false,
                            false,
                        );
                    }
                }
            }
        }
        match m {
            MemberAst::Value { ty, dflt, .. } => {
                self.ty(ty);
                if let Some(d) = dflt {
                    self.expr(d);
                }
            }
            MemberAst::Derived { ty, expr, .. } => {
                if let Some(t) = ty {
                    self.ty(t);
                }
                self.expr(expr);
            }
            MemberAst::Context { ty, .. } => self.ty(ty),
            MemberAst::Assert { cond, tail, .. } => {
                self.expr(cond);
                if let Some(t) = tail {
                    self.tail(t);
                }
            }
            MemberAst::When { cond, body, .. } => {
                self.expr(cond);
                for b in body {
                    self.member(b);
                }
            }
        }
    }
    fn expr(&mut self, e: &Rc<Expr>) {
        match &**e {
            Expr::Call { fun, args } if self.hints.1 => {
                if let Expr::Name(n) = &**fun {
                    let target = match self.t.res.get(&key_of(fun)) {
                        Some(r) => r.clone(),
                        None => resolve_in(&self.m.env, n),
                    };
                    if let Some(site) = site_of_target(self.st, self.a, target.as_ref()) {
                        let sm = site.module.clone();
                        if let Some(DeclBody::Func { params, .. }) =
                            decl_by_id(&sm, site.decl).map(|d| &d.body)
                        {
                            for (i, arg) in args.iter().enumerate() {
                                if let (Some(p), Some(al)) = (params.get(i), expr_loc(arg)) {
                                    let pos = Pos {
                                        line: al.sl,
                                        character: al.sc,
                                    };
                                    if self.in_range(pos) {
                                        self.hint(
                                            pos,
                                            format!("{}:", p.name),
                                            Some(2),
                                            false,
                                            true,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Expr::Ctx(name)
                if self.hints.4 && (name == "$parent" || name == "$root" || name == "$key") =>
            {
                // the bound the enclosing type declares for the variable
                if let Some(l) = expr_loc(e) {
                    let decl = self.m.decls.iter().find(|d| {
                        matches!(d.body, DeclBody::Type { .. })
                            && d.loc
                                .map(|dl| dl.sl <= l.sl && l.el <= dl.el)
                                .unwrap_or(false)
                    });
                    let tl =
                        decl.and_then(record_body_of)
                            .map(record_members)
                            .and_then(|members| {
                                members.iter().find_map(|mm| match mm {
                                    MemberAst::Context { variable, ty, .. } if variable == name => {
                                        ty.loc()
                                    }
                                    _ => None,
                                })
                            });
                    let p = Pos {
                        line: l.el,
                        character: l.ec,
                    };
                    if let Some(tl) = tl {
                        if self.in_range(p) {
                            self.hint(
                                p,
                                format!(": {}", src_of(&self.text, tl)),
                                Some(1),
                                false,
                                false,
                            );
                        }
                    }
                }
            }
            Expr::UnitLit { num, unit } if self.hints.2 => {
                if let (Ok((key, to_base)), Some(l)) = (self.m.env.unit_info(unit), expr_loc(e)) {
                    let base = self
                        .m
                        .env
                        .base_unit_of
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or(key);
                    let p = Pos {
                        line: l.el,
                        character: l.ec,
                    };
                    if base != *unit && self.in_range(p) {
                        self.hint(
                            p,
                            format!("= {} {}", crate::semantics::js_num_str(num * to_base), base),
                            None,
                            true,
                            false,
                        );
                    }
                }
            }
            _ => {}
        }
        match &**e {
            Expr::Template(parts) => self.template(parts),
            Expr::Obj(entries) => {
                for (_, v) in entries {
                    self.expr(v);
                }
            }
            Expr::Arr(items) => {
                for (_, v) in items {
                    self.expr(v);
                }
            }
            Expr::Comp { head, clauses } => {
                self.expr(head);
                for c in clauses {
                    self.expr(&c.iter);
                    for f in &c.filters {
                        self.expr(f);
                    }
                }
            }
            Expr::MapComp { key, val, clauses } => {
                self.expr(key);
                self.expr(val);
                for c in clauses {
                    self.expr(&c.iter);
                    for f in &c.filters {
                        self.expr(f);
                    }
                }
            }
            Expr::Bin { l, r, .. } => {
                self.expr(l);
                self.expr(r);
            }
            Expr::Un { x, .. } | Expr::Paren(x) => self.expr(x),
            Expr::If { c, t, f } => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Call { fun, args } => {
                self.expr(fun);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Member { x, .. } => self.expr(x),
            Expr::Index { x, i } => {
                self.expr(x);
                self.expr(i);
            }
            Expr::With { base, patch } => {
                self.expr(base);
                self.expr(patch);
            }
            Expr::Match { subject, arms } => {
                self.expr(subject);
                for a in arms {
                    if let Some(t) = &a.ty {
                        self.ty(t);
                    }
                    self.expr(&a.body);
                }
            }
            _ => {}
        }
    }
}
fn inlay_hints(st: &mut State, uri: &str, range: (Pos, Pos)) -> J {
    let Some(a) = st.analysis_of(uri) else {
        return J::Arr(vec![]);
    };
    let Some(m) = module_of(&a, &path_of(uri)) else {
        return J::Arr(vec![]);
    };
    let t = tables_of(&a, &m);
    let text = text_of(st, &m);
    let hints = (
        st.hint_types,
        st.hint_parameter_names,
        st.hint_units,
        st.hint_values,
        st.hint_context_variables,
    );
    let mut w = HintWalk {
        st,
        a: &a,
        m: &m,
        text,
        t,
        range,
        hints,
        out: vec![],
    };
    for d in &m.decls {
        w.decl(d);
    }
    let mut out = w.out;
    out.sort_by(|p, q| {
        p.0.line
            .cmp(&q.0.line)
            .then(p.0.character.cmp(&q.0.character))
    });
    J::Arr(out.into_iter().map(|x| x.1).collect())
}

// ---------------- hierarchies ----------------
fn hierarchy_item(m: &Module, d: &Decl, text: &str) -> J {
    let name = d.name().unwrap_or("");
    J::obj(vec![
        ("name", J::s(name)),
        ("kind", J::Num(symbol_kind(d).unwrap_or(13))),
        ("uri", J::s(uri_of(&m.path))),
        ("range", range_json(d.loc.unwrap())),
        ("selectionRange", range_json(name_range(text, d, name))),
    ])
}
fn prepare_hierarchy(st: &mut State, uri: &str, pos: Pos, want: &str) -> J {
    let Some(a) = st.analysis_of(uri) else {
        return J::Null;
    };
    let Some(site) = site_at(st, &a, uri, pos).and_then(|s| s.site) else {
        return J::Null;
    };
    if site.decl.is_none() || site.kind != want {
        return J::Null;
    }
    let sm = site.module.clone();
    let Some(decl) = decl_by_id(&sm, site.decl) else {
        return J::Null;
    };
    J::Arr(vec![hierarchy_item(&sm, decl, &text_of(st, &sm))])
}
fn module_of_uri(st: &State, uri: &str) -> Option<(Rc<Analysis>, Rc<Module>)> {
    for a in st.last_good.values() {
        if let Some(m) = a.run.modules.iter().find(|x| uri_of(&x.path) == uri) {
            return Some((a.clone(), m.clone()));
        }
    }
    None
}
fn decl_containing(m: &Module, loc: Loc) -> Option<&Decl> {
    m.decls.iter().find(|d| {
        d.loc
            .map(|dl| dl.sl <= loc.sl && loc.el <= dl.el)
            .unwrap_or(false)
            && d.name().is_some()
    })
}
fn item_uri_line(item: Option<&Value>) -> Option<(String, usize)> {
    let item = item?;
    let uri = as_str(get(item, "uri"))?.to_string();
    let line = as_usize(
        get(item, "range")
            .and_then(|r| get(r, "start"))
            .and_then(|s| get(s, "line")),
    )?;
    Some((uri, line))
}
fn incoming_calls(st: &State, item: Option<&Value>) -> J {
    let Some((uri, line)) = item_uri_line(item) else {
        return J::Arr(vec![]);
    };
    let Some((a, _)) = module_of_uri(st, &uri) else {
        return J::Arr(vec![]);
    };
    let mut out: Vec<J> = vec![];
    for m in a.run.modules.clone() {
        let t = tables_of(&a, &m);
        let text = text_of(st, &m);
        let mut by_caller: Vec<(usize, Vec<Loc>)> = vec![];
        for d in &m.decls {
            let mut exprs: Vec<Rc<Expr>> = vec![];
            let mut types: Vec<&TypeAst> = vec![];
            collect_decl(d, &mut exprs, &mut types);
            for x in &exprs {
                let Expr::Call { fun, .. } = &**x else {
                    continue;
                };
                let Expr::Name(n) = &**fun else { continue };
                let Some(fl) = expr_loc(fun) else { continue };
                let tg = match t.res.get(&key_of(fun)) {
                    Some(r) => r.clone(),
                    None => resolve_in(&m.env, n),
                };
                let Some(site) = site_of_target(st, &a, tg.as_ref()) else {
                    continue;
                };
                if site.decl.is_none()
                    || uri_of(&site.module.path) != uri
                    || site.decl_loc.map(|l| l.sl) != Some(line)
                {
                    continue;
                }
                if let Some(caller) = decl_containing(&m, fl) {
                    let id = decl_id(caller);
                    match by_caller.iter_mut().find(|(k, _)| *k == id) {
                        Some(e) => e.1.push(fl),
                        None => by_caller.push((id, vec![fl])),
                    }
                }
            }
        }
        for (id, locs) in by_caller {
            let Some(caller) = decl_by_id(&m, Some(id)) else {
                continue;
            };
            out.push(J::obj(vec![
                ("from", hierarchy_item(&m, caller, &text)),
                (
                    "fromRanges",
                    J::Arr(locs.into_iter().map(range_json).collect()),
                ),
            ]));
        }
    }
    J::Arr(out)
}
fn outgoing_calls(st: &State, item: Option<&Value>) -> J {
    let Some((uri, line)) = item_uri_line(item) else {
        return J::Arr(vec![]);
    };
    let Some((a, m)) = module_of_uri(st, &uri) else {
        return J::Arr(vec![]);
    };
    let t = tables_of(&a, &m);
    let Some(decl) = m.decls.iter().find(|d| d.loc.map(|l| l.sl) == Some(line)) else {
        return J::Arr(vec![]);
    };
    let mut exprs: Vec<Rc<Expr>> = vec![];
    let mut types: Vec<&TypeAst> = vec![];
    collect_decl(decl, &mut exprs, &mut types);
    let mut by_callee: Vec<(String, J, Vec<Loc>)> = vec![];
    for x in &exprs {
        let Expr::Call { fun, .. } = &**x else {
            continue;
        };
        let Expr::Name(n) = &**fun else { continue };
        let Some(fl) = expr_loc(fun) else { continue };
        let tg = match t.res.get(&key_of(fun)) {
            Some(r) => r.clone(),
            None => resolve_in(&m.env, n),
        };
        let Some(site) = site_of_target(st, &a, tg.as_ref()) else {
            continue;
        };
        if site.kind != "func" {
            continue;
        }
        let sm = site.module.clone();
        let Some(callee) = decl_by_id(&sm, site.decl) else {
            continue;
        };
        let key = format!(
            "{}:{}",
            sm.path.to_string_lossy(),
            callee.loc.map(|l| l.sl).unwrap_or(0)
        );
        match by_callee.iter_mut().find(|(k, _, _)| *k == key) {
            Some(e) => e.2.push(fl),
            None => by_callee.push((
                key,
                hierarchy_item(&sm, callee, &text_of(st, &sm)),
                vec![fl],
            )),
        }
    }
    J::Arr(
        by_callee
            .into_iter()
            .map(|(_, to, locs)| {
                J::obj(vec![
                    ("to", to),
                    (
                        "fromRanges",
                        J::Arr(locs.into_iter().map(range_json).collect()),
                    ),
                ])
            })
            .collect(),
    )
}
fn supertypes(st: &State, item: Option<&Value>) -> J {
    let Some((uri, line)) = item_uri_line(item) else {
        return J::Arr(vec![]);
    };
    let Some((a, m)) = module_of_uri(st, &uri) else {
        return J::Arr(vec![]);
    };
    let base = m.decls.iter().find_map(|d| match &d.body {
        DeclBody::Type {
            ty: TypeAst::Named {
                name, ext: Some(_), ..
            },
            ..
        } if d.loc.map(|l| l.sl) == Some(line) => Some(name.clone()),
        _ => None,
    });
    let Some(base) = base else {
        return J::Arr(vec![]);
    };
    let Some(site) = site_of_target(st, &a, resolve_in(&m.env, &base).as_ref()) else {
        return J::Arr(vec![]);
    };
    let sm = site.module.clone();
    match decl_by_id(&sm, site.decl) {
        Some(d) => J::Arr(vec![hierarchy_item(&sm, d, &text_of(st, &sm))]),
        None => J::Arr(vec![]),
    }
}
fn subtypes(st: &State, item: Option<&Value>) -> J {
    let Some((uri, line)) = item_uri_line(item) else {
        return J::Arr(vec![]);
    };
    let Some((a, _)) = module_of_uri(st, &uri) else {
        return J::Arr(vec![]);
    };
    let mut out: Vec<J> = vec![];
    for m in a.run.modules.clone() {
        for d in &m.decls {
            let DeclBody::Type {
                ty: TypeAst::Named {
                    name, ext: Some(_), ..
                },
                ..
            } = &d.body
            else {
                continue;
            };
            if d.loc.is_none() {
                continue;
            }
            let Some(site) = site_of_target(st, &a, resolve_in(&m.env, name).as_ref()) else {
                continue;
            };
            if site.decl.is_some()
                && uri_of(&site.module.path) == uri
                && site.decl_loc.map(|l| l.sl) == Some(line)
            {
                out.push(hierarchy_item(&m, d, &text_of(st, &m)));
            }
        }
    }
    J::Arr(out)
}

// ---------------- code actions ----------------
fn js_value_string(v: &Value) -> String {
    match v {
        Value::Str(s) => json_str(s),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => crate::semantics::js_num_str(*f),
        Value::Bool(b) => b.to_string(),
        _ => "null".into(),
    }
}
// the record body of a type declaration: its own, or its extension's
trait ConstExprLoc {
    fn const_expr_loc(&self) -> Option<Loc>;
}
impl ConstExprLoc for DeclBody {
    fn const_expr_loc(&self) -> Option<Loc> {
        match self {
            DeclBody::Const { expr, .. } => expr_loc(expr),
            _ => None,
        }
    }
}
fn record_body_of(d: &Decl) -> Option<&TypeAst> {
    let DeclBody::Type { ty, .. } = &d.body else {
        return None;
    };
    match ty {
        TypeAst::Record { .. } => Some(ty),
        TypeAst::Named { ext: Some(x), .. } => match &**x {
            TypeAst::Record { .. } => Some(&**x),
            _ => None,
        },
        _ => None,
    }
}
fn record_parts(t: &TypeAst) -> Option<(&[MemberAst], Loc, bool)> {
    match t {
        TypeAst::Record {
            members,
            open,
            loc: Some(l),
        } => Some((members, *l, *open)),
        _ => None,
    }
}
fn safe_resolve(m: &Module, type_name: &str) -> Option<RT> {
    m.env
        .resolve(
            &TypeAst::Named {
                name: type_name.to_string(),
                args: vec![],
                preds: None,
                ext: None,
                loc: None,
            },
            None,
        )
        .ok()
}
fn placeholder_for(rt: Option<&RT>) -> String {
    let Some(rt) = rt else { return "null".into() };
    let r: RT = match &rt.k {
        RTk::Pred { base, .. } => base.clone(),
        _ => rt.clone(),
    };
    match &r.k {
        RTk::Prim(name) => match name.as_str() {
            "string" => "\"\"".into(),
            "int" => "0".into(),
            "float" => "0.0".into(),
            "bool" => "false".into(),
            _ => "null".into(),
        },
        RTk::Lit(v) => js_value_string(v),
        RTk::Range { lo, .. } => js_value_string(lo),
        RTk::Rec(_) => "{ }".into(),
        RTk::Arr { .. } => "[]".into(),
        RTk::Map { .. } => "{}".into(),
        RTk::Union(arms) => placeholder_for(arms.first()),
        _ => "null".into(),
    }
}
fn value_to_j(v: &Value) -> J {
    match v {
        Value::Null | Value::Undef | Value::Absent => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::Num(i.to_string().parse().unwrap_or(0)),
        Value::Float(f) => J::Str(crate::semantics::js_num_str(*f)),
        Value::Str(s) => J::s(s.clone()),
        Value::JArr(items) => J::Arr(items.iter().map(value_to_j).collect()),
        Value::JObj(es) => J::Obj(es.iter().map(|(k, x)| (k.clone(), value_to_j(x))).collect()),
        other => J::s(format!("{other:?}")),
    }
}
// `path.resolve(dir, spec)`: the joined path, `.` and `..` folded lexically
fn resolve_lexical(dir: &Path, spec: &str) -> PathBuf {
    let joined = if spec.starts_with('/') {
        PathBuf::from(spec)
    } else {
        dir.join(spec)
    };
    let mut parts: Vec<String> = vec![];
    for c in joined.to_string_lossy().split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x.to_string()),
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}
fn relative_path(from_dir: &Path, to: &Path) -> String {
    let f: Vec<&str> = from_dir
        .to_str()
        .unwrap_or("")
        .split('/')
        .filter(|x| !x.is_empty())
        .collect();
    let t: Vec<&str> = to
        .to_str()
        .unwrap_or("")
        .split('/')
        .filter(|x| !x.is_empty())
        .collect();
    let mut i = 0;
    while i < f.len() && i < t.len() && f[i] == t[i] {
        i += 1;
    }
    let mut parts: Vec<&str> = f[i..].iter().map(|_| "..").collect();
    parts.extend(t[i..].iter().copied());
    parts.join("/")
}
fn require_rel(from: &Path, to: &Path) -> String {
    let rel = relative_path(from.parent().unwrap_or(Path::new("/")), to);
    if rel.starts_with('.') {
        rel.strip_prefix("./").unwrap_or(&rel).to_string()
    } else {
        rel
    }
}
// the modules that export a name: the universe's, the other open
// documents' universes, then the .decl files beside the module
fn exporters_of(st: &State, a: &Analysis, m: &Module, name: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    let mut seen: Vec<PathBuf> = vec![m.path.clone()];
    let consider = |md: &Module, out: &mut Vec<PathBuf>, seen: &mut Vec<PathBuf>| {
        if !seen.contains(&md.path) && md.exports.borrow().contains_key(name) {
            seen.push(md.path.clone());
            out.push(md.path.clone());
        }
    };
    for md in &a.run.modules {
        consider(md, &mut out, &mut seen);
    }
    for other in st.last_good.values() {
        for md in &other.run.modules {
            consider(md, &mut out, &mut seen);
        }
    }
    let dir = m.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for f in names {
        if !f.ends_with(".decl") {
            continue;
        }
        let p = resolve_lexical(&dir, &f);
        if seen.contains(&p) {
            continue;
        }
        let text = st.overlay.get(&p).cloned().unwrap_or_else(|| read_text(&p));
        let parsed = parse_source(&text);
        if !parsed.errors.is_empty() {
            continue;
        }
        if parsed.decls.iter().any(|d| {
            d.exported && d.name() == Some(name) && !matches!(d.body, DeclBody::Import { .. })
        }) {
            seen.push(p.clone());
            out.push(p);
        }
    }
    out
}
fn edit_json(l: Loc, new_text: &str) -> J {
    J::obj(vec![("range", range_json(l)), ("newText", J::s(new_text))])
}
fn insert_json(p: Pos, new_text: &str) -> J {
    edit_json(
        Loc {
            sl: p.line,
            sc: p.character,
            el: p.line,
            ec: p.character,
        },
        new_text,
    )
}
fn action_edits_json(
    title: String,
    kind: &str,
    diagnostic: Option<J>,
    preferred: bool,
    uri: &str,
    edits: Vec<J>,
) -> J {
    let mut item = vec![("title", J::s(title)), ("kind", J::s(kind))];
    if let Some(d) = diagnostic {
        item.push(("diagnostics", J::Arr(vec![d])));
    }
    if preferred {
        item.push(("isPreferred", J::Bool(true)));
    }
    item.push((
        "edit",
        J::obj(vec![(
            "changes",
            J::Obj(vec![(uri.to_string(), J::Arr(edits))]),
        )]),
    ));
    J::obj(item)
}
fn action_json(
    title: String,
    kind: &str,
    diagnostic: Option<J>,
    preferred: bool,
    uri: &str,
    edit: J,
) -> J {
    action_edits_json(title, kind, diagnostic, preferred, uri, vec![edit])
}
fn diag_pos(d: &Value, which: &str) -> Pos {
    let p = get(d, "range").and_then(|r| get(r, which));
    Pos {
        line: as_usize(p.and_then(|s| get(s, "line"))).unwrap_or(0),
        character: as_usize(p.and_then(|s| get(s, "character"))).unwrap_or(0),
    }
}
fn chain_of<'a>(hit: &Hit<'a>) -> Vec<NodeRef<'a>> {
    let mut chain: Vec<NodeRef> = vec![hit.node.clone()];
    chain.extend(hit.parents.iter().rev().cloned());
    chain
}
fn chain_expr<'a>(chain: &[NodeRef<'a>], pred: impl Fn(&Expr) -> bool) -> Option<Rc<Expr>> {
    chain.iter().find_map(|n| match n {
        NodeRef::Expr(e) if pred(e) => Some((*e).clone()),
        _ => None,
    })
}
// every expression under a node, in the reference's pre-order
fn exprs_under<'a>(n: &NodeRef<'a>) -> Vec<Rc<Expr>> {
    let mut exprs: Vec<Rc<Expr>> = vec![];
    let mut types: Vec<&TypeAst> = vec![];
    match n {
        NodeRef::Decl(d) => collect_decl(d, &mut exprs, &mut types),
        NodeRef::Member(m) => collect_member(m, &mut exprs, &mut types),
        NodeRef::Type(t) => collect_type(t, &mut exprs, &mut types),
        NodeRef::Expr(e) => collect_expr(e, &mut exprs, &mut types),
    }
    exprs
}
// a literal's type widened to its primitive: a declaration wants `bool`, not `true`
fn widen(rt: &RT) -> RT {
    match &rt.k {
        RTk::Lit(v) => {
            let name = match v {
                Value::Str(_) => "string",
                Value::Bool(_) => "bool",
                Value::Int(_) => "int",
                Value::Float(_) => "float",
                _ => "null",
            };
            crate::semantics::ty(RTk::Prim(name.to_string()))
        }
        _ => rt.clone(),
    }
}
fn is_logical(op: &str) -> bool {
    op == "&&" || op == "||"
}
// the mixed `??` operand: the first binary node (pre-order) whose operator
// mixes with its parent's, whichever side it is on
fn mixed_in_expr(e: &Rc<Expr>, parent_op: Option<&str>) -> Option<Rc<Expr>> {
    if let Expr::Bin { op, .. } = &**e {
        if let (Some(pop), Some(_)) = (parent_op, expr_loc(e)) {
            if (op == "??" && is_logical(pop)) || (is_logical(op) && pop == "??") {
                return Some(e.clone());
            }
        }
    }
    let op: Option<&str> = match &**e {
        Expr::Bin { op, .. } => Some(op.as_str()),
        _ => None,
    };
    let kids: Vec<Rc<Expr>> = match &**e {
        Expr::Template(parts) => parts
            .iter()
            .filter_map(|p| {
                if let TPart::Expr(x) = p {
                    Some(x.clone())
                } else {
                    None
                }
            })
            .collect(),
        Expr::Obj(entries) => entries.iter().map(|(_, v)| v.clone()).collect(),
        Expr::Arr(items) => items.iter().map(|(_, v)| v.clone()).collect(),
        Expr::Comp { head, clauses } => std::iter::once(head.clone())
            .chain(
                clauses
                    .iter()
                    .flat_map(|c| std::iter::once(c.iter.clone()).chain(c.filters.iter().cloned())),
            )
            .collect(),
        Expr::MapComp { key, val, clauses } => vec![key.clone(), val.clone()]
            .into_iter()
            .chain(
                clauses
                    .iter()
                    .flat_map(|c| std::iter::once(c.iter.clone()).chain(c.filters.iter().cloned())),
            )
            .collect(),
        Expr::Bin { l, r, .. } => vec![l.clone(), r.clone()],
        Expr::Un { x, .. } | Expr::Paren(x) => vec![x.clone()],
        Expr::If { c, t, f } => vec![c.clone(), t.clone(), f.clone()],
        Expr::Lambda { body, .. } => vec![body.clone()],
        Expr::Call { fun, args } => std::iter::once(fun.clone())
            .chain(args.iter().cloned())
            .collect(),
        Expr::Member { x, .. } => vec![x.clone()],
        Expr::Index { x, i } => vec![x.clone(), i.clone()],
        Expr::With { base, patch } => vec![base.clone(), patch.clone()],
        Expr::Match { subject, arms } => std::iter::once(subject.clone())
            .chain(arms.iter().map(|a| a.body.clone()))
            .collect(),
        _ => vec![],
    };
    for k in kids {
        if let Some(t) = mixed_in_expr(&k, op) {
            return Some(t);
        }
    }
    None
}
fn mixed_in_node(n: &NodeRef) -> Option<Rc<Expr>> {
    match n {
        NodeRef::Expr(e) => mixed_in_expr(e, None),
        NodeRef::Decl(d) => {
            let tops: Vec<Rc<Expr>> = match &d.body {
                DeclBody::Const { expr, .. } | DeclBody::Output { expr, .. } => vec![expr.clone()],
                DeclBody::Input { fallback, .. } => fallback.iter().cloned().collect(),
                DeclBody::Func { body, .. } => vec![body.clone()],
                DeclBody::Unit { factor, .. } => factor.iter().cloned().collect(),
                _ => vec![],
            };
            tops.iter().find_map(|e| mixed_in_expr(e, None))
        }
        NodeRef::Member(m) => match m {
            MemberAst::Derived { expr, .. } => mixed_in_expr(expr, None),
            MemberAst::Value { dflt: Some(d), .. } => mixed_in_expr(d, None),
            MemberAst::Assert { cond, .. } | MemberAst::When { cond, .. } => {
                mixed_in_expr(cond, None)
            }
            _ => None,
        },
        NodeRef::Type(_) => None,
    }
}
// does an expression read any name (a constant expression reads none)?
fn mentions_name(e: &Rc<Expr>) -> bool {
    let mut exprs: Vec<Rc<Expr>> = vec![];
    let mut types: Vec<&TypeAst> = vec![];
    collect_expr(e, &mut exprs, &mut types);
    matches!(&**e, Expr::Name(_) | Expr::Ctx(_) | Expr::Referrers { .. })
        || exprs
            .iter()
            .any(|x| matches!(&**x, Expr::Name(_) | Expr::Ctx(_) | Expr::Referrers { .. }))
}
fn leading_spaces(line: &str) -> String {
    line.chars().take_while(|c| *c == ' ').collect()
}
// ---------------- on-type formatting ----------------
// `\n`: the new line takes the previous line's indentation, one level
// deeper after an opening bracket or a continuation point (§2.9); `}`:
// the line dedents to the opening brace's line
fn on_type_formatting(st: &State, uri: &str, pos: Pos, ch: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Arr(vec![]);
    };
    let lines: Vec<&str> = text.split('\n').collect();
    let line = |i: usize| lines.get(i).copied().unwrap_or("");
    let indent_of = |s: &str| s.chars().take_while(|c| *c == ' ').count();
    let edit = |l: usize, have: usize, want: usize| {
        if have == want {
            J::Arr(vec![])
        } else {
            J::Arr(vec![edit_json(
                Loc {
                    sl: l,
                    sc: 0,
                    el: l,
                    ec: have,
                },
                &" ".repeat(want),
            )])
        }
    };
    if ch == "\n" {
        let prev = if pos.line >= 1 {
            line(pos.line - 1)
        } else {
            ""
        };
        let cur = line(pos.line);
        let body = Regex::new(r"//.*$")
            .unwrap()
            .replace(prev, "")
            .trim_end()
            .to_string();
        let mut want = indent_of(prev);
        if Regex::new(r"[{\[(]$").unwrap().is_match(&body)
            || Regex::new(r"(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$")
                .unwrap()
                .is_match(&body)
        {
            want += 4;
        }
        if Regex::new(r"^\s*[}\])]").unwrap().is_match(cur) {
            want = want.saturating_sub(4);
        }
        return edit(pos.line, indent_of(cur), want);
    }
    if ch == "}" || ch == "]" || ch == ")" {
        let cur = line(pos.line);
        if cur.trim() != ch {
            return J::Arr(vec![]);
        }
        let close = ch.chars().next().unwrap();
        let open = match ch {
            "}" => '{',
            "]" => '[',
            _ => '(',
        };
        // the opening bracket's line: scan back with a depth count
        let mut depth = 0i64;
        for l in (0..=pos.line).rev() {
            let s = line(l);
            let chars: Vec<char> = s.chars().collect();
            let end = if l == pos.line {
                cur.find(ch).map(|b| cur[..b].chars().count()).unwrap_or(0)
            } else {
                chars.len()
            };
            for i in (0..end).rev() {
                let c = chars[i];
                if c == close {
                    depth += 1;
                } else if c == open {
                    if depth == 0 {
                        return edit(pos.line, indent_of(cur), indent_of(s));
                    }
                    depth -= 1;
                }
            }
        }
        return J::Arr(vec![]);
    }
    J::Arr(vec![])
}

fn code_actions(st: &mut State, uri: &str, range: (Pos, Pos), diagnostics: &[Value]) -> J {
    let Some(text) = st.text(uri).cloned() else {
        return J::Arr(vec![]);
    };
    let a = st.analysis_of(uri);
    let mut out: Vec<J> = vec![];
    let parsed = parse_source(&text);
    let lines: Vec<&str> = text.split('\n').collect();
    let (Some(a), true) = (a, parsed.errors.is_empty()) else {
        return J::Arr(out);
    };
    let Some(m) = module_of(&a, &path_of(uri)) else {
        return J::Arr(out);
    };
    let t = tables_of(&a, &m);
    let re_unknown = Regex::new(r"^unknown name ([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let re_undeclared = Regex::new(
        r"^member ([A-Za-z_][A-Za-z0-9_]*) is not declared on ([A-Za-z_][A-Za-z0-9_]*)$",
    )
    .unwrap();
    let re_missing = Regex::new(r"^required member ([A-Za-z_][A-Za-z0-9_]*) missing").unwrap();
    let re_ctx_undeclared =
        Regex::new(r"^(\$[a-z]+) used without a context declaration in ([A-Za-z_][A-Za-z0-9_]*)$")
            .unwrap();
    let re_ctx_ref =
        Regex::new(r"^(\$[a-z]+) declaration must be ref<\.\.\.> \(([A-Za-z_][A-Za-z0-9_]*)\)$")
            .unwrap();
    let re_override = Regex::new(r"^(?:illegal member-kind transition for|override widens inherited member) ([A-Za-z_][A-Za-z0-9_]*)[^(]*\(([A-Za-z_][A-Za-z0-9_]*)\)$").unwrap();
    let re_union =
        Regex::new(r"^record union arms not discriminable in ([A-Za-z_][A-Za-z0-9_]*)$").unwrap();
    let re_restated =
        Regex::new(r"^derived member ([A-Za-z_][A-Za-z0-9_]*) restated with a differing value")
            .unwrap();
    let trailing_comma = Regex::new(r",\s*$").unwrap();
    // the fixes of the diagnostics that touch the range (a client may send more)
    let touches = |d: &Value| {
        get(d, "range").is_some()
            && !(diag_pos(d, "end").line < range.0.line || diag_pos(d, "start").line > range.1.line)
    };
    for d in diagnostics.iter().filter(|d| touches(d)) {
        let message = as_str(get(d, "message")).unwrap_or("").to_string();
        let dpos = diag_pos(d, "start");
        if let Some(cap) = re_unknown.captures(&message) {
            let name = cap[1].to_string();
            for other in exporters_of(st, &a, &m, &name) {
                let mut spec = format!("./{}", require_rel(&m.path, &other));
                let dir = m.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let existing = parsed.decls.iter().find(|x| match &x.body {
                    DeclBody::Import {
                        from,
                        names: Some(_),
                        ..
                    } => resolve_lexical(&dir, from) == other,
                    _ => false,
                });
                let edit = match existing {
                    Some(x) => {
                        let l = x.loc.unwrap();
                        let line = lines.get(l.sl).copied().unwrap_or("");
                        let close = find16(line, "}", l.sc).map(|c| c as i64).unwrap_or(-1);
                        if let DeclBody::Import { from, .. } = &x.body {
                            spec = from.clone();
                        }
                        edit_json(
                            Loc {
                                sl: l.sl,
                                sc: close.max(0) as usize,
                                el: l.sl,
                                ec: close.max(0) as usize,
                            },
                            &format!(", {name} "),
                        )
                    }
                    None => {
                        let last_import = parsed.decls.iter().rev().find(|x| {
                            matches!(x.body, DeclBody::Import { .. } | DeclBody::ReExport { .. })
                        });
                        let at = last_import
                            .and_then(|x| x.loc)
                            .map(|l| l.el + 1)
                            .unwrap_or(0);
                        edit_json(
                            Loc {
                                sl: at,
                                sc: 0,
                                el: at,
                                ec: 0,
                            },
                            &format!("import {{ {name} }} from \"{spec}\"\n"),
                        )
                    }
                };
                out.push(action_json(
                    format!("import {name} from \"{spec}\""),
                    "quickfix",
                    Some(value_to_j(d)),
                    true,
                    uri,
                    edit,
                ));
            }
            // a namespace import that exports it: qualify the name (namespaces in declaration order)
            let mut nss: Vec<String> = vec![];
            for x in &parsed.decls {
                if let DeclBody::Import { ns: Some(ns), .. } = &x.body {
                    if !nss.contains(ns) {
                        nss.push(ns.clone());
                    }
                }
            }
            for ns in nss {
                let exports = m.env.namespaces.borrow().get(&ns).map(|(_, ex)| ex.clone());
                let Some(exports) = exports else { continue };
                if !exports.borrow().contains_key(&name) {
                    continue;
                }
                let Some(hit) = node_at(&parsed.decls, dpos) else {
                    continue;
                };
                let chain = chain_of(&hit);
                let n = chain_expr(&chain, |x| matches!(x, Expr::Name(nm) if *nm == name));
                if let Some(l) = n.as_ref().and_then(expr_loc) {
                    out.push(action_json(
                        format!("qualify as {ns}.{name}"),
                        "quickfix",
                        Some(value_to_j(d)),
                        false,
                        uri,
                        edit_json(l, &format!("{ns}.{name}")),
                    ));
                }
            }
        }
        if let Some(cap) = re_undeclared.captures(&message) {
            // declare the member on the type, with the supplied value's inferred type
            let (name, type_name) = (cap[1].to_string(), cap[2].to_string());
            let site = site_of_target(st, &a, resolve_in(&m.env, &type_name).as_ref());
            if let Some(site) = site {
                let sm = site.module.clone();
                let body: Option<(&[MemberAst], Loc)> =
                    decl_by_id(&sm, site.decl).and_then(|decl| match &decl.body {
                        DeclBody::Type { ty, .. } => match ty {
                            TypeAst::Record {
                                members,
                                loc: Some(l),
                                ..
                            } => Some((members.as_slice(), *l)),
                            TypeAst::Named { ext: Some(ext), .. } => match &**ext {
                                TypeAst::Record {
                                    members,
                                    loc: Some(l),
                                    ..
                                } => Some((members.as_slice(), *l)),
                                _ => None,
                            },
                            _ => None,
                        },
                        _ => None,
                    });
                if let Some((members, body_loc)) = body {
                    let hit = node_at(&parsed.decls, dpos);
                    let mut entry: Option<Rc<Expr>> = None;
                    if let Some(hit) = &hit {
                        let chain = chain_of(hit);
                        if let Some(obj) = chain_expr(&chain, |x| matches!(x, Expr::Obj(_))) {
                            if let Expr::Obj(entries) = &*obj {
                                entry = entries
                                    .iter()
                                    .find(|(k, _)| *k == name)
                                    .map(|(_, v)| v.clone());
                            }
                        }
                        if entry.is_none() {
                            for o in exprs_under(&hit.node) {
                                if let Expr::Obj(entries) = &*o {
                                    if let Some((_, v)) = entries.iter().find(|(k, _)| *k == name) {
                                        entry = Some(v.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    let ty = entry
                        .as_ref()
                        .and_then(|v| t.types.get(&key_of(v)))
                        .and_then(|x| x.rt.clone());
                    let member_type = ty
                        .map(|rt| type_text(Some(&widen(&rt))))
                        .unwrap_or_else(|| "any".into());
                    let last = members.last().and_then(|x| x.loc());
                    let (at, new_text) = match last {
                        Some(l) => (
                            Pos {
                                line: l.el,
                                character: l.ec,
                            },
                            format!("\n    {name}: {member_type}"),
                        ),
                        None => (
                            Pos {
                                line: body_loc.sl,
                                character: body_loc.sc + 1,
                            },
                            format!(" {name}: {member_type}"),
                        ),
                    };
                    out.push(action_json(
                        format!("declare {name}: {member_type} on {type_name}"),
                        "quickfix",
                        Some(value_to_j(d)),
                        true,
                        &uri_of(&sm.path),
                        insert_json(at, &new_text),
                    ));
                }
            }
        }
        if message.starts_with("member access on a maybe-absent expression") {
            if let Some(hit) = node_at(&parsed.decls, dpos) {
                let chain = chain_of(&hit);
                if let Some(n) =
                    chain_expr(&chain, |x| matches!(x, Expr::Member { safe: false, .. }))
                {
                    if let (Expr::Member { name, .. }, Some(_)) = (&*n, expr_loc(&n)) {
                        let tok = member_token_loc(&text, &n, name);
                        out.push(action_json(
                            "use ?.".into(),
                            "quickfix",
                            Some(value_to_j(d)),
                            true,
                            uri,
                            edit_json(
                                Loc {
                                    sl: tok.sl,
                                    sc: tok.sc - 1,
                                    el: tok.sl,
                                    ec: tok.sc,
                                },
                                "?.",
                            ),
                        ));
                    }
                }
            }
        }
        if message.starts_with("maybe-absent expression consumed") {
            if let Some(hit) = node_at(&parsed.decls, dpos) {
                if let NodeRef::Expr(e) = &hit.node {
                    if let Some(l) = expr_loc(e) {
                        out.push(action_json(
                            "supply a fallback with ??".into(),
                            "quickfix",
                            Some(value_to_j(d)),
                            false,
                            uri,
                            insert_json(
                                Pos {
                                    line: l.el,
                                    character: l.ec,
                                },
                                " ?? null",
                            ),
                        ));
                    }
                }
            }
        }
        if message.starts_with("`??` mixed with") {
            // parenthesize the `??` operand of the mixed expression
            if let Some(hit) = node_at(&parsed.decls, dpos) {
                if let Some(target) = mixed_in_node(&hit.node) {
                    let l = expr_loc(&target).unwrap();
                    out.push(action_edits_json(
                        "parenthesize the ?? expression".into(),
                        "quickfix",
                        Some(value_to_j(d)),
                        true,
                        uri,
                        vec![
                            insert_json(
                                Pos {
                                    line: l.sl,
                                    character: l.sc,
                                },
                                "(",
                            ),
                            insert_json(
                                Pos {
                                    line: l.el,
                                    character: l.ec,
                                },
                                ")",
                            ),
                        ],
                    ));
                }
            }
        }
        if message.starts_with("`match` is not exhaustive") {
            if let Some(hit) = node_at(&parsed.decls, dpos) {
                let chain = chain_of(&hit);
                if let Some(n) = chain_expr(&chain, |x| matches!(x, Expr::Match { .. })) {
                    let Expr::Match {
                        subject,
                        arms: match_arms,
                    } = &*n
                    else {
                        unreachable!()
                    };
                    let subject_rt = t.types.get(&key_of(subject)).and_then(|x| x.rt.clone());
                    let arms: Vec<String> = match subject_rt.as_ref().map(|r| &r.k) {
                        Some(RTk::Union(us)) => us
                            .iter()
                            .filter_map(|r| {
                                if let RTk::Rec(_) = &r.k {
                                    rec_name(Some(r))
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        _ => vec![],
                    };
                    let covered: Vec<String> = match_arms
                        .iter()
                        .map(|arm| match &arm.ty {
                            Some(TypeAst::Named { name, .. }) => name.clone(),
                            _ => String::new(),
                        })
                        .collect();
                    let missing: Vec<String> =
                        arms.into_iter().filter(|x| !covered.contains(x)).collect();
                    if let (Some(l), false) = (expr_loc(&n), missing.is_empty()) {
                        let at = Pos {
                            line: l.el,
                            character: l.ec - 1,
                        };
                        let indent = format!(
                            "{}    ",
                            leading_spaces(lines.get(l.sl).copied().unwrap_or(""))
                        );
                        let new_text = format!(
                            "{}{}",
                            missing
                                .iter()
                                .map(|x| format!("{indent}(v: {x}) => null\n"))
                                .collect::<String>(),
                            &indent[4..]
                        );
                        out.push(action_json(
                            format!(
                                "add the missing arm{}: {}",
                                if missing.len() > 1 { "s" } else { "" },
                                missing.join(", ")
                            ),
                            "quickfix",
                            Some(value_to_j(d)),
                            true,
                            uri,
                            insert_json(at, &new_text),
                        ));
                    }
                }
            }
        }
        if let Some(cap) = re_ctx_undeclared.captures(&message) {
            // declare the context variable on the type: `$parent: ref<{ ... }>`, `$root: ref<{ ... }>`, `$key: string`
            let (variable, type_name) = (cap[1].to_string(), cap[2].to_string());
            if let Some(site) = site_of_target(st, &a, resolve_in(&m.env, &type_name).as_ref()) {
                let sm = site.module.clone();
                if let Some((members, body_loc, _)) = decl_by_id(&sm, site.decl)
                    .and_then(record_body_of)
                    .and_then(record_parts)
                {
                    let bound = if variable == "$key" {
                        "string"
                    } else {
                        "ref<{ ... }>"
                    };
                    let first = members.first().and_then(|x| x.loc());
                    let at = match first {
                        Some(l) => Pos {
                            line: l.sl,
                            character: l.sc,
                        },
                        None => Pos {
                            line: body_loc.sl,
                            character: body_loc.sc + 1,
                        },
                    };
                    let new_text = match first {
                        Some(l) if l.sl > body_loc.sl => {
                            format!("{variable}: {bound}\n{}", " ".repeat(l.sc))
                        }
                        Some(_) => format!("{variable}: {bound}, "),
                        None => format!(" {variable}: {bound}, "),
                    };
                    out.push(action_json(
                        format!("declare {variable}: {bound} on {type_name}"),
                        "quickfix",
                        Some(value_to_j(d)),
                        true,
                        &uri_of(&sm.path),
                        insert_json(at, &new_text),
                    ));
                }
            }
        }
        if let Some(cap) = re_ctx_ref.captures(&message) {
            // the declared bound wrapped in ref<…>
            let (variable, type_name) = (cap[1].to_string(), cap[2].to_string());
            if let Some(site) = site_of_target(st, &a, resolve_in(&m.env, &type_name).as_ref()) {
                let sm = site.module.clone();
                let tl = decl_by_id(&sm, site.decl)
                    .and_then(record_body_of)
                    .map(record_members)
                    .and_then(|members| {
                        members.iter().find_map(|x| match x {
                            MemberAst::Context {
                                variable: v, ty, ..
                            } if *v == variable => ty.loc(),
                            _ => None,
                        })
                    });
                if let Some(tl) = tl {
                    let src = src_of(&text_of(st, &sm), tl);
                    out.push(action_json(
                        format!("declare {variable} as ref<{src}>"),
                        "quickfix",
                        Some(value_to_j(d)),
                        true,
                        &uri_of(&sm.path),
                        edit_json(tl, &format!("ref<{src}>")),
                    ));
                }
            }
        }
        if let Some(cap) = re_override.captures(&message) {
            // the override replaced by the parent's declaration of the member
            let (member, type_name) = (cap[1].to_string(), cap[2].to_string());
            if let Some(site) = site_of_target(st, &a, resolve_in(&m.env, &type_name).as_ref()) {
                let sm = site.module.clone();
                if let Some(decl) = decl_by_id(&sm, site.decl) {
                    let own = record_body_of(decl)
                        .map(record_members)
                        .and_then(|members| {
                            members
                                .iter()
                                .find(|x| x.name() == Some(member.as_str()) && x.loc().is_some())
                        })
                        .and_then(|x| x.loc());
                    let base = match &decl.body {
                        DeclBody::Type {
                            ty: TypeAst::Named { name, .. },
                            ..
                        } => Some(name.clone()),
                        _ => None,
                    };
                    let parent = base.and_then(|b| {
                        member_site(st, &a, &sm, safe_resolve(&sm, &b).as_ref(), &member)
                    });
                    if let (Some(own_loc), Some(parent)) = (own, parent) {
                        if let Some(pl) = parent.member_loc {
                            let parent_text = trailing_comma
                                .replace(&src_of(&text_of(st, &parent.module), pl), "")
                                .to_string();
                            out.push(action_json(
                                format!("use the parent's declaration: {parent_text}"),
                                "quickfix",
                                Some(value_to_j(d)),
                                true,
                                &uri_of(&sm.path),
                                edit_json(own_loc, &parent_text),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(cap) = re_union.captures(&message) {
            // a literal-typed `kind` member on every arm that is a local record type
            let uname = cap[1].to_string();
            if let Some(site) = site_of_target(st, &a, resolve_in(&m.env, &uname).as_ref()) {
                let sm = site.module.clone();
                let arms: Vec<String> = match decl_by_id(&sm, site.decl).map(|d| &d.body) {
                    Some(DeclBody::Type {
                        ty: TypeAst::Union { arms, .. },
                        ..
                    }) => arms
                        .iter()
                        .filter_map(|t| match t {
                            TypeAst::Named { name, .. } => Some(name.clone()),
                            _ => None,
                        })
                        .collect(),
                    _ => vec![],
                };
                let mut changes: Vec<(String, Vec<J>)> = vec![];
                for arm in arms {
                    let Some(as_) = site_of_target(st, &a, resolve_in(&sm.env, &arm).as_ref())
                    else {
                        continue;
                    };
                    let am = as_.module.clone();
                    let Some((members, body_loc, _)) = decl_by_id(&am, as_.decl)
                        .and_then(record_body_of)
                        .and_then(record_parts)
                    else {
                        continue;
                    };
                    if members.iter().any(|x| x.name() == Some("kind")) {
                        continue;
                    }
                    let first = members.first().and_then(|x| x.loc());
                    let at = match first {
                        Some(l) => Pos {
                            line: l.sl,
                            character: l.sc,
                        },
                        None => Pos {
                            line: body_loc.sl,
                            character: body_loc.sc + 1,
                        },
                    };
                    let new_text = match first {
                        Some(l) if l.sl > body_loc.sl => {
                            format!("kind: \"{arm}\"\n{}", " ".repeat(l.sc))
                        }
                        Some(_) => format!("kind: \"{arm}\", "),
                        None => format!(" kind: \"{arm}\", "),
                    };
                    let u = uri_of(&am.path);
                    match changes.iter_mut().find(|(k, _)| *k == u) {
                        Some((_, v)) => v.push(insert_json(at, &new_text)),
                        None => changes.push((u, vec![insert_json(at, &new_text)])),
                    }
                }
                if !changes.is_empty() {
                    out.push(J::obj(vec![
                        (
                            "title",
                            J::s(format!("add a discriminant `kind` to the arms of {uname}")),
                        ),
                        ("kind", J::s("quickfix")),
                        ("diagnostics", J::Arr(vec![value_to_j(d)])),
                        ("isPreferred", J::Bool(true)),
                        (
                            "edit",
                            J::obj(vec![(
                                "changes",
                                J::Obj(changes.into_iter().map(|(k, v)| (k, J::Arr(v))).collect()),
                            )]),
                        ),
                    ]));
                }
            }
        }
        if let Some(cap) = re_restated.captures(&message) {
            // a document supplies it: make it defaulted (`x?: T = e`) where it is declared with a type
            let member = cap[1].to_string();
            for decl in &m.decls {
                let Some(members) = record_body_of(decl).map(record_members) else {
                    continue;
                };
                let Some(own) = members.iter().find(|x| matches!(x, MemberAst::Derived { name, ty: Some(_), loc: Some(_), .. } if *name == member)) else { continue };
                let r = member_range(&text, own, &member);
                out.push(action_json(
                    format!(
                        "make {}.{member} defaulted (x?: T = e)",
                        decl.name().unwrap_or("")
                    ),
                    "quickfix",
                    Some(value_to_j(d)),
                    false,
                    uri,
                    insert_json(
                        Pos {
                            line: r.el,
                            character: r.ec,
                        },
                        "?",
                    ),
                ));
            }
        }
        if let Some(cap) = re_missing.captures(&message) {
            let name = cap[1].to_string();
            // the construction: the literal at the diagnostic, or the root's literal when the diagnostic names the declaration
            let Some(hit) = node_at(&parsed.decls, dpos) else {
                continue;
            };
            let chain = chain_of(&hit);
            let mut obj: Option<Rc<Expr>> = chain_expr(&chain, |x| matches!(x, Expr::Obj(_)));
            if obj.is_none() {
                obj = chain
                    .iter()
                    .filter_map(|n| match n {
                        NodeRef::Decl(d) => Some(*d),
                        _ => None,
                    })
                    .filter_map(|d| match &d.body {
                        DeclBody::Output { expr, .. } => Some(expr.clone()),
                        DeclBody::Input { fallback, .. } => fallback.clone(),
                        DeclBody::Const { expr, .. } => Some(expr.clone()),
                        _ => None,
                    })
                    .find(|e| matches!(&**e, Expr::Obj(_)));
            }
            let Some(obj) = obj else { continue };
            // the literal's type: its declared position (a root's annotation), else what inference recorded
            let owner = chain.iter().find_map(|n| match n {
                NodeRef::Decl(d) => match &d.body {
                    DeclBody::Output { ty, .. } | DeclBody::Input { ty, .. } => Some(ty.clone()),
                    _ => None,
                },
                _ => None,
            });
            let rt: Option<RT> = match owner {
                Some(ty) => m.env.resolve(&ty, None).ok(),
                None => t.types.get(&key_of(&obj)).and_then(|x| x.rt.clone()),
            };
            let mem = rt
                .as_ref()
                .and_then(|r| rec_members(r).into_iter().find(|x| x.name == name));
            let value = placeholder_for(mem.as_ref().and_then(|x| x.ty.as_ref()));
            let Expr::Obj(entries) = &*obj else { continue };
            let edit = match entries.last().and_then(|(_, v)| expr_loc(v)) {
                Some(vl) => insert_json(
                    Pos {
                        line: vl.el,
                        character: vl.ec,
                    },
                    &format!(", {name}: {value}"),
                ),
                None => {
                    let ol = expr_loc(&obj).unwrap();
                    insert_json(
                        Pos {
                            line: ol.sl,
                            character: ol.sc + 1,
                        },
                        &format!(" {name}: {value}"),
                    )
                }
            };
            out.push(action_json(
                format!("add {name}: {value}"),
                "quickfix",
                Some(value_to_j(d)),
                true,
                uri,
                edit,
            ));
        }
    }
    // assists at the range
    let Some(hit) = node_at(&parsed.decls, range.0) else {
        return J::Arr(out);
    };
    let chain = chain_of(&hit);
    let mut one = |title: String, kind: &str, edits: Vec<J>| {
        out.push(J::obj(vec![
            ("title", J::s(title)),
            ("kind", J::s(kind)),
            (
                "edit",
                J::obj(vec![(
                    "changes",
                    J::Obj(vec![(uri.to_string(), J::Arr(edits))]),
                )]),
            ),
        ]))
    };
    // annotate an unannotated derived member or constant with its inferred type
    for n in &chain {
        let (expr, r, hidden) = match n {
            NodeRef::Member(
                mm @ MemberAst::Derived {
                    name,
                    ty: None,
                    expr,
                    hidden,
                    ..
                },
            ) => (expr.clone(), member_range(&text, mm, name), *hidden),
            NodeRef::Decl(d) => match &d.body {
                DeclBody::Const {
                    name,
                    ty: None,
                    expr,
                } => (expr.clone(), name_range(&text, d, name), false),
                _ => continue,
            },
            _ => continue,
        };
        if let Some(rt) = t.types.get(&key_of(&expr)).and_then(|x| x.rt.clone()) {
            let tt = type_text(Some(&rt));
            let at = Pos {
                line: r.el,
                character: r.ec + if hidden { 1 } else { 0 },
            };
            one(
                format!("annotate: {tt}"),
                "refactor.rewrite",
                vec![insert_json(at, &format!(": {tt}"))],
            );
        }
        break;
    }
    // convert a member's kind: derived <-> hidden, defaulted <-> derived, optional <-> required
    let member = chain.iter().find_map(|n| match n {
        NodeRef::Member(
            mm @ (MemberAst::Derived { loc: Some(_), .. } | MemberAst::Value { loc: Some(_), .. }),
        ) => Some(*mm),
        _ => None,
    });
    if let Some(mm) = member {
        let name = mm.name().unwrap_or("");
        let r = member_range(&text, mm, name);
        let after_name = Pos {
            line: r.el,
            character: r.ec,
        };
        let remove_next = edit_json(
            Loc {
                sl: r.el,
                sc: r.ec,
                el: r.el,
                ec: r.ec + 1,
            },
            "",
        );
        match mm {
            MemberAst::Derived { hidden, ty, .. } => {
                if *hidden {
                    one(
                        "make visible (derived)".into(),
                        "refactor.rewrite",
                        vec![remove_next.clone()],
                    );
                } else {
                    one(
                        "make hidden (x$)".into(),
                        "refactor.rewrite",
                        vec![insert_json(after_name, "$")],
                    );
                }
                if ty.is_some() {
                    one(
                        "make defaulted (x?: T = e)".into(),
                        "refactor.rewrite",
                        vec![insert_json(after_name, "?")],
                    );
                }
            }
            MemberAst::Value { dflt: Some(_), .. } => one(
                "make derived (x: T = e)".into(),
                "refactor.rewrite",
                vec![remove_next],
            ),
            MemberAst::Value { opt: true, .. } => one(
                "make required".into(),
                "refactor.rewrite",
                vec![remove_next],
            ),
            _ => one(
                "make optional".into(),
                "refactor.rewrite",
                vec![insert_json(after_name, "?")],
            ),
        }
    }
    // generate: export, an output or input skeleton for a type, the fixture header
    let decl = chain.iter().find_map(|n| match n {
        NodeRef::Decl(d) => Some(*d),
        _ => None,
    });
    if let Some(d) = decl {
        if let (Some(l), Some(name), false) = (d.loc, d.name(), d.exported) {
            if !matches!(d.body, DeclBody::Import { .. } | DeclBody::ReExport { .. }) {
                one(
                    format!("export {name}"),
                    "refactor.rewrite",
                    vec![insert_json(
                        Pos {
                            line: l.sl,
                            character: l.sc,
                        },
                        "export ",
                    )],
                );
            }
        }
        if let DeclBody::Type { name, ty, .. } = &d.body {
            if let Ok(rt) = m.env.resolve(ty, None) {
                if is_rec(&rt) {
                    let req: Vec<String> = rec_members(&rt)
                        .iter()
                        .filter(|x| x.kind == MKind::Req)
                        .map(|x| format!("{}: {}", x.name, placeholder_for(x.ty.as_ref())))
                        .collect();
                    let last = lines.last().copied().unwrap_or("");
                    let end = Pos {
                        line: lines.len() - 1,
                        character: u16len(last),
                    };
                    let lead = if last.is_empty() { "" } else { "\n" };
                    let mut chars = name.chars();
                    let lower = match chars.next() {
                        Some(c) => format!("{}{}", c.to_lowercase(), chars.as_str()),
                        None => String::new(),
                    };
                    one(
                        format!("generate an output of {name}"),
                        "refactor.rewrite",
                        vec![insert_json(
                            end,
                            &format!(
                                "{lead}output {lower}: {name} = {{ {}{}}}\n",
                                req.join(", "),
                                if req.is_empty() { "" } else { " " }
                            ),
                        )],
                    );
                    one(
                        format!("generate an input of {name}"),
                        "refactor.rewrite",
                        vec![insert_json(end, &format!("{lead}input {lower}: {name}\n"))],
                    );
                }
            }
        }
    }
    if !diagnostics.is_empty()
        && !lines
            .first()
            .copied()
            .unwrap_or("")
            .starts_with("// @expect-")
    {
        let first = diagnostics
            .iter()
            .find(|d| matches!(get(d, "severity"), Some(Value::Int(i)) if i.to_string() == "1"))
            .unwrap_or(&diagnostics[0]);
        let code = match get(first, "code") {
            Some(Value::Str(c)) => c.clone(),
            Some(Value::Int(i)) => i.to_string(),
            Some(Value::Float(f)) => crate::semantics::js_num_str(*f),
            _ => String::new(),
        };
        let assert_id = Regex::new(r"^[A-Z][A-Za-z0-9_]*\.").unwrap();
        let phase = if code.starts_with("E1") || code.starts_with("E2") {
            "parsing"
        } else if code.starts_with("E5") || code.starts_with("E6") || assert_id.is_match(&code) {
            "binding"
        } else {
            "checking"
        };
        one(
            "generate the fixture header (@expect-phase / @expect-error)".into(),
            "refactor.rewrite",
            vec![insert_json(
                Pos {
                    line: 0,
                    character: 0,
                },
                &format!("// @expect-phase: {phase}\n// @expect-error: {code}\n"),
            )],
        );
    }
    // fill the missing required members of a literal
    if let Some(obj) = chain_expr(&chain, |x| matches!(x, Expr::Obj(_))) {
        if let (Some(ol), Expr::Obj(entries)) = (expr_loc(&obj), &*obj) {
            let owner = chain.iter().find_map(|n| match n {
                NodeRef::Decl(d) => match &d.body {
                    DeclBody::Output { ty, expr, .. } if Rc::ptr_eq(expr, &obj) => Some(ty.clone()),
                    DeclBody::Input { ty, .. } => Some(ty.clone()),
                    _ => None,
                },
                _ => None,
            });
            // the reference: `owner && owner.expr === obj` — an input's fallback is never `expr`
            let owner = match (
                &owner,
                chain.iter().find_map(|n| match n {
                    NodeRef::Decl(d) => Some(*d),
                    _ => None,
                }),
            ) {
                (Some(_), Some(d)) if matches!(&d.body, DeclBody::Input { .. }) => None,
                _ => owner,
            };
            let rt: Option<RT> = match owner {
                Some(ty) => m.env.resolve(&ty, None).ok(),
                None => t.types.get(&key_of(&obj)).and_then(|x| x.rt.clone()),
            };
            if let Some(rt) = rt.filter(is_rec) {
                let have: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
                let missing: Vec<crate::semantics::Member> = rec_members(&rt)
                    .into_iter()
                    .filter(|x| x.kind == MKind::Req && !have.contains(&x.name.as_str()))
                    .collect();
                if !missing.is_empty() {
                    let fill = missing
                        .iter()
                        .map(|x| format!("{}: {}", x.name, placeholder_for(x.ty.as_ref())))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let edit = match entries.last().and_then(|(_, v)| expr_loc(v)) {
                        Some(vl) => insert_json(
                            Pos {
                                line: vl.el,
                                character: vl.ec,
                            },
                            &format!(", {fill}"),
                        ),
                        None => insert_json(
                            Pos {
                                line: ol.sl,
                                character: ol.sc + 1,
                            },
                            &format!(" {fill}"),
                        ),
                    };
                    one(
                        format!(
                            "fill the required members: {}",
                            missing
                                .iter()
                                .map(|x| x.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        "refactor.rewrite",
                        vec![edit],
                    );
                }
            }
        }
    }
    // inline a constant: its expression at every use, the declaration gone
    if let Some(d) = decl {
        if let (DeclBody::Const { name, expr, .. }, Some(dl), Some(el)) =
            (&d.body, d.loc, d.body.const_expr_loc())
        {
            let nr = name_range(&text, d, name);
            let refs: Vec<Loc> = references(
                st,
                uri,
                Pos {
                    line: nr.sl,
                    character: nr.sc,
                },
                false,
            )
            .into_iter()
            .filter(|(rm, _)| rm.path == m.path)
            .map(|(_, l)| l)
            .collect();
            if !refs.is_empty() {
                let src = src_of(&text, el);
                let plain = matches!(
                    &**expr,
                    Expr::Name(_)
                        | Expr::Lit(_)
                        | Expr::UnitLit { .. }
                        | Expr::Call { .. }
                        | Expr::Member { .. }
                        | Expr::Paren(_)
                );
                let new_text = if plain {
                    src.clone()
                } else {
                    format!("({src})")
                };
                let mut edits: Vec<J> = refs.iter().map(|l| edit_json(*l, &new_text)).collect();
                edits.push(edit_json(
                    Loc {
                        sl: dl.sl,
                        sc: 0,
                        el: dl.el + 1,
                        ec: 0,
                    },
                    "",
                ));
                one(format!("inline {name}"), "refactor.inline", edits);
            }
        }
    }
    // extract an inline record type into a named type
    let own_body: Option<*const TypeAst> =
        decl.and_then(record_body_of).map(|t| t as *const TypeAst);
    let inline_record: Option<Loc> = chain
        .iter()
        .find_map(|n| match n {
            NodeRef::Type(t @ TypeAst::Record { loc: Some(l), .. })
                if own_body != Some(*t as *const TypeAst) =>
            {
                Some(*l)
            }
            _ => None,
        })
        .or_else(|| {
            chain.iter().find_map(|n| match n {
                NodeRef::Member(MemberAst::Value {
                    ty: TypeAst::Record { loc: Some(l), .. },
                    ..
                }) => Some(*l),
                _ => None,
            })
        });
    if let (Some(rl), Some(dl)) = (inline_record, decl.and_then(|d| d.loc)) {
        one(
            "extract to a named type".into(),
            "refactor.extract",
            vec![
                insert_json(
                    Pos {
                        line: dl.sl,
                        character: 0,
                    },
                    &format!("type Extracted = {}\n", src_of(&text, rl)),
                ),
                edit_json(rl, "Extracted"),
            ],
        );
    }
    // a unit literal in its base unit
    if let Some(ul) = chain_expr(&chain, |x| matches!(x, Expr::UnitLit { .. })) {
        if let (Expr::UnitLit { num, unit }, Some(l)) = (&*ul, expr_loc(&ul)) {
            if let Ok((key, to_base)) = m.env.unit_info(unit) {
                let base = m
                    .env
                    .base_unit_of
                    .borrow()
                    .get(&key)
                    .cloned()
                    .unwrap_or(key);
                if base != *unit {
                    let converted =
                        format!("{}{base}", crate::semantics::js_num_str(num * to_base));
                    one(
                        format!("convert to {converted}"),
                        "refactor.rewrite",
                        vec![edit_json(l, &converted)],
                    );
                }
            }
        }
    }
    // reorder a record's members into the canonical order
    let type_decl = chain.iter().find_map(|n| match n {
        NodeRef::Decl(d) if matches!(d.body, DeclBody::Type { .. }) => Some(*d),
        _ => None,
    });
    if let Some((members, bl, open)) = type_decl.and_then(record_body_of).and_then(record_parts) {
        if members.len() > 1 && members.iter().all(|x| x.loc().is_some()) {
            let rank = |x: &MemberAst| match x {
                MemberAst::Context { .. } => 0,
                MemberAst::Value { dflt: Some(_), .. } => 3,
                MemberAst::Value { opt: true, .. } => 2,
                MemberAst::Value { .. } => 1,
                MemberAst::Derived { hidden: true, .. } => 5,
                MemberAst::Derived { .. } => 4,
                _ => 6,
            };
            let mut sorted: Vec<(usize, &MemberAst)> = members.iter().enumerate().collect();
            sorted.sort_by_key(|(i, x)| (rank(x), *i));
            if sorted.iter().enumerate().any(|(i, (j, _))| i != *j) {
                let lead = leading_spaces(lines.get(bl.sl).copied().unwrap_or(""));
                let indent = format!("{lead}    ");
                let body: Vec<String> = sorted
                    .iter()
                    .map(|(_, x)| {
                        format!(
                            "{indent}{}",
                            trailing_comma.replace(&src_of(&text, x.loc().unwrap()), "")
                        )
                    })
                    .collect();
                let trailing = if open {
                    format!("\n{indent}...")
                } else {
                    String::new()
                };
                one(
                    "reorder the members canonically".into(),
                    "refactor.rewrite",
                    vec![edit_json(
                        bl,
                        &format!("{{\n{}{trailing}\n{lead}}}", body.join("\n")),
                    )],
                );
            }
        }
    }
    // an assert's inline `else error …` into a diagnostic declaration, and back
    let assert_member = chain.iter().find_map(|n| match n {
        NodeRef::Member(mm @ MemberAst::Assert { loc: Some(_), .. }) => Some(*mm),
        _ => None,
    });
    if let (
        Some(MemberAst::Assert {
            name: aname,
            tail: Some(tail),
            loc: Some(al),
            ..
        }),
        Some(tdl),
    ) = (assert_member, type_decl.and_then(|d| d.loc))
    {
        let asrc = src_of(&text, *al);
        if let Some(em) = Regex::new(r"\belse\b").unwrap().find(&asrc) {
            let (mut line, mut col) = (al.sl, al.sc);
            for u in asrc[..em.start()].encode_utf16() {
                if u == '\n' as u16 {
                    line += 1;
                    col = 0;
                } else {
                    col += 1;
                }
            }
            let tail_loc = Loc {
                sl: line,
                sc: col,
                el: al.el,
                ec: al.ec,
            };
            match tail {
                Tail::Inline { severity, template } => {
                    // the names the template reads become the parameters, typed as inferred
                    let mut names: Vec<String> = vec![];
                    let mut params: Vec<String> = vec![];
                    for part in template {
                        if let TPart::Expr(e) = part {
                            for n in exprs_under(&NodeRef::Expr(e)) {
                                if let Expr::Name(nm) = &*n {
                                    if !names.contains(nm) {
                                        names.push(nm.clone());
                                        let ty = t
                                            .types
                                            .get(&key_of(&n))
                                            .and_then(|x| x.rt.as_ref())
                                            .map(|rt| crate::infer::type_text(Some(rt)))
                                            .unwrap_or_else(|| "any".to_string());
                                        params.push(format!("{nm}: {ty}"));
                                    }
                                }
                            }
                        }
                    }
                    let tmpl = template
                        .iter()
                        .map(|p| match p {
                            TPart::Text(s) => s.clone(),
                            TPart::Expr(e) => format!("${{{}}}", crate::session::expr_text(e)),
                        })
                        .collect::<String>();
                    one(format!("declare a diagnostic for {aname}"), "refactor.extract", vec![
                        insert_json(Pos { line: tdl.sl, character: 0 }, &format!("diagnostic {aname}({}) {{\n    severity = {severity}\n    message = `{tmpl}`\n}}\n", params.join(", "))),
                        edit_json(tail_loc, &format!("else {aname}({})", names.join(", "))),
                    ]);
                }
                Tail::Ref { name: dname, args } => {
                    let dd = m.env.diags.borrow().get(dname).cloned();
                    let declared = m.decls.iter().any(
                        |d| matches!(&d.body, DeclBody::Diagnostic { name, .. } if name == dname),
                    );
                    if let (Some(dd), true) = (dd, declared) {
                        let arg_text: Vec<String> = args
                            .iter()
                            .map(|a| {
                                expr_loc(a)
                                    .map(|l| src_of(&text, l))
                                    .unwrap_or_else(|| crate::session::expr_text(a))
                            })
                            .collect();
                        let mut message = String::from("`");
                        for p in &dd.template {
                            match p {
                                TPart::Text(s) => message.push_str(s),
                                TPart::Expr(e) => {
                                    let i = match &**e {
                                        Expr::Name(nm) => {
                                            dd.params.iter().position(|q| &q.name == nm)
                                        }
                                        _ => None,
                                    };
                                    message.push_str("${");
                                    message.push_str(
                                        &i.and_then(|i| arg_text.get(i).cloned())
                                            .unwrap_or_else(|| crate::session::expr_text(e)),
                                    );
                                    message.push('}');
                                }
                            }
                        }
                        message.push('`');
                        one(
                            format!("inline the diagnostic {dname}"),
                            "refactor.inline",
                            vec![edit_json(
                                tail_loc,
                                &format!("else {} {message}", dd.severity),
                            )],
                        );
                    }
                }
            }
        }
    }
    // an `if` chain over a discriminant into a `match`, and back
    if let Some(if_expr) = chain_expr(&chain, |x| matches!(x, Expr::If { .. })) {
        if let Some(il) = expr_loc(&if_expr) {
            // conditions `subject.member == "lit"` on one subject and member, the union's arms telling which record each literal picks
            let mut arms: Vec<(String, Rc<Expr>)> = vec![];
            let mut subject: Option<Rc<Expr>> = None;
            let mut member: Option<String> = None;
            let mut tail_expr: Option<Rc<Expr>> = None;
            let mut ok = true;
            let mut cur = if_expr.clone();
            loop {
                let (c, tt, f) = match &*cur {
                    Expr::If { c, t, f } => (c.clone(), t.clone(), f.clone()),
                    _ => {
                        tail_expr = Some(cur.clone());
                        break;
                    }
                };
                let mut matched = false;
                if let Expr::Bin { op, l, r } = &*c {
                    if op == "==" {
                        if let (Expr::Member { x, name, .. }, Expr::Lit(Value::Str(lit))) =
                            (&**l, &**r)
                        {
                            match &subject {
                                None => {
                                    subject = Some(x.clone());
                                    member = Some(name.clone());
                                }
                                Some(s) => {
                                    if crate::session::expr_text(s) != crate::session::expr_text(x)
                                        || member.as_deref() != Some(name.as_str())
                                    {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            arms.push((lit.clone(), tt.clone()));
                            matched = true;
                        }
                    }
                }
                if !matched {
                    ok = false;
                    break;
                }
                cur = f;
            }
            let srt = subject
                .as_ref()
                .and_then(|s| t.types.get(&key_of(s)))
                .and_then(|x| x.rt.clone());
            if let (true, false, Some(subj), Some(mem), Some(srt)) = (
                ok,
                arms.is_empty(),
                subject.as_ref(),
                member.as_ref(),
                srt.as_ref(),
            ) {
                if let RTk::Union(uarms) = &srt.k {
                    let arm_name = |lit: &str| -> Option<String> {
                        uarms.iter()
                            .find(|r| matches!(r.k, RTk::Rec(_)) && rec_members(r).iter().any(|mm| &mm.name == mem && matches!(mm.ty.as_ref().map(|t| &t.k), Some(RTk::Lit(Value::Str(v))) if v == lit)))
                            .and_then(|r| r.name.borrow().clone())
                    };
                    let names: Vec<Option<String>> =
                        arms.iter().map(|(lit, _)| arm_name(lit)).collect();
                    if names.iter().all(|n| n.is_some())
                        && arms.iter().all(|(_, b)| expr_loc(b).is_some())
                    {
                        let lead = leading_spaces(lines.get(il.sl).copied().unwrap_or(""));
                        let indent = format!("{lead}    ");
                        let subj_text = crate::session::expr_text(subj);
                        let v: String = match &**subj {
                            Expr::Name(n) => n
                                .chars()
                                .next()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "v".into()),
                            _ => "v".into(),
                        };
                        let subj_re =
                            Regex::new(&format!(r"\b{}\b", regex::escape(&subj_text))).unwrap();
                        let mut cases: Vec<String> = arms
                            .iter()
                            .zip(names.iter())
                            .map(|((_, body), n)| {
                                format!(
                                    "{indent}({v}: {}) => {}",
                                    n.as_ref().unwrap(),
                                    subj_re.replace_all(
                                        &src_of(&text, expr_loc(body).unwrap()),
                                        v.as_str()
                                    )
                                )
                            })
                            .collect();
                        if let Some(tl) = tail_expr.as_ref().and_then(expr_loc) {
                            cases.push(format!("{indent}(other) => {}", src_of(&text, tl)));
                        }
                        one(
                            "convert to match".into(),
                            "refactor.rewrite",
                            vec![edit_json(
                                il,
                                &format!("match {subj_text} {{\n{}\n{lead}}}", cases.join("\n")),
                            )],
                        );
                    }
                }
            }
        }
    }
    if let Some(mx) = chain_expr(&chain, |x| matches!(x, Expr::Match { .. })) {
        if let (Expr::Match { subject, arms }, Some(ml)) = (&*mx, expr_loc(&mx)) {
            // arms typed by records that a literal member discriminates: `if subject.kind == "lit" then … else …`
            let srt = t.types.get(&key_of(subject)).and_then(|x| x.rt.clone());
            let recs: Vec<RT> = match srt.as_ref().map(|r| &r.k) {
                Some(RTk::Union(us)) => us
                    .iter()
                    .filter(|r| matches!(r.k, RTk::Rec(_)))
                    .cloned()
                    .collect(),
                _ => vec![],
            };
            let is_lit = |m: &crate::semantics::Member| {
                matches!(m.ty.as_ref().map(|t| &t.k), Some(RTk::Lit(_)))
            };
            let disc: Option<String> = recs.first().and_then(|r0| {
                rec_members(r0)
                    .into_iter()
                    .find(|mm| {
                        is_lit(mm)
                            && recs.iter().all(|r| {
                                rec_members(r)
                                    .iter()
                                    .any(|x| x.name == mm.name && is_lit(x))
                            })
                    })
                    .map(|mm| mm.name)
            });
            if let Some(disc) = disc {
                let subj_text = crate::session::expr_text(subject);
                let mut parts: Vec<String> = vec![];
                let mut fallback: Option<String> = None;
                let mut ok = true;
                for arm in arms {
                    let Some(bl) = expr_loc(&arm.body) else {
                        ok = false;
                        break;
                    };
                    let var_re = Regex::new(&format!(r"\b{}\b", regex::escape(&arm.v))).unwrap();
                    let body = var_re
                        .replace_all(&src_of(&text, bl), subj_text.as_str())
                        .to_string();
                    let rec = match &arm.ty {
                        Some(TypeAst::Named { name, .. }) => recs
                            .iter()
                            .find(|r| r.name.borrow().as_deref() == Some(name.as_str())),
                        _ => None,
                    };
                    let lit = rec
                        .and_then(|r| rec_members(r).into_iter().find(|x| x.name == disc))
                        .and_then(|x| x.ty)
                        .and_then(|ty| match &ty.k {
                            RTk::Lit(Value::Str(s)) => Some(s.clone()),
                            _ => None,
                        });
                    match lit {
                        Some(l) => parts.push(format!(
                            "if {subj_text}.{disc} == {} then {body}",
                            json_str(&l)
                        )),
                        None => fallback = Some(body),
                    }
                }
                if ok && !parts.is_empty() {
                    one(
                        "convert to if".into(),
                        "refactor.rewrite",
                        vec![edit_json(
                            ml,
                            &format!(
                                "{} else {}",
                                parts.join(" else "),
                                fallback.unwrap_or_else(|| "null".into())
                            ),
                        )],
                    );
                }
            }
        }
    }
    // inline a derived member into its sibling uses
    let derived = chain.iter().find_map(|n| match n {
        NodeRef::Member(
            mm @ MemberAst::Derived {
                loc: Some(_), expr, ..
            },
        ) if expr_loc(expr).is_some() => Some(*mm),
        _ => None,
    });
    if let (
        Some(
            dm @ MemberAst::Derived {
                name: dname,
                expr: dexpr,
                loc: Some(dl),
                ..
            },
        ),
        Some((members, _, _)),
    ) = (
        derived,
        type_decl.and_then(record_body_of).and_then(record_parts),
    ) {
        let mut uses: Vec<Loc> = vec![];
        for other in members {
            if std::ptr::eq(other, dm) {
                continue;
            }
            for e in exprs_under(&NodeRef::Member(other)) {
                if let Expr::Name(nm) = &*e {
                    if nm == dname {
                        if let Some(l) = expr_loc(&e) {
                            uses.push(l);
                        }
                    }
                }
            }
        }
        if !uses.is_empty() {
            let src = src_of(&text, expr_loc(dexpr).unwrap());
            let plain = matches!(
                &**dexpr,
                Expr::Name(_)
                    | Expr::Lit(_)
                    | Expr::UnitLit { .. }
                    | Expr::Call { .. }
                    | Expr::Member { .. }
                    | Expr::Paren(_)
            );
            let wrapped = if plain { src } else { format!("({src})") };
            let mut edits: Vec<J> = uses.iter().map(|l| edit_json(*l, &wrapped)).collect();
            edits.push(edit_json(
                Loc {
                    sl: dl.sl,
                    sc: 0,
                    el: dl.el + 1,
                    ec: 0,
                },
                "",
            ));
            one(format!("inline {dname}"), "refactor.inline", edits);
        }
    }
    // flip the operands of a comparison
    let cmp = chain_expr(
        &chain,
        |x| matches!(x, Expr::Bin { op, l, r } if ["<", ">", "<=", ">=", "==", "!="].contains(&op.as_str()) && expr_loc(l).is_some() && expr_loc(r).is_some()),
    );
    if let Some(c) = cmp {
        if let (Expr::Bin { op, l, r }, Some(cl)) = (&*c, expr_loc(&c)) {
            let flipped = match op.as_str() {
                "<" => ">",
                ">" => "<",
                "<=" => ">=",
                ">=" => "<=",
                other => other,
            };
            one(
                "flip the comparison".into(),
                "refactor.rewrite",
                vec![edit_json(
                    cl,
                    &format!(
                        "{} {} {}",
                        src_of(&text, expr_loc(r).unwrap()),
                        flipped,
                        src_of(&text, expr_loc(l).unwrap())
                    ),
                )],
            );
        }
    }
    // extract the selected expression: into a constant (a constant expression), or a derived member (inside a record body)
    let selected = if range.0.line != range.1.line || range.0.character != range.1.character {
        chain_expr(&chain, |_| true).and_then(|_| {
            chain.iter().find_map(|n| match n {
                NodeRef::Expr(e) => expr_loc(e)
                    .filter(|l| {
                        l.sl == range.0.line
                            && l.sc == range.0.character
                            && l.el == range.1.line
                            && l.ec == range.1.character
                    })
                    .map(|_| (*e).clone()),
                _ => None,
            })
        })
    } else {
        None
    };
    if let Some(sel) = selected.filter(|e| !matches!(&**e, Expr::Name(_))) {
        let src = src_of(&text, expr_loc(&sel).unwrap());
        let enclosing_member = chain.iter().find_map(|n| match n {
            NodeRef::Member(mm) if mm.loc().is_some() => Some(*mm),
            _ => None,
        });
        let enclosing_decl = chain.iter().find_map(|n| match n {
            NodeRef::Decl(d) => Some(*d),
            _ => None,
        });
        if let (false, Some(dl)) = (mentions_name(&sel), enclosing_decl.and_then(|d| d.loc)) {
            one(
                "extract to a constant".into(),
                "refactor.extract",
                vec![
                    insert_json(
                        Pos {
                            line: dl.sl,
                            character: 0,
                        },
                        &format!("const extracted = {src}\n"),
                    ),
                    edit_json(expr_loc(&sel).unwrap(), "extracted"),
                ],
            );
        }
        if let Some(mm) = enclosing_member.filter(|mm| !matches!(mm, MemberAst::Context { .. })) {
            let ml = mm.loc().unwrap();
            let indent = leading_spaces(lines.get(ml.sl).copied().unwrap_or(""));
            one(
                "extract to a derived member".into(),
                "refactor.extract",
                vec![
                    insert_json(
                        Pos {
                            line: ml.sl,
                            character: 0,
                        },
                        &format!("{indent}extracted = {src}\n"),
                    ),
                    edit_json(expr_loc(&sel).unwrap(), "extracted"),
                ],
            );
        }
    }
    J::Arr(out)
}

// ---------------- local variables: linked editing, rename ----------------
// a comprehension variable, a lambda parameter, a match arm's variable, or
// a function parameter: its binding token and its uses in its scope
fn binds_name(n: &NodeRef, name: &str) -> bool {
    match n {
        NodeRef::Expr(e) => match &***e {
            Expr::Comp { clauses, .. } | Expr::MapComp { clauses, .. } => {
                clauses.iter().any(|c| c.v == name)
            }
            Expr::Lambda { params, .. } => params.iter().any(|p| p == name),
            Expr::Match { arms, .. } => arms.iter().any(|a| a.v == name),
            _ => false,
        },
        NodeRef::Decl(d) => {
            matches!(&d.body, DeclBody::Func { params, .. } if params.iter().any(|p| p.name == name))
        }
        _ => false,
    }
}
fn expr_binds_name(e: &Rc<Expr>, name: &str) -> bool {
    binds_name(&NodeRef::Expr(e), name)
}
// an offset into the scope's source text (UTF-16 units) -> a Loc of `name`
fn loc_in(scope: Loc, src: &str, byte: usize, name: &str) -> Loc {
    let before = &src[..byte];
    let (line, col) = match before.rfind('\n') {
        Some(nl) => (
            scope.sl + before.matches('\n').count(),
            u16len(&before[nl + 1..]),
        ),
        None => (scope.sl, scope.sc + u16len(before)),
    };
    Loc {
        sl: line,
        sc: col,
        el: line,
        ec: col + u16len(name),
    }
}
fn binding_locs(text: &str, scope: &NodeRef, scope_loc: Loc, name: &str) -> Vec<Loc> {
    let src = src_of(text, scope_loc);
    let esc = regex::escape(name);
    let mut out: Vec<Loc> = vec![];
    match scope {
        NodeRef::Decl(_) => {
            let re = Regex::new(&format!(r"\(([^)]*)(?-u:\b)({esc})(?-u:\b)")).unwrap();
            if let Some(m) = re.find(&src) {
                // the pattern ends with the name itself: the token is the tail of the match
                out.push(loc_in(scope_loc, &src, m.end() - name.len(), name));
            }
        }
        NodeRef::Expr(e) if matches!(&***e, Expr::Lambda { .. }) => {
            // `\b(name)\b(?=[^=]*=>)`: the first whole-word occurrence whose next `=` opens `=>`
            let re = Regex::new(&format!(r"(?-u:\b)({esc})(?-u:\b)")).unwrap();
            for m in re.find_iter(&src) {
                let rest = &src[m.end()..];
                let ok = match rest.find('=') {
                    Some(i) => rest[i..].starts_with("=>"),
                    None => false,
                };
                if ok {
                    out.push(loc_in(scope_loc, &src, m.start(), name));
                    break;
                }
            }
        }
        NodeRef::Expr(e) if matches!(&***e, Expr::Match { .. }) => {
            let re = Regex::new(&format!(r"\(\s*({esc})(?-u:\b)")).unwrap();
            for m in re.find_iter(&src) {
                out.push(loc_in(scope_loc, &src, m.end() - name.len(), name));
            }
        }
        _ => {
            let re = Regex::new(&format!(r"(?-u:\b)for\s+({esc})(?-u:\b)")).unwrap();
            for m in re.find_iter(&src) {
                out.push(loc_in(scope_loc, &src, m.end() - name.len(), name));
            }
        }
    }
    out
}
// the uses of a local in its scope: name expressions, shadowing scopes skipped
fn uses_in_expr(e: &Rc<Expr>, name: &str, scope_key: usize, out: &mut Vec<Loc>) {
    if key_of(e) != scope_key && expr_binds_name(e, name) {
        return;
    }
    match &**e {
        Expr::Name(n) => {
            if n == name {
                if let Some(l) = expr_loc(e) {
                    out.push(l);
                }
            }
        }
        Expr::Template(parts) => {
            for p in parts {
                if let TPart::Expr(x) = p {
                    uses_in_expr(x, name, scope_key, out);
                }
            }
        }
        Expr::Obj(entries) => {
            for (_, v) in entries {
                uses_in_expr(v, name, scope_key, out);
            }
        }
        Expr::Arr(items) => {
            for (_, v) in items {
                uses_in_expr(v, name, scope_key, out);
            }
        }
        Expr::Comp { head, clauses } => {
            uses_in_expr(head, name, scope_key, out);
            for c in clauses {
                uses_in_expr(&c.iter, name, scope_key, out);
                for f in &c.filters {
                    uses_in_expr(f, name, scope_key, out);
                }
            }
        }
        Expr::MapComp { key, val, clauses } => {
            uses_in_expr(key, name, scope_key, out);
            uses_in_expr(val, name, scope_key, out);
            for c in clauses {
                uses_in_expr(&c.iter, name, scope_key, out);
                for f in &c.filters {
                    uses_in_expr(f, name, scope_key, out);
                }
            }
        }
        Expr::Bin { l, r, .. } => {
            uses_in_expr(l, name, scope_key, out);
            uses_in_expr(r, name, scope_key, out);
        }
        Expr::Un { x, .. } | Expr::Paren(x) => uses_in_expr(x, name, scope_key, out),
        Expr::If { c, t, f } => {
            uses_in_expr(c, name, scope_key, out);
            uses_in_expr(t, name, scope_key, out);
            uses_in_expr(f, name, scope_key, out);
        }
        Expr::Lambda { body, .. } => uses_in_expr(body, name, scope_key, out),
        Expr::Call { fun, args } => {
            uses_in_expr(fun, name, scope_key, out);
            for a in args {
                uses_in_expr(a, name, scope_key, out);
            }
        }
        Expr::Member { x, .. } => uses_in_expr(x, name, scope_key, out),
        Expr::Index { x, i } => {
            uses_in_expr(x, name, scope_key, out);
            uses_in_expr(i, name, scope_key, out);
        }
        Expr::With { base, patch } => {
            uses_in_expr(base, name, scope_key, out);
            uses_in_expr(patch, name, scope_key, out);
        }
        Expr::Match { subject, arms } => {
            uses_in_expr(subject, name, scope_key, out);
            for a in arms {
                if let Some(t) = &a.ty {
                    uses_in_type(t, name, scope_key, out);
                }
                uses_in_expr(&a.body, name, scope_key, out);
            }
        }
        _ => {}
    }
}
fn uses_in_type(t: &TypeAst, name: &str, scope_key: usize, out: &mut Vec<Loc>) {
    match t {
        TypeAst::Record { members, .. } => {
            for m in members {
                uses_in_member(m, name, scope_key, out);
            }
        }
        TypeAst::Map { key, val, .. } => {
            uses_in_type(key, name, scope_key, out);
            uses_in_type(val, name, scope_key, out);
        }
        TypeAst::Array { elem, .. } => uses_in_type(elem, name, scope_key, out),
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => {
            for a in arms {
                uses_in_type(a, name, scope_key, out);
            }
        }
        TypeAst::Func { params, ret, .. } => {
            for a in params {
                uses_in_type(a, name, scope_key, out);
            }
            uses_in_type(ret, name, scope_key, out);
        }
        TypeAst::Named {
            args, preds, ext, ..
        } => {
            for a in args {
                uses_in_type(a, name, scope_key, out);
            }
            for x in preds.iter().flatten() {
                uses_in_expr(x, name, scope_key, out);
            }
            if let Some(x) = ext {
                uses_in_type(x, name, scope_key, out);
            }
        }
        _ => {}
    }
}
fn uses_in_member(m: &MemberAst, name: &str, scope_key: usize, out: &mut Vec<Loc>) {
    match m {
        MemberAst::Value { ty, dflt, .. } => {
            uses_in_type(ty, name, scope_key, out);
            if let Some(d) = dflt {
                uses_in_expr(d, name, scope_key, out);
            }
        }
        MemberAst::Derived { ty, expr, .. } => {
            if let Some(t) = ty {
                uses_in_type(t, name, scope_key, out);
            }
            uses_in_expr(expr, name, scope_key, out);
        }
        MemberAst::Context { ty, .. } => uses_in_type(ty, name, scope_key, out),
        MemberAst::Assert { cond, tail, .. } => {
            uses_in_expr(cond, name, scope_key, out);
            if let Some(t) = tail {
                uses_in_tail(t, name, scope_key, out);
            }
        }
        MemberAst::When { cond, body, .. } => {
            uses_in_expr(cond, name, scope_key, out);
            for b in body {
                uses_in_member(b, name, scope_key, out);
            }
        }
    }
}
fn uses_in_tail(t: &Tail, name: &str, scope_key: usize, out: &mut Vec<Loc>) {
    match t {
        Tail::Inline { template, .. } => {
            for p in template {
                if let TPart::Expr(x) = p {
                    uses_in_expr(x, name, scope_key, out);
                }
            }
        }
        Tail::Ref { args, .. } => {
            for a in args {
                uses_in_expr(a, name, scope_key, out);
            }
        }
    }
}
fn local_ranges(st: &State, uri: &str, pos: Pos) -> Option<Vec<Loc>> {
    let text = st.text(uri)?.clone();
    let parsed = parse_source(&text);
    if !parsed.errors.is_empty() {
        return None;
    }
    let hit = node_at(&parsed.decls, pos)?;
    let mut chain: Vec<NodeRef> = vec![hit.node.clone()];
    chain.extend(hit.parents.iter().rev().cloned());
    let (name, scope): (String, Option<NodeRef>) = match &hit.node {
        NodeRef::Expr(e) if matches!(&***e, Expr::Name(_)) => {
            let Expr::Name(n) = &***e else { unreachable!() };
            let scope = hit.parents.iter().rev().find(|p| binds_name(p, n)).cloned();
            (n.clone(), scope)
        }
        _ => {
            // on a binding token: the scope node itself, the name under the cursor
            let line = text.split('\n').nth(pos.line).unwrap_or("");
            let re = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap();
            let mut found: Option<String> = None;
            for m in re.find_iter(line) {
                let a = u16_col(line, m.start());
                let b = a + u16len(m.as_str());
                if a <= pos.character && pos.character <= b {
                    found = Some(m.as_str().to_string());
                    break;
                }
            }
            let n = found?;
            let scope = chain.iter().find(|p| binds_name(p, &n)).cloned();
            (n, scope)
        }
    };
    let scope = scope?;
    let scope_loc = scope.loc()?;
    let mut locs = binding_locs(&text, &scope, scope_loc, &name);
    match &scope {
        NodeRef::Expr(e) => {
            // the scope's own children, the scope itself not counted as shadowing
            let key = key_of(e);
            match &***e {
                Expr::Comp { head, clauses } => {
                    uses_in_expr(head, &name, key, &mut locs);
                    for c in clauses {
                        uses_in_expr(&c.iter, &name, key, &mut locs);
                        for f in &c.filters {
                            uses_in_expr(f, &name, key, &mut locs);
                        }
                    }
                }
                Expr::MapComp {
                    key: k,
                    val,
                    clauses,
                } => {
                    uses_in_expr(k, &name, key, &mut locs);
                    uses_in_expr(val, &name, key, &mut locs);
                    for c in clauses {
                        uses_in_expr(&c.iter, &name, key, &mut locs);
                        for f in &c.filters {
                            uses_in_expr(f, &name, key, &mut locs);
                        }
                    }
                }
                Expr::Lambda { body, .. } => uses_in_expr(body, &name, key, &mut locs),
                Expr::Match { subject, arms } => {
                    uses_in_expr(subject, &name, key, &mut locs);
                    for a in arms {
                        if let Some(t) = &a.ty {
                            uses_in_type(t, &name, key, &mut locs);
                        }
                        uses_in_expr(&a.body, &name, key, &mut locs);
                    }
                }
                _ => {}
            }
        }
        NodeRef::Decl(d) => {
            if let DeclBody::Func {
                params, ret, body, ..
            } = &d.body
            {
                for p in params {
                    if let Some(t) = &p.ty {
                        uses_in_type(t, &name, 0, &mut locs);
                    }
                }
                if let Some(t) = ret {
                    uses_in_type(t, &name, 0, &mut locs);
                }
                uses_in_expr(body, &name, 0, &mut locs);
            }
        }
        _ => {}
    }
    let mut seen: Vec<(usize, usize)> = vec![];
    let mut out: Vec<Loc> = vec![];
    for l in locs {
        if !seen.contains(&(l.sl, l.sc)) {
            seen.push((l.sl, l.sc));
            out.push(l);
        }
    }
    out.sort_by(|p, q| p.sl.cmp(&q.sl).then(p.sc.cmp(&q.sc)));
    Some(out)
}
fn linked_editing_range(st: &State, uri: &str, pos: Pos) -> J {
    match local_ranges(st, uri, pos) {
        Some(locs) if !locs.is_empty() => J::obj(vec![
            (
                "ranges",
                J::Arr(locs.iter().map(|l| range_json(*l)).collect()),
            ),
            ("wordPattern", J::s("[A-Za-z_][A-Za-z0-9_]*")),
        ]),
        _ => J::Null,
    }
}

// ---------------- the syntax tree ----------------
// tree-sitter's own S-expression (`ts_node_string`): named nodes with
// their field names — what web-tree-sitter's `toString()` prints too
fn syntax_tree(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else {
        return J::Null;
    };
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = crate::parse::LANGUAGE.into();
    parser.set_language(&lang).expect("grammar");
    let tree = parser.parse(text, None).expect("parse");
    J::obj(vec![("tree", J::s(tree.root_node().to_sexp()))])
}

// ---------------- request handling ----------------
/// returns the exit code when the client asked to exit
fn handle(st: &mut State, msg: &Value) -> Option<i32> {
    // a message without a method is the client's response to a request of
    // ours (window/workDoneProgress/create): nothing to answer
    get(msg, "method")?;
    let id = get(msg, "id");
    let method = as_str(get(msg, "method")).unwrap_or("");
    let params = get(msg, "params");
    let td_uri = || {
        as_str(
            params
                .and_then(|p| get(p, "textDocument"))
                .and_then(|t| get(t, "uri")),
        )
        .unwrap_or("")
        .to_string()
    };
    let position = || {
        let pos = params.and_then(|p| get(p, "position"));
        Pos {
            line: as_usize(pos.and_then(|p| get(p, "line"))).unwrap_or(0),
            character: as_usize(pos.and_then(|p| get(p, "character"))).unwrap_or(0),
        }
    };
    let reanalyze = |st: &mut State| {
        st.analyses.clear();
        let uris: Vec<String> = st.docs.iter().map(|(u, _)| u.clone()).collect();
        for u in uris {
            analyze(st, &u);
        }
    };
    match method {
        "initialize" => {
            st.progress_supported = as_bool(
                params
                    .and_then(|p| get(p, "capabilities"))
                    .and_then(|c| get(c, "window"))
                    .and_then(|w| get(w, "workDoneProgress")),
            )
            .unwrap_or(false);
            let caps = J::obj(vec![
                ("textDocumentSync", J::Num(1)),
                ("hoverProvider", J::Bool(true)),
                ("definitionProvider", J::Bool(true)),
                ("typeDefinitionProvider", J::Bool(true)),
                ("referencesProvider", J::Bool(true)),
                ("documentHighlightProvider", J::Bool(true)),
                ("documentSymbolProvider", J::Bool(true)),
                ("foldingRangeProvider", J::Bool(true)),
                ("documentFormattingProvider", J::Bool(true)),
                (
                    "renameProvider",
                    J::obj(vec![("prepareProvider", J::Bool(true))]),
                ),
                (
                    "completionProvider",
                    J::obj(vec![(
                        "triggerCharacters",
                        J::Arr(vec![J::s("."), J::s("$"), J::s(":")]),
                    )]),
                ),
                (
                    "codeLensProvider",
                    J::obj(vec![("resolveProvider", J::Bool(false))]),
                ),
                (
                    "signatureHelpProvider",
                    J::obj(vec![(
                        "triggerCharacters",
                        J::Arr(vec![J::s("("), J::s(",")]),
                    )]),
                ),
                ("workspaceSymbolProvider", J::Bool(true)),
                ("selectionRangeProvider", J::Bool(true)),
                (
                    "semanticTokensProvider",
                    J::obj(vec![
                        (
                            "legend",
                            J::obj(vec![
                                (
                                    "tokenTypes",
                                    J::Arr(TOKEN_TYPES.iter().map(|t| J::s(*t)).collect()),
                                ),
                                (
                                    "tokenModifiers",
                                    J::Arr(TOKEN_MODS.iter().map(|t| J::s(*t)).collect()),
                                ),
                            ]),
                        ),
                        ("full", J::Bool(true)),
                    ]),
                ),
                ("inlayHintProvider", J::Bool(true)),
                ("callHierarchyProvider", J::Bool(true)),
                ("typeHierarchyProvider", J::Bool(true)),
                (
                    "codeActionProvider",
                    J::obj(vec![(
                        "codeActionKinds",
                        J::Arr(vec![
                            J::s("quickfix"),
                            J::s("refactor.rewrite"),
                            J::s("refactor.extract"),
                            J::s("refactor.inline"),
                        ]),
                    )]),
                ),
                ("linkedEditingRangeProvider", J::Bool(true)),
                (
                    "documentOnTypeFormattingProvider",
                    J::obj(vec![
                        ("firstTriggerCharacter", J::s("\n")),
                        (
                            "moreTriggerCharacter",
                            J::Arr(vec![J::s("}"), J::s("]"), J::s(")")]),
                        ),
                    ]),
                ),
                (
                    "executeCommandProvider",
                    J::obj(vec![(
                        "commands",
                        J::Arr(
                            [
                                "decl.evaluate",
                                "decl.validate",
                                "decl.trace",
                                "decl.showSyntaxTree",
                                "decl.reloadWorkspace",
                            ]
                            .iter()
                            .map(|c| J::s(*c))
                            .collect(),
                        ),
                    )]),
                ),
            ]);
            reply(
                id,
                J::obj(vec![
                    ("capabilities", caps),
                    (
                        "serverInfo",
                        J::obj(vec![
                            ("name", J::s("decl-lsp")),
                            ("version", J::s(env!("CARGO_PKG_VERSION"))),
                        ]),
                    ),
                ]),
            );
        }
        "initialized" => {}
        "workspace/didChangeConfiguration" => {
            let inputs = params
                .and_then(|p| get(p, "settings"))
                .and_then(|s| get(s, "decl"))
                .and_then(|d| get(d, "inputs"));
            st.inputs = match inputs {
                Some(Value::JObj(es)) => es
                    .iter()
                    .filter_map(|(k, v)| as_str(Some(v)).map(|f| (k.clone(), f.to_string())))
                    .collect(),
                _ => vec![],
            };
            let hints = params
                .and_then(|p| get(p, "settings"))
                .and_then(|s| get(s, "decl"))
                .and_then(|d| get(d, "inlayHints"));
            if let Some(b) = as_bool(hints.and_then(|h| get(h, "types"))) {
                st.hint_types = b;
            }
            if let Some(b) = as_bool(hints.and_then(|h| get(h, "parameterNames"))) {
                st.hint_parameter_names = b;
            }
            if let Some(b) = as_bool(hints.and_then(|h| get(h, "values"))) {
                st.hint_values = b;
            }
            if let Some(b) = as_bool(hints.and_then(|h| get(h, "units"))) {
                st.hint_units = b;
            }
            if let Some(b) = as_bool(hints.and_then(|h| get(h, "contextVariables"))) {
                st.hint_context_variables = b;
            }
            reanalyze(st);
        }
        "workspace/didChangeWatchedFiles" => reanalyze(st),
        "decl/files" => {
            // a browser client's workspace files (the reference's web worker): the host here is the
            // file system, so they become overlay texts — never written to disk
            if let Some(Value::JArr(files)) = params.and_then(|p| get(p, "files")) {
                for f in files.iter() {
                    if let (Some(u), Some(text)) = (as_str(get(f, "uri")), as_str(get(f, "text"))) {
                        st.overlay.insert(path_of(u), text.to_string());
                    }
                }
            }
            if let Some(Value::JArr(removed)) = params.and_then(|p| get(p, "remove")) {
                for u in removed.iter() {
                    if let Some(u) = as_str(Some(u)) {
                        st.overlay.remove(&path_of(u));
                    }
                }
            }
            reanalyze(st);
        }
        "textDocument/didOpen" => {
            let uri = td_uri();
            let text = as_str(
                params
                    .and_then(|p| get(p, "textDocument"))
                    .and_then(|t| get(t, "text")),
            )
            .unwrap_or("")
            .to_string();
            st.set(&uri, text);
            st.analyses.clear();
            analyze(st, &uri);
        }
        "textDocument/didChange" => {
            let uri = td_uri();
            let text = params
                .and_then(|p| get(p, "contentChanges"))
                .and_then(|c| {
                    if let Value::JArr(items) = c {
                        items.first()
                    } else {
                        None
                    }
                })
                .and_then(|c| as_str(get(c, "text")))
                .unwrap_or("")
                .to_string();
            st.set(&uri, text);
            st.analyses.clear();
            analyze(st, &uri);
        }
        "textDocument/didSave" => {}
        "textDocument/didClose" => {
            let uri = td_uri();
            st.docs.retain(|(u, _)| *u != uri);
            st.overlay.remove(&path_of(&uri));
            st.analyses.remove(&uri);
            st.last_good.remove(&uri);
            notify(
                "textDocument/publishDiagnostics",
                J::obj(vec![
                    ("uri", J::s(uri.clone())),
                    ("diagnostics", J::Arr(vec![])),
                ]),
            );
        }
        "textDocument/hover" => {
            let r = hover(st, &td_uri(), position());
            reply(id, r);
        }
        "textDocument/definition" => {
            let r = definition(st, &td_uri(), position());
            reply(id, r);
        }
        "textDocument/typeDefinition" => {
            let r = type_definition(st, &td_uri(), position());
            reply(id, r);
        }
        "textDocument/references" => {
            let incl = as_bool(
                params
                    .and_then(|p| get(p, "context"))
                    .and_then(|c| get(c, "includeDeclaration")),
            )
            .unwrap_or(false);
            let refs = references(st, &td_uri(), position(), incl);
            reply(
                id,
                J::Arr(refs.iter().map(|(m, l)| location(m, *l)).collect()),
            );
        }
        "textDocument/documentHighlight" => {
            let uri = td_uri();
            let path = path_of(&uri);
            let refs = references(st, &uri, position(), true);
            reply(
                id,
                J::Arr(
                    refs.iter()
                        .filter(|(m, _)| m.path == path)
                        .map(|(_, l)| J::obj(vec![("range", range_json(*l)), ("kind", J::Num(1))]))
                        .collect(),
                ),
            );
        }
        "textDocument/completion" => {
            let r = completion(st, &td_uri(), position());
            reply(id, r);
        }
        "textDocument/documentSymbol" => reply(id, document_symbols(st, &td_uri())),
        "textDocument/foldingRange" => reply(id, folding_ranges(st, &td_uri())),
        "textDocument/formatting" => reply(id, formatting(st, &td_uri())),
        "textDocument/prepareRename" => {
            let r = prepare_rename(st, &td_uri(), position());
            reply(id, r);
        }
        "textDocument/rename" => {
            let new_name = as_str(params.and_then(|p| get(p, "newName")))
                .unwrap_or("")
                .to_string();
            let r = rename(st, &td_uri(), position(), &new_name);
            reply(id, r);
        }
        "textDocument/codeLens" => reply(id, code_lenses(st, &td_uri())),
        "textDocument/signatureHelp" => {
            let r = signature_help(st, &td_uri(), position());
            reply(id, r);
        }
        "workspace/symbol" => reply(
            id,
            workspace_symbols(
                st,
                as_str(params.and_then(|p| get(p, "query"))).unwrap_or(""),
            ),
        ),
        "textDocument/selectionRange" => {
            let positions: Vec<Pos> = match params.and_then(|p| get(p, "positions")) {
                Some(Value::JArr(items)) => items
                    .iter()
                    .map(|p| Pos {
                        line: as_usize(get(p, "line")).unwrap_or(0),
                        character: as_usize(get(p, "character")).unwrap_or(0),
                    })
                    .collect(),
                _ => vec![],
            };
            reply(id, selection_ranges(st, &td_uri(), &positions));
        }
        "textDocument/semanticTokens/full" => {
            let r = semantic_tokens(st, &td_uri());
            reply(id, r);
        }
        "textDocument/inlayHint" => {
            let rg = params.and_then(|p| get(p, "range"));
            let pt = |k: &str| {
                let p = rg.and_then(|r| get(r, k));
                Pos {
                    line: as_usize(p.and_then(|x| get(x, "line"))).unwrap_or(0),
                    character: as_usize(p.and_then(|x| get(x, "character"))).unwrap_or(0),
                }
            };
            let range = (pt("start"), pt("end"));
            let r = inlay_hints(st, &td_uri(), range);
            reply(id, r);
        }
        "textDocument/prepareCallHierarchy" => {
            let r = prepare_hierarchy(st, &td_uri(), position(), "func");
            reply(id, r);
        }
        "callHierarchy/incomingCalls" => {
            reply(id, incoming_calls(st, params.and_then(|p| get(p, "item"))))
        }
        "callHierarchy/outgoingCalls" => {
            reply(id, outgoing_calls(st, params.and_then(|p| get(p, "item"))))
        }
        "textDocument/prepareTypeHierarchy" => {
            let r = prepare_hierarchy(st, &td_uri(), position(), "type");
            reply(id, r);
        }
        "typeHierarchy/supertypes" => {
            reply(id, supertypes(st, params.and_then(|p| get(p, "item"))))
        }
        "typeHierarchy/subtypes" => reply(id, subtypes(st, params.and_then(|p| get(p, "item")))),
        "textDocument/codeAction" => {
            let rg = params.and_then(|p| get(p, "range"));
            let pt = |k: &str| {
                let p = rg.and_then(|r| get(r, k));
                Pos {
                    line: as_usize(p.and_then(|x| get(x, "line"))).unwrap_or(0),
                    character: as_usize(p.and_then(|x| get(x, "character"))).unwrap_or(0),
                }
            };
            let range = (pt("start"), pt("end"));
            let diags: Vec<Value> = match params
                .and_then(|p| get(p, "context"))
                .and_then(|c| get(c, "diagnostics"))
            {
                Some(Value::JArr(items)) => items.iter().cloned().collect(),
                _ => vec![],
            };
            let r = code_actions(st, &td_uri(), range, &diags);
            reply(id, r);
        }
        "textDocument/linkedEditingRange" => {
            reply(id, linked_editing_range(st, &td_uri(), position()))
        }
        "textDocument/onTypeFormatting" => {
            let ch = as_str(params.and_then(|p| get(p, "ch")))
                .unwrap_or("")
                .to_string();
            reply(id, on_type_formatting(st, &td_uri(), position(), &ch));
        }
        "workspace/executeCommand" => {
            let command = as_str(params.and_then(|p| get(p, "command")))
                .unwrap_or("")
                .to_string();
            let r = execute_command(st, &command, params.and_then(|p| get(p, "arguments")));
            reply(id, r);
        }
        "shutdown" => reply(id, J::Null),
        "exit" => return Some(0),
        _ => reply(id, J::Null),
    }
    None
}

pub fn main() -> i32 {
    std::panic::set_hook(Box::new(|_| {})); // the error reply carries the message; nothing on stderr
    let mut st = State::default();
    let mut stdin = std::io::stdin().lock();
    let mut buf: Vec<u8> = vec![];
    let mut chunk = [0u8; 65536];
    let cl = Regex::new(r"(?i)Content-Length: (\d+)").unwrap();
    loop {
        let n = match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => return 0, // stdin closed: everything queued was handled in order
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let Some(len) = cl
                .captures(&header)
                .and_then(|c| c[1].parse::<usize>().ok())
            else {
                buf.drain(..header_end + 4);
                continue;
            };
            if buf.len() < header_end + 4 + len {
                break;
            }
            let body =
                String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + len]).to_string();
            buf.drain(..header_end + 4 + len);
            if let Ok(msg) = read_json(&body) {
                // a request whose handler panics is answered with an error, never left waiting
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle(&mut st, &msg)
                }));
                match outcome {
                    Ok(Some(code)) => return code,
                    Ok(None) => {}
                    Err(payload) => {
                        let message = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|x| x.to_string()))
                            .unwrap_or_else(|| "internal error".into());
                        notify(
                            "window/logMessage",
                            J::obj(vec![
                                ("type", J::Num(1)),
                                ("message", J::s(message.clone())),
                            ]),
                        );
                        if let Some(id) = get(&msg, "id") {
                            send(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":-32603,\"message\":{}}}}}", json_of(id), json_str(&message)));
                        }
                    }
                }
            }
        }
    }
}
