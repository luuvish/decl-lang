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
use crate::infer::{resolve_in, type_text, Target, Ty};
use crate::module::Module;
use crate::parse::parse_source;
use crate::semantics::{json_str, parse_path, read_json, seg_text, Diag, RTk, Seg, Value, RT};
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
            J::Arr(items) => format!("[{}]", items.iter().map(J::text).collect::<Vec<_>>().join(",")),
            J::Obj(es) => format!("{{{}}}", es.iter().map(|(k, v)| format!("{}:{}", json_str(k), v.text())).collect::<Vec<_>>().join(",")),
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
        Value::JArr(items) => format!("[{}]", items.iter().map(json_of).collect::<Vec<_>>().join(",")),
        Value::JObj(es) => format!("{{{}}}", es.iter().map(|(k, x)| format!("{}:{}", json_str(k), json_of(x))).collect::<Vec<_>>().join(",")),
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
        send(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", json_of(id), result.text()));
    }
}
fn notify(method: &str, params: J) {
    send(&format!("{{\"jsonrpc\":\"2.0\",\"method\":{},\"params\":{}}}", json_str(method), params.text()));
}

// ---------------- documents ----------------
pub fn path_of(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let raw = raw.split(['?', '#']).next().unwrap_or(raw);
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 {
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
        ("start", J::obj(vec![("line", J::Num(l.sl as i64)), ("character", J::Num(l.sc as i64))])),
        ("end", J::obj(vec![("line", J::Num(l.el as i64)), ("character", J::Num(l.ec as i64))])),
    ])
}
fn contains(l: Loc, p: Pos) -> bool {
    (l.sl < p.line || (l.sl == p.line && l.sc <= p.character)) && (p.line < l.el || (p.line == l.el && p.character <= l.ec))
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

#[derive(Default)]
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
        let run = session.run(Mode::Full);
        let a = Rc::new(Analysis { text, session, run, tables: RefCell::new(HashMap::new()) });
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
    let t = Rc::new(Tables { types: types.borrow().clone(), res: res.borrow().clone() });
    a.tables.borrow_mut().insert(m.path.clone(), t.clone());
    t
}
fn module_of(a: &Analysis, path: &Path) -> Option<Rc<Module>> {
    a.run.modules.iter().find(|m| m.path == path).cloned()
}
fn text_of(st: &State, m: &Module) -> String {
    st.overlay.get(&m.path).cloned().unwrap_or_else(|| read_text(&m.path))
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
        if ["error", "in", "the", "a", "is", "not", "std", "module", "import", "type", "name"].contains(&n) {
            continue;
        }
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(n))).unwrap();
        for (i, line) in lines.iter().enumerate() {
            if let Some(mm) = re.find(line) {
                let a = u16_col(line, mm.start());
                return Loc { sl: i, sc: a, el: i, ec: a + u16len(n) };
            }
        }
    }
    Loc { sl: 0, sc: 0, el: 0, ec: u16len(lines.first().copied().unwrap_or("")).max(1) }
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
            (Expr::Obj(entries), Seg::Name(k)) | (Expr::Obj(entries), Seg::Key(k)) => entries.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()),
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
    let mut item = vec![("range", range_json(loc)), ("severity", J::Num(severity_of(&d.severity))), ("source", J::s("decl"))];
    if d.code.is_some() || d.id.is_some() {
        item.push(("code", J::s(d.id.clone().or_else(|| d.code.clone()).unwrap_or_default())));
    }
    item.push(("message", J::s(if d.path.is_empty() { d.message.clone() } else { format!("{} (at {})", d.message, d.path) })));
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
                ("range", range_json(Loc { sl: *row, sc: *col, el: *row, ec: col + 1 })),
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
                    x.loc.is_some() && d.message.contains(from.strip_prefix("./").unwrap_or(from).strip_suffix(".decl").unwrap_or(from.strip_prefix("./").unwrap_or(from)))
                }
                _ => false,
            });
            out.push(diag_json(imp.and_then(|x| x.loc).unwrap_or_else(|| anchor_for(&src, &d.message)), d));
        }
        for (file, d) in &r.checks {
            if PathBuf::from(file) != path {
                continue;
            }
            out.push(diag_json(d.loc.unwrap_or_else(|| anchor_for(&src, &d.message)), d));
        }
        for d in &r.diags {
            if d.severity == "information" {
                continue;
            }
            let segs = if d.path.is_empty() { None } else { parse_path(&d.path, "").ok() };
            let Some(loc) = segs.and_then(|s| loc_of_path(&parsed.decls, &s)) else { continue }; // a root declared elsewhere: its own module's business
            out.push(diag_json(loc, d));
        }
    }
    notify("textDocument/publishDiagnostics", J::obj(vec![("uri", J::s(uri)), ("diagnostics", J::Arr(out))]));
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
    fn enter(&mut self, node: NodeRef<'a>, parents: &Vec<NodeRef<'a>>) -> Vec<NodeRef<'a>> {
        let own = node.loc().filter(|l| contains(*l, self.pos));
        if let Some(l) = own {
            if self.best.as_ref().map(|b| span(l) <= span(b.loc)).unwrap_or(true) {
                self.best = Some(Hit { node: node.clone(), loc: l, parents: parents.clone() });
            }
            let mut p = parents.clone();
            p.push(node);
            p
        } else {
            parents.clone()
        }
    }
    fn decls(&mut self, decls: &'a [Decl]) {
        for d in decls {
            self.decl(d, &vec![]);
        }
    }
    fn decl(&mut self, d: &'a Decl, parents: &Vec<NodeRef<'a>>) {
        let p = self.enter(NodeRef::Decl(d), parents);
        match &d.body {
            DeclBody::Type { params, ty, tail, .. } => {
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
            DeclBody::Func { params, ret, body, .. } => {
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
            DeclBody::Diagnostic { params, template, .. } => {
                for pr in params {
                    if let Some(t) = &pr.ty {
                        self.ty(t, &p);
                    }
                }
                self.template(template, &p);
            }
            DeclBody::Unit { factor, .. } => {
                if let Some(f) = factor {
                    self.expr(f, &p);
                }
            }
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
    fn ty(&mut self, t: &'a TypeAst, parents: &Vec<NodeRef<'a>>) {
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
            TypeAst::Named { args, preds, ext, .. } => {
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
            TypeAst::Prim { .. } | TypeAst::Lit { .. } | TypeAst::Range { .. } | TypeAst::Pattern { .. } => {}
        }
    }
    fn member(&mut self, m: &'a MemberAst, parents: &Vec<NodeRef<'a>>) {
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
    fn expr(&mut self, e: &'a Rc<Expr>, parents: &Vec<NodeRef<'a>>) {
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
            Expr::Lit(_) | Expr::UnitLit { .. } | Expr::Name(_) | Expr::Ctx(_) | Expr::Referrers { .. } | Expr::Pattern(_) => {}
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
        let from = if i == loc.sl { byte_col(lines[i], loc.sc) } else { 0 };
        if let Some(m) = re.find_at(lines[i], from) {
            let a = u16_col(lines[i], m.start());
            return Loc { sl: i, sc: a, el: i, ec: a + u16len(name) };
        }
        i += 1;
    }
    loc
}
fn member_range(text: &str, member: &MemberAst, name: &str) -> Loc {
    let loc = member.loc().unwrap();
    let line = text.split('\n').nth(loc.sl).unwrap_or("");
    match find16(line, name, loc.sc) {
        Some(i) => Loc { sl: loc.sl, sc: i, el: loc.sl, ec: i + u16len(name) },
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
    let m = a.run.modules.iter().find(|x| Rc::ptr_eq(&x.env, env))?.clone();
    let text = text_of(st, &m);
    let decl = m.decls.iter().find(|d| d.name() == Some(t.name.as_str()) && d.loc.is_some() && !matches!(d.body, DeclBody::Import { .. }))?;
    Some(Site { kind: decl_kind(decl).to_string(), module: m.clone(), decl: Some(decl_id(decl)), decl_loc: decl.loc, member_loc: None, range: name_range(&text, decl, &t.name), name: t.name.clone() })
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
fn member_site(st: &State, a: &Analysis, m: &Rc<Module>, rt: Option<&RT>, member: &str) -> Option<Site> {
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
        let decl = sm.decls.iter().find(|d| decl_id(d) == site.decl.unwrap_or(0))?;
        let DeclBody::Type { ty, .. } = &decl.body else { return None };
        let members = record_members(ty);
        if let Some(mem) = members.iter().find(|x| x.name() == Some(member)) {
            if mem.loc().is_some() {
                return Some(Site { kind: "member".into(), module: sm.clone(), decl: Some(decl_id(decl)), decl_loc: decl.loc, member_loc: mem.loc(), range: member_range(&text_of(st, &sm), mem, member), name: member.to_string() });
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
    let Some(hit) = node_at(decls, pos) else { return Some(SiteAt { site: None, ty: None, hit: None, module: m }) };
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
                    Some(SiteAt { site: site_of_target(st, a, target.as_ref()), ty, hit: Some(hit), module: m })
                }
                Expr::Member { x, name, .. } => {
                    if let Expr::Name(xn) = &**x {
                        if m.env.namespaces.borrow().contains_key(xn) {
                            let site = ns_export_site(st, a, &m, xn, name);
                            return Some(SiteAt { site, ty, hit: Some(hit), module: m });
                        }
                    }
                    let xt = t.types.get(&key_of(x)).cloned();
                    let site = member_site(st, a, &m, xt.as_ref().and_then(|t| t.rt.as_ref()), name);
                    Some(SiteAt { site, ty, hit: Some(hit), module: m })
                }
                _ => Some(SiteAt { site: None, ty, hit: Some(hit), module: m }),
            }
        }
        NodeRef::Type(TypeAst::Named { name, .. }) => {
            let mut parts = name.splitn(2, '.');
            let head = parts.next().unwrap_or("");
            let tail = parts.next();
            let target = match tail {
                Some(tail) if m.env.namespaces.borrow().contains_key(head) => {
                    let site = ns_export_site(st, a, &m, head, tail);
                    return Some(SiteAt { site, ty: None, hit: Some(hit), module: m });
                }
                _ => resolve_in(&m.env, head),
            };
            Some(SiteAt { site: site_of_target(st, a, target.as_ref()), ty: None, hit: Some(hit), module: m })
        }
        NodeRef::Member(mem) if mem.name().is_some() => {
            // the member's own declaration
            let name = mem.name().unwrap().to_string();
            let decl = hit.parents.iter().find_map(|p| if let NodeRef::Decl(d) = p { Some(*d) } else { None });
            let site = decl.map(|d| Site { kind: "member".into(), module: m.clone(), decl: Some(decl_id(d)), decl_loc: d.loc, member_loc: mem.loc(), range: member_range(&text_of(st, &m), mem, &name), name });
            Some(SiteAt { site, ty: None, hit: Some(hit), module: m })
        }
        NodeRef::Decl(d) if d.name().is_some() => {
            let name = d.name().unwrap().to_string();
            let r = name_range(&text_of(st, &m), d, &name);
            if contains(r, pos) {
                let site = Site { kind: decl_kind(d).to_string(), module: m.clone(), decl: Some(decl_id(d)), decl_loc: d.loc, member_loc: None, range: r, name };
                return Some(SiteAt { site: Some(site), ty: None, hit: Some(hit), module: m });
            }
            Some(SiteAt { site: None, ty: None, hit: Some(hit), module: m })
        }
        _ => Some(SiteAt { site: None, ty: None, hit: Some(hit), module: m }),
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
        out.extend(body.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
        return out;
    }
    let l = site.decl_loc.unwrap();
    let mut out = doc_above(l.sl);
    let body: Vec<String> = lines[l.sl..=l.el.min(lines.len() - 1)].iter().map(|x| x.to_string()).collect();
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
    let Some(a) = st.analysis_of(uri) else { return J::Null };
    let Some(s) = site_at(st, &a, uri, pos) else { return J::Null };
    let mut parts: Vec<String> = vec![];
    if let Some(site) = &s.site {
        let lines = decl_text(st, site);
        let doc: Vec<String> = lines.iter().filter(|l| l.starts_with("///")).map(|l| l.strip_prefix("///").map(|r| r.strip_prefix(' ').unwrap_or(r)).unwrap_or(l).to_string()).collect();
        let code: Vec<&String> = lines.iter().filter(|l| !l.starts_with("///")).collect();
        if !doc.is_empty() {
            parts.push(doc.join("\n"));
        }
        parts.push(format!("```decl\n{}\n```", code.iter().map(|x| x.as_str()).collect::<Vec<_>>().join("\n")));
    }
    if let Some(ty) = &s.ty {
        parts.push(format!("`{}{}`", type_text(ty.rt.as_ref()), if ty.abs { "?" } else { "" }));
    }
    if parts.is_empty() {
        return J::Null;
    }
    let contents = J::obj(vec![("kind", J::s("markdown")), ("value", J::s(parts.join("\n\n")))]);
    match s.hit.as_ref().map(|h| h.loc) {
        Some(l) => J::obj(vec![("contents", contents), ("range", range_json(l))]),
        None => J::obj(vec![("contents", contents)]),
    }
}

// ---------------- navigation ----------------
fn location(m: &Module, loc: Loc) -> J {
    J::obj(vec![("uri", J::s(uri_of(&m.path))), ("range", range_json(loc))])
}
fn definition(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else { return J::Null };
    match site_at(st, &a, uri, pos).and_then(|s| s.site) {
        Some(site) => location(&site.module, site.range),
        None => J::Null,
    }
}
fn type_definition(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else { return J::Null };
    let Some(s) = site_at(st, &a, uri, pos) else { return J::Null };
    let Some(name) = rec_name(s.ty.as_ref().and_then(|t| t.rt.as_ref())) else { return J::Null };
    match site_of_target(st, &a, resolve_in(&s.module.env, &name).as_ref()) {
        Some(site) => location(&site.module, site.range),
        None => J::Null,
    }
}
fn same(x: Option<&Site>, target: &Site) -> bool {
    match x {
        Some(x) => Rc::ptr_eq(&x.module, &target.module) && x.name == target.name && x.kind == target.kind && (x.kind != "member" || x.decl == target.decl),
        None => false,
    }
}
fn member_token_loc(text: &str, e: &Rc<Expr>, name: &str) -> Loc {
    let l = expr_loc(e).unwrap();
    let line = text.split('\n').nth(l.el).unwrap_or("");
    match rfind16(line, name, l.ec) {
        Some(i) => Loc { sl: l.el, sc: i, el: l.el, ec: i + u16len(name) },
        None => l,
    }
}
fn type_name_loc(l: Loc, offset: usize, name: &str) -> Loc {
    Loc { sl: l.sl, sc: l.sc + offset, el: l.sl, ec: l.sc + offset + u16len(name) }
}
fn import_item_loc(text: &str, d: &Decl, name: &str) -> Loc {
    let l = d.loc.unwrap();
    let line = text.split('\n').nth(l.sl).unwrap_or("");
    match find16(line, name, l.sc) {
        Some(i) => Loc { sl: l.sl, sc: i, el: l.sl, ec: i + u16len(name) },
        None => l,
    }
}
// every reference to a site across the universe: name and member nodes
// that resolve to the same declaration, plus the declaration itself
fn references(st: &mut State, uri: &str, pos: Pos, include_declaration: bool) -> Vec<(Rc<Module>, Loc)> {
    let Some(a) = st.analysis_of(uri) else { return vec![] };
    let Some(target) = site_at(st, &a, uri, pos).and_then(|s| s.site) else { return vec![] };
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
                        Expr::Name(xn) if m.env.namespaces.borrow().contains_key(xn) => ns_export_site(st, &a, &m, xn, name),
                        _ => member_site(st, &a, &m, t.types.get(&key_of(xx)).and_then(|t| t.rt.as_ref()), name),
                    };
                    if same(site.as_ref(), &target) {
                        out.push((m.clone(), member_token_loc(&text, x, name)));
                    }
                }
                _ => {}
            }
        }
        for ty in types {
            let TypeAst::Named { name, loc: Some(l), .. } = ty else { continue };
            let mut parts = name.splitn(2, '.');
            let head = parts.next().unwrap_or("");
            let tail = parts.next();
            let site = match tail {
                Some(tail) if m.env.namespaces.borrow().contains_key(head) => ns_export_site(st, &a, &m, head, tail),
                _ => site_of_target(st, &a, resolve_in(&m.env, head).as_ref()),
            };
            if same(site.as_ref(), &target) {
                out.push((m.clone(), type_name_loc(*l, if tail.is_some() { u16len(head) + 1 } else { 0 }, tail.unwrap_or(head))));
            }
        }
        // import items naming the declaration
        for d in &m.decls {
            let (names, has_loc) = match &d.body {
                DeclBody::Import { names: Some(names), .. } => (names, d.loc.is_some()),
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
                    if same(site_of_target(st, &a, resolve_in(&im.env, &im.name).as_ref()).as_ref(), &target) {
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
        let (pp, qp) = (p.0.path.to_string_lossy().to_string(), q.0.path.to_string_lossy().to_string());
        pp.cmp(&qp).then(p.1.sl.cmp(&q.1.sl)).then(p.1.sc.cmp(&q.1.sc))
    });
    kept
}
// the expression and type nodes of a declaration, in the reference's traversal order
fn collect_decl<'a>(d: &'a Decl, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    let mut ty = |t: &'a TypeAst| collect_type(t, exprs, types);
    match &d.body {
        DeclBody::Type { params, ty: t, tail, .. } => {
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
        DeclBody::Func { params, ret, body, .. } => {
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
        DeclBody::Input { ty: t, fallback, .. } => {
            collect_type(t, exprs, types);
            if let Some(f) = fallback {
                collect_expr(f, exprs, types);
            }
        }
        DeclBody::Diagnostic { params, template, .. } => {
            for p in params {
                if let Some(pt) = &p.ty {
                    collect_type(pt, exprs, types);
                }
            }
            collect_template(template, exprs, types);
        }
        DeclBody::Unit { factor, .. } => {
            if let Some(f) = factor {
                collect_expr(f, exprs, types);
            }
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
fn collect_template<'a>(parts: &'a [TPart], exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    for p in parts {
        if let TPart::Expr(x) = p {
            collect_expr(x, exprs, types);
        }
    }
}
fn collect_type<'a>(t: &'a TypeAst, exprs: &mut Vec<Rc<Expr>>, types: &mut Vec<&'a TypeAst>) {
    types.push(t);
    match t {
        TypeAst::Record { members, .. } => members.iter().for_each(|m| collect_member(m, exprs, types)),
        TypeAst::Map { key, val, .. } => {
            collect_type(key, exprs, types);
            collect_type(val, exprs, types);
        }
        TypeAst::Array { elem, .. } => collect_type(elem, exprs, types),
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => arms.iter().for_each(|a| collect_type(a, exprs, types)),
        TypeAst::Func { params, ret, .. } => {
            params.iter().for_each(|a| collect_type(a, exprs, types));
            collect_type(ret, exprs, types);
        }
        TypeAst::Named { args, preds, ext, .. } => {
            args.iter().for_each(|a| collect_type(a, exprs, types));
            preds.iter().flatten().for_each(|x| collect_expr(x, exprs, types));
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
    let Some(text) = st.text(uri).cloned() else { return J::obj(vec![("isIncomplete", J::Bool(false)), ("items", J::Arr(vec![]))]) };
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
                    if d.starts_with("derived") || d.starts_with("required") || d.starts_with("optional") || d.starts_with("defaulted") {
                        5
                    } else {
                        6
                    }
                }
                None => {
                    if label.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
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
    J::obj(vec![("isIncomplete", J::Bool(false)), ("items", J::Arr(items))])
}

// ---------------- symbols, folding, formatting ----------------
fn symbol_kind(d: &Decl) -> Option<i64> {
    Some(match &d.body {
        DeclBody::Type { .. } => 5,
        DeclBody::Const { .. } => 14,
        DeclBody::Func { .. } => 12,
        DeclBody::Output { .. } | DeclBody::Input { .. } | DeclBody::Dimension { .. } | DeclBody::Unit { .. } => 13,
        DeclBody::Diagnostic { .. } => 24,
        _ => return None,
    })
}
fn document_symbols(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else { return J::Arr(vec![]) };
    let parsed = parse_source(text);
    if !parsed.errors.is_empty() {
        return J::Arr(vec![]);
    }
    let mut out: Vec<J> = vec![];
    for d in &parsed.decls {
        let (Some(loc), Some(name), Some(kind)) = (d.loc, d.name(), symbol_kind(d)) else { continue };
        let mut sym = vec![("name", J::s(name)), ("kind", J::Num(kind)), ("range", range_json(loc)), ("selectionRange", range_json(name_range(text, d, name)))];
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
                    J::obj(vec![("name", J::s(label)), ("kind", J::Num(kind)), ("range", range_json(m.loc().unwrap())), ("selectionRange", range_json(member_range(text, m, n)))])
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
    let Some(text) = st.text(uri) else { return J::Arr(vec![]) };
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
        out.push(J::obj(vec![("startLine", J::Num(r.0 as i64)), ("endLine", J::Num(r.1 as i64)), ("kind", J::s("region"))]));
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
        DeclBody::Type { params, ty, tail, .. } => {
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
        DeclBody::Func { params, ret, body, .. } => {
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
        DeclBody::Diagnostic { params, template, .. } => {
            for p in params {
                if let Some(t) = &p.ty {
                    fold_type(t, out);
                }
            }
            fold_template(template, out);
        }
        DeclBody::Unit { factor, .. } => {
            if let Some(f) = factor {
                fold_expr(f, out);
            }
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
        TypeAst::Union { arms, .. } | TypeAst::Isect { arms, .. } => arms.iter().for_each(|a| fold_type(a, out)),
        TypeAst::Func { params, ret, .. } => {
            params.iter().for_each(|a| fold_type(a, out));
            fold_type(ret, out);
        }
        TypeAst::Named { args, preds, ext, .. } => {
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
    let Some(text) = st.text(uri) else { return J::Arr(vec![]) };
    let Ok(out) = format(text) else { return J::Arr(vec![]) };
    if &out == text {
        return J::Arr(vec![]);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    J::Arr(vec![J::obj(vec![
        ("range", range_json(Loc { sl: 0, sc: 0, el: last, ec: u16len(lines[last]) })),
        ("newText", J::s(out)),
    ])])
}

// ---------------- rename ----------------
fn prepare_rename(st: &mut State, uri: &str, pos: Pos) -> J {
    let Some(a) = st.analysis_of(uri) else { return J::Null };
    let Some(s) = site_at(st, &a, uri, pos) else { return J::Null };
    let (Some(site), Some(hit)) = (&s.site, &s.hit) else { return J::Null };
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
    J::obj(vec![("range", range_json(loc)), ("placeholder", J::s(site.name.clone()))])
}
fn rename(st: &mut State, uri: &str, pos: Pos, new_name: &str) -> J {
    let refs = references(st, uri, pos, true);
    if refs.is_empty() {
        return J::Null;
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
    J::obj(vec![("changes", J::Obj(changes.into_iter().map(|(k, v)| (k, J::Arr(v))).collect()))])
}

// ---------------- lenses and commands ----------------
fn code_lenses(st: &State, uri: &str) -> J {
    let Some(text) = st.text(uri) else { return J::Arr(vec![]) };
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
            ("range", range_json(Loc { sl: l.sl, sc: l.sc, el: l.sl, ec: l.sc })),
            ("command", J::obj(vec![("title", J::s(title)), ("command", J::s(command)), ("arguments", J::Arr(vec![J::s(uri), J::s(name.clone())]))])),
        ]));
    }
    J::Arr(out)
}
fn execute_command(st: &mut State, command: &str, args: Option<&Value>) -> J {
    let items: Vec<&Value> = match args {
        Some(Value::JArr(a)) => a.iter().collect(),
        _ => vec![],
    };
    let Some(uri) = as_str(items.first().copied()) else { return J::Null };
    let root = as_str(items.get(1).copied()).map(|s| s.to_string());
    let path = path_of(uri);
    let mut session = Session::with_overlay(Some(&path.to_string_lossy()), Some(&st.overlay));
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for (name, file) in st.inputs.clone() {
        let abs = std::path::absolute(dir.join(&file)).unwrap_or_else(|_| dir.join(&file));
        let _ = session.apply(Op::Bind { name, src: BindSource::File { file: file.clone(), text: read_text(&abs) } });
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
            let Ok((run, ds, _exported)) = session.evaluate(&names) else { return J::Null };
            let diagnostics = diags_of(&run, &run.diags);
            if let Some(r) = root {
                let doc = ds.first().and_then(|(_, j)| j.clone());
                return J::obj(vec![("root", J::s(r)), ("document", doc.map(J::s).unwrap_or(J::Null)), ("diagnostics", diagnostics)]);
            }
            let all = if run.eng.is_some() && ds.iter().all(|(_, j)| j.is_some()) {
                Some(format!("{{{}}}", ds.iter().map(|(n, j)| format!("{}:{}", json_str(n), j.clone().unwrap())).collect::<Vec<_>>().join(",")))
            } else {
                None
            };
            J::obj(vec![("root", J::Null), ("document", all.map(J::s).unwrap_or(J::Null)), ("diagnostics", diagnostics)])
        }
        "decl.validate" => {
            let Ok((run, verdicts, diags)) = session.validate(&names) else { return J::Null };
            let vs: Vec<J> = verdicts.iter().map(|(n, e, w)| J::obj(vec![("name", J::s(n.clone())), ("errors", J::Num(*e as i64)), ("warnings", J::Num(*w as i64))])).collect();
            J::obj(vec![("verdicts", J::Arr(vs)), ("diagnostics", diags_of(&run, &diags))])
        }
        "decl.trace" => match root {
            Some(r) => match session.trace(&r) {
                Ok(lines) => J::obj(vec![("lines", J::Arr(lines.into_iter().map(J::s).collect()))]),
                Err(_) => J::Null,
            },
            None => J::Null,
        },
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

// ---------------- request handling ----------------
/// returns the exit code when the client asked to exit
fn handle(st: &mut State, msg: &Value) -> Option<i32> {
    let id = get(msg, "id");
    let method = as_str(get(msg, "method")).unwrap_or("");
    let params = get(msg, "params");
    let td_uri = || as_str(params.and_then(|p| get(p, "textDocument")).and_then(|t| get(t, "uri"))).unwrap_or("").to_string();
    let position = || {
        let pos = params.and_then(|p| get(p, "position"));
        Pos { line: as_usize(pos.and_then(|p| get(p, "line"))).unwrap_or(0), character: as_usize(pos.and_then(|p| get(p, "character"))).unwrap_or(0) }
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
                ("renameProvider", J::obj(vec![("prepareProvider", J::Bool(true))])),
                ("completionProvider", J::obj(vec![("triggerCharacters", J::Arr(vec![J::s("."), J::s("$"), J::s(":")]))])),
                ("codeLensProvider", J::obj(vec![("resolveProvider", J::Bool(false))])),
                ("executeCommandProvider", J::obj(vec![("commands", J::Arr(["decl.evaluate", "decl.validate", "decl.trace", "decl.reloadWorkspace"].iter().map(|c| J::s(*c)).collect()))])),
            ]);
            reply(id, J::obj(vec![("capabilities", caps), ("serverInfo", J::obj(vec![("name", J::s("decl-lsp")), ("version", J::s("0.3.0"))]))]));
        }
        "initialized" => {}
        "workspace/didChangeConfiguration" => {
            let inputs = params.and_then(|p| get(p, "settings")).and_then(|s| get(s, "decl")).and_then(|d| get(d, "inputs"));
            st.inputs = match inputs {
                Some(Value::JObj(es)) => es.iter().filter_map(|(k, v)| as_str(Some(v)).map(|f| (k.clone(), f.to_string()))).collect(),
                _ => vec![],
            };
            reanalyze(st);
        }
        "workspace/didChangeWatchedFiles" => reanalyze(st),
        "textDocument/didOpen" => {
            let uri = td_uri();
            let text = as_str(params.and_then(|p| get(p, "textDocument")).and_then(|t| get(t, "text"))).unwrap_or("").to_string();
            st.set(&uri, text);
            st.analyses.clear();
            analyze(st, &uri);
        }
        "textDocument/didChange" => {
            let uri = td_uri();
            let text = params
                .and_then(|p| get(p, "contentChanges"))
                .and_then(|c| if let Value::JArr(items) = c { items.first() } else { None })
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
            notify("textDocument/publishDiagnostics", J::obj(vec![("uri", J::s(uri.clone())), ("diagnostics", J::Arr(vec![]))]));
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
            let incl = as_bool(params.and_then(|p| get(p, "context")).and_then(|c| get(c, "includeDeclaration"))).unwrap_or(false);
            let refs = references(st, &td_uri(), position(), incl);
            reply(id, J::Arr(refs.iter().map(|(m, l)| location(m, *l)).collect()));
        }
        "textDocument/documentHighlight" => {
            let uri = td_uri();
            let path = path_of(&uri);
            let refs = references(st, &uri, position(), true);
            reply(id, J::Arr(refs.iter().filter(|(m, _)| m.path == path).map(|(_, l)| J::obj(vec![("range", range_json(*l)), ("kind", J::Num(1))])).collect()));
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
            let new_name = as_str(params.and_then(|p| get(p, "newName"))).unwrap_or("").to_string();
            let r = rename(st, &td_uri(), position(), &new_name);
            reply(id, r);
        }
        "textDocument/codeLens" => reply(id, code_lenses(st, &td_uri())),
        "workspace/executeCommand" => {
            let command = as_str(params.and_then(|p| get(p, "command"))).unwrap_or("").to_string();
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
        loop {
            let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { break };
            let header = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let Some(len) = cl.captures(&header).and_then(|c| c[1].parse::<usize>().ok()) else {
                buf.drain(..header_end + 4);
                continue;
            };
            if buf.len() < header_end + 4 + len {
                break;
            }
            let body = String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + len]).to_string();
            buf.drain(..header_end + 4 + len);
            if let Ok(msg) = read_json(&body) {
                if let Some(code) = handle(&mut st, &msg) {
                    return code;
                }
            }
        }
    }
}
