//! The session object (docs/tooling/02_repl.md §1) — a port of the
//! reference implementation's session.ts: a universe — the modules loaded
//! from an entry file, their texts taken as a snapshot — plus an operation
//! log (bindings, document edits, session declarations, reloads). The
//! state is the universe with the log applied, recomputed deterministically
//! from the snapshot, which is what makes `:undo` exact and a scripted
//! session reproducible. The REPL (repl.rs) drives it; nothing here prints,
//! and every answer is the same checker, inference, and engine the command
//! line runs.
use crate::ast::{Decl, DeclBody, Expr, TPart};
use crate::checker::check_module;
use crate::engine::{fmt_f, Engine, RootSrc};
use crate::fmt::format;
use crate::infer::{infer, make_ctx, std_names, type_text, Ctx, Ty};
use crate::module::{load_modules, Module};
use crate::package::{open_package_universe, verify_lock};
use crate::parse::parse_source;
use crate::semantics::{
    json_str, parse_path, path_str, read_json, rec_members, seg_text, sort_diags, Diag, Env, Fail,
    MKind, RTk, Scope, Seg, SegPath, SlotState, Value, RT,
};
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Instant;

// a detached declaration's type text on one line (compiled once)
static SQUEEZE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\n\s*").unwrap());

// ---------------- operations ----------------
#[derive(Clone, Debug)]
/// what a document is bound from
pub enum BindSource {
    /// a JSON file, with its text as read
    File {
        /// the file's path
        file: String,
        /// its text
        text: String,
    },
    /// JSON text given in the session
    Inline {
        /// the text
        text: String,
    },
    /// an expression evaluated over the universe
    Expr {
        /// the expression's text
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// an edit of a document
pub enum EditKind {
    /// a member added
    Create,
    /// a member's value replaced
    Update,
    /// a member removed
    Remove,
}
impl EditKind {
    /// The edit's name, as the REPL spells it.
    pub fn word(self) -> &'static str {
        match self {
            EditKind::Create => "create",
            EditKind::Update => "update",
            EditKind::Remove => "remove",
        }
    }
}

#[derive(Clone)]
/// an operation of the session log (docs/tooling/02_repl.md §1)
pub enum Op {
    /// bind a document to a root
    Bind {
        /// the root
        name: String,
        /// the document
        src: BindSource,
    },
    /// unbind a root
    Unbind {
        /// the root
        name: String,
    },
    /// edit a document
    Edit {
        /// the edit
        kind: EditKind,
        /// the canonical path edited
        path: String,
        /// the new value, as an expression
        expr: Option<String>,
    },
    /// declare a type, a function, or a constant in the session
    Declare {
        /// its name
        name: String,
        /// the declaration's text
        text: String,
    },
    /// add a session output
    Output {
        /// its name
        name: String,
        /// its type annotation
        ty: Option<String>,
        /// its expression
        expr: String,
    },
    /// drop a session declaration
    Drop {
        /// its name
        name: String,
    },
    /// re-read the universe from disk
    Reload {
        /// the texts as they were, so undo restores them
        snapshot: HashMap<PathBuf, String>,
    },
    /// forget every operation
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// where a root's document came from
pub enum Origin {
    /// a file
    File,
    /// inline text
    Inline,
    /// an expression
    Expr,
    /// the input's fallback
    Fallback,
    /// an output detached by an edit
    Detached,
}
impl Origin {
    /// The origin's name, as `:roots` shows it.
    pub fn word(self) -> &'static str {
        match self {
            Origin::File => "file",
            Origin::Inline => "inline",
            Origin::Expr => "expr",
            Origin::Fallback => "fallback",
            Origin::Detached => "detached",
        }
    }
}

/// the document a root is built from, as the session holds it
#[derive(Clone)]
pub struct Document {
    /// where it came from
    pub origin: Origin,
    /// the file, for a file
    pub file: Option<String>,
    /// the document, in the JSON reader's shape
    pub doc: Value,
    /// what it started from, for `:diff`
    pub base: Value,
    /// whether an edit changed it
    pub edited: bool,
}

#[derive(Default)]
struct State {
    snapshot: HashMap<PathBuf, String>,
    decls: Vec<(String, String)>, // session declarations, in order
    outputs: Vec<(String, Option<String>, String)>, // session outputs `x = e`
    documents: Vec<(String, Document)>,
}
impl State {
    fn decl(&self, n: &str) -> Option<&String> {
        self.decls.iter().find(|(k, _)| k == n).map(|(_, t)| t)
    }
    fn output(&self, n: &str) -> Option<(&Option<String>, &String)> {
        self.outputs
            .iter()
            .find(|(k, _, _)| k == n)
            .map(|(_, t, e)| (t, e))
    }
    fn document(&self, n: &str) -> Option<&Document> {
        self.documents.iter().find(|(k, _)| k == n).map(|(_, d)| d)
    }
    fn document_mut(&mut self, n: &str) -> Option<&mut Document> {
        self.documents
            .iter_mut()
            .find(|(k, _)| k == n)
            .map(|(_, d)| d)
    }
    fn set_document(&mut self, n: &str, d: Document) {
        if let Some(e) = self.documents.iter_mut().find(|(k, _)| k == n) {
            e.1 = d;
        } else {
            self.documents.push((n.to_string(), d));
        }
    }
    fn remove_decl(&mut self, n: &str) -> bool {
        let before = self.decls.len();
        self.decls.retain(|(k, _)| k != n);
        before != self.decls.len()
    }
    fn remove_output(&mut self, n: &str) -> bool {
        let before = self.outputs.len();
        self.outputs.retain(|(k, _, _)| k != n);
        before != self.outputs.len()
    }
}

#[derive(Debug, Clone)]
/// an operation refused, with the reason
pub struct SessionError(pub String);
impl SessionError {
    fn new(msg: impl Into<String>) -> SessionError {
        SessionError(msg.into())
    }
}
/// an operation's outcome
pub type SResult<T> = Result<T, SessionError>;

#[derive(Clone, Copy, Debug)]
/// the milliseconds of a run, by phase (`:time`)
pub struct Timing {
    /// loading
    pub load: f64,
    /// checking
    pub check: f64,
    /// binding
    pub bind: f64,
    /// evaluating
    pub evaluate: f64,
    /// all of it
    pub total: f64,
    /// the incremental step: how many slots were recomputed, and out of how many (a full run: None)
    pub recomputed: Option<usize>,
    /// the slots in the universe, when the incremental step counted them
    pub slots: Option<usize>,
}

/// full recomputation on every question (the harness's cross-check)
pub fn full_recompute() -> bool {
    std::env::var("DECL_FULL_RECOMPUTE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq)]
/// how much of a run to do
pub enum Mode {
    /// load and check only
    Check,
    /// evaluate what is asked
    Lazy,
    /// evaluate everything
    Full,
}

#[derive(Clone)]
/// one run of the universe: the modules, the checks, the engine, the diagnostics
pub struct Run {
    /// the modules loaded
    pub modules: Vec<Rc<Module>>,
    /// the entry module
    pub entry: Option<Rc<Module>>,
    /// the loading diagnostics
    pub load_diags: Vec<Diag>,
    /// the static findings, by file
    pub checks: Vec<(String, Diag)>,
    /// session outputs whose expressions do not check (path: the output)
    pub session_checks: Vec<Diag>,
    /// the session outputs bound, as bound
    pub session_roots: Vec<(String, Rc<Expr>, RT)>,
    /// the engine, when evaluation ran
    pub eng: Option<Rc<Engine>>,
    /// the evaluation diagnostics
    pub diags: Vec<Diag>,
    /// the timing
    pub timing: Timing,
}

/// a root as `:roots` lists it
pub struct RootInfo {
    /// output or input
    pub kind: &'static str,
    /// its name
    pub name: String,
    /// the module declaring it; empty for a session root
    pub module: String,
    /// whether it is exported
    pub exported: bool,
    /// whether the session declared it
    pub session: bool,
    /// how it is bound, for an input or a detached output
    pub binding: String,
    /// the bound file, when there is one
    pub detail: String,
    /// whether an edit changed its document
    pub edited: bool,
}

/// an expression evaluated in the session
pub struct ExprResult {
    /// its value as JSON, when it has one
    pub value: Option<String>,
    /// the diagnostics it raised
    pub diags: Vec<Diag>,
    /// Some(code, message): a failure; an empty message prints only `(invalid)`
    pub error: Option<(Option<String>, String)>,
}

fn ms(i: Instant) -> f64 {
    i.elapsed().as_secs_f64() * 1000.0
}
/// Whether a diagnostic concerns a root: its path starts there.
pub fn is_root_diag(d: &Diag, root: &str) -> bool {
    d.path == root
        || d.path.starts_with(&format!("{root}."))
        || d.path.starts_with(&format!("{root}["))
}

/// parse one expression: the text is wrapped in a constant declaration
pub fn parse_expr(text: &str) -> SResult<Rc<Expr>> {
    let r = parse_source(&format!("const __e = {text}\n"));
    if r.errors.is_empty() && r.decls.len() == 1 {
        if let DeclBody::Const { expr, .. } = &r.decls[0].body {
            return Ok(expr.clone());
        }
    }
    Err(SessionError::new(format!(
        "cannot parse expression: {}",
        text.trim()
    )))
}

/// parse one module-level declaration; returns it with its name
pub fn parse_decl(text: &str) -> SResult<(Decl, String)> {
    let r = parse_source(&format!("{}\n", text.trim()));
    if !r.errors.is_empty() || r.decls.len() != 1 {
        return Err(SessionError::new(format!(
            "cannot parse declaration: {}",
            text.trim().lines().next().unwrap_or("")
        )));
    }
    let d = r.decls.into_iter().next().unwrap();
    let name = match (&d.body, d.name()) {
        (_, Some(n)) => n.to_string(),
        (DeclBody::Import { from, .. }, None) => format!("import {from}"),
        (DeclBody::ReExport { from, .. }, None) => format!("re_export {from}"),
        _ => String::new(),
    };
    Ok((d, name))
}

fn parse_doc(text: &str, what: &str) -> SResult<Value> {
    read_json(text).map_err(|_| SessionError::new(format!("{what} is not well-formed JSON")))
}

// ---------------- JSON documents (read_json's shape) ----------------
/// A document in the wire form: canonical JSON, one line.
pub fn doc_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => fmt_f(*f),
        Value::Str(s) => json_str(s),
        Value::JArr(items) => format!(
            "[{}]",
            items.iter().map(doc_json).collect::<Vec<_>>().join(",")
        ),
        Value::JObj(es) => format!(
            "{{{}}}",
            es.iter()
                .map(|(k, x)| format!("{}:{}", json_str(k), doc_json(x)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => "null".into(),
    }
}

fn doc_step(v: &Value, seg: &Seg) -> Option<Value> {
    match (v, seg) {
        (Value::JObj(es), Seg::Name(k)) | (Value::JObj(es), Seg::Key(k)) => {
            es.iter().find(|(kk, _)| kk == k).map(|(_, x)| x.clone())
        }
        (Value::JArr(items), Seg::Idx(i)) => items.get(*i).cloned(),
        _ => None,
    }
}

// ---------------- pretty printing ----------------
/// canonical JSON, re-indented (numbers and strings untouched)
pub fn pretty_json(compact: &str) -> String {
    let cs: Vec<char> = compact.chars().collect();
    let mut out = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    let pad = |d: usize| "  ".repeat(d);
    while i < cs.len() {
        let c = cs[i];
        if c == '"' {
            let mut j = i + 1;
            while j < cs.len() && cs[j] != '"' {
                if cs[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            out.extend(&cs[i..=j.min(cs.len() - 1)]);
            i = j + 1;
            continue;
        }
        if c == '{' || c == '[' {
            let close = if c == '{' { '}' } else { ']' };
            if i + 1 < cs.len() && cs[i + 1] == close {
                out.push(c);
                out.push(close);
                i += 2;
                continue;
            }
            depth += 1;
            out.push(c);
            out.push('\n');
            out.push_str(&pad(depth));
            i += 1;
            continue;
        }
        if c == '}' || c == ']' {
            depth = depth.saturating_sub(1);
            out.push('\n');
            out.push_str(&pad(depth));
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            out.push_str(",\n");
            out.push_str(&pad(depth));
            i += 1;
            continue;
        }
        if c == ':' {
            out.push_str(": ");
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn identifiers(text: &str) -> HashSet<String> {
    Regex::new(r"[A-Za-z_][A-Za-z0-9_]*")
        .unwrap()
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

fn relative_path(from_dir: &Path, p: &Path) -> String {
    let a: Vec<_> = from_dir.components().collect();
    let b: Vec<_> = p.components().collect();
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    let mut parts: Vec<String> = vec!["..".to_string(); a.len() - i];
    parts.extend(
        b[i..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().to_string()),
    );
    parts.join("/")
}

// ---------------- the session ----------------
/// the session: the universe of an entry module, the documents bound, the
/// session's declarations, and the log of operations with its cursor
pub struct Session {
    /// the entry module, absolute
    pub entry_path: Option<PathBuf>,
    /// the operations applied, in order
    pub log: Vec<Op>,
    /// how many of them are in force (undo moves it back)
    pub cursor: usize,
    /// the last run's timing
    pub last_timing: Cell<Option<Timing>>,
    snapshot0: HashMap<PathBuf, String>,
    state: State,
    // the last full run, kept for the incremental step (§6): reused as long
    // as the universe's texts and declarations are the same, its engine
    // rebinding the documents that changed and recomputing what read them
    last: RefCell<Option<Last>>,
    /// texts that override the disk (the language server's open buffers), by absolute path
    pub overlay: HashMap<PathBuf, String>,
}

/// the file name of the session's own declarations
pub const SCRATCH: &str = "<session>";

#[derive(Clone)]
struct Last {
    key: String,
    docs: Vec<(String, String)>,
    run: Run,
}

fn under(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{root}.")) || path.starts_with(&format!("{root}["))
}

impl Session {
    /// A session over an entry module (none: a scratch session).
    pub fn new(entry: Option<&str>) -> Session {
        Session::with_overlay(entry, None)
    }
    /// `overlay`: texts that override the disk (the language server's open buffers), by absolute path
    pub fn with_overlay(
        entry: Option<&str>,
        overlay: Option<&HashMap<PathBuf, String>>,
    ) -> Session {
        let entry_path = entry.map(|e| std::path::absolute(e).unwrap_or_else(|_| PathBuf::from(e)));
        let mut s = Session {
            entry_path,
            log: vec![],
            cursor: 0,
            last_timing: Cell::new(None),
            snapshot0: HashMap::new(),
            state: State::default(),
            last: RefCell::new(None),
            overlay: overlay.cloned().unwrap_or_default(),
        };
        s.snapshot0 = s.snapshot_from_disk();
        s.state = s.initial_state();
        s
    }

    /// The entry module's absolute path.
    pub fn entry_abs(&self) -> PathBuf {
        self.entry_path.clone().unwrap_or_else(|| {
            std::path::absolute(SCRATCH).unwrap_or_else(|_| PathBuf::from(SCRATCH))
        })
    }
    /// The entry module's file name, as `:roots` shows it.
    pub fn entry_name(&self) -> String {
        self.entry_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| SCRATCH.to_string())
    }

    // the universe's texts as they are on disk now: the entry and every
    // module reachable from it (a module that cannot be read is absent and
    // reported on use, as the command line reports it)
    fn snapshot_from_disk(&self) -> HashMap<PathBuf, String> {
        let mut snap = HashMap::new();
        let Some(entry) = &self.entry_path else {
            return snap;
        };
        let pkg = open_package_universe(entry);
        let r = load_modules(
            entry,
            pkg.as_ref().map(|u| &u.resolver),
            Some(&self.overlay),
        );
        let mut paths: Vec<PathBuf> = vec![entry.clone()];
        paths.extend(r.modules.iter().map(|m| m.path.clone()));
        for p in paths {
            if let Some(t) = self.overlay.get(&p) {
                snap.insert(p, t.clone());
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(&p) {
                snap.insert(p, t);
            }
        }
        snap
    }
    fn initial_state(&self) -> State {
        State {
            snapshot: self.snapshot0.clone(),
            decls: vec![],
            outputs: vec![],
            documents: vec![],
        }
    }

    // ---- the log ----
    /// Apply an operation and log it; a refused operation is an error and is not
    /// logged. An operation after undo discards what was undone.
    pub fn apply(&mut self, op: Op) -> SResult<()> {
        self.log.truncate(self.cursor); // a new operation after :undo discards what was undone
        let mut st = std::mem::take(&mut self.state);
        let r = self.apply_to(&mut st, &op); // a refused operation errs and is not logged
        self.state = st;
        r?;
        self.log.push(op);
        self.cursor += 1;
        Ok(())
    }
    /// Step back `n` operations; returns how many were undone.
    pub fn undo(&mut self, n: usize) -> usize {
        let to = self.cursor.saturating_sub(n);
        let stepped = self.cursor - to;
        self.cursor = to;
        self.replay();
        stepped
    }
    /// Step forward `n` operations; returns how many were redone.
    pub fn redo(&mut self, n: usize) -> usize {
        let to = (self.cursor + n).min(self.log.len());
        let stepped = to - self.cursor;
        self.cursor = to;
        self.replay();
        stepped
    }
    fn replay(&mut self) {
        let mut st = self.initial_state();
        let ops: Vec<Op> = self.log[..self.cursor].to_vec();
        for op in &ops {
            let _ = self.apply_to(&mut st, op);
        }
        self.state = st;
    }
    /// The reload operation for the universe as it is on disk now.
    pub fn reload_op(&self) -> Op {
        Op::Reload {
            snapshot: self.snapshot_from_disk(),
        }
    }

    fn apply_to(&self, st: &mut State, op: &Op) -> SResult<()> {
        match op {
            Op::Bind { name, src } => {
                let (modules, _, _) = self.build(st);
                if !modules
                    .iter()
                    .any(|m| m.env.inputs.borrow().contains_key(name))
                {
                    return Err(SessionError::new(format!("no input named {name}")));
                }
                let (doc, origin, file) = match src {
                    BindSource::Expr { text } => (self.eval_to_doc(st, text)?, Origin::Expr, None),
                    BindSource::File { file, text } => {
                        (parse_doc(text, file)?, Origin::File, Some(file.clone()))
                    }
                    BindSource::Inline { text } => {
                        (parse_doc(text, "the document")?, Origin::Inline, None)
                    }
                };
                st.set_document(
                    name,
                    Document {
                        origin,
                        file,
                        base: doc.clone(),
                        doc,
                        edited: false,
                    },
                );
                Ok(())
            }
            Op::Unbind { name } => {
                if st.document(name).is_none() {
                    return Err(SessionError::new(format!("{name} is not bound")));
                }
                st.documents.retain(|(k, _)| k != name);
                Ok(())
            }
            Op::Edit { kind, path, expr } => self.edit(st, *kind, path, expr.as_deref()),
            Op::Declare { name, text } => {
                st.remove_decl(name);
                st.remove_output(name);
                st.decls.push((name.clone(), text.clone()));
                Ok(())
            }
            Op::Output { name, ty, expr } => {
                st.remove_decl(name);
                st.remove_output(name);
                st.outputs.push((name.clone(), ty.clone(), expr.clone()));
                Ok(())
            }
            Op::Drop { name } => {
                let a = st.remove_decl(name);
                let b = st.remove_output(name);
                if !a && !b {
                    return Err(SessionError::new(format!(
                        "no session declaration named {name}"
                    )));
                }
                Ok(())
            }
            Op::Reload { snapshot } => {
                st.snapshot = snapshot.clone();
                Ok(())
            }
            Op::Reset => {
                st.decls.clear();
                st.outputs.clear();
                st.documents.clear();
                Ok(())
            }
        }
    }

    // ---- documents and edits (§3) ----
    fn eval_to_doc(&self, st: &State, expr_text: &str) -> SResult<Value> {
        let expr = parse_expr(expr_text)?;
        let r = self.engine_for(st);
        if r.eng.is_none() || r.entry.is_none() {
            return Err(SessionError::new(self.load_failure(&r)));
        }
        let sc = Scope::new("", Some(r.entry.as_ref().unwrap().env.clone()));
        self.scratch(&r, |eng, _| {
            let result = (|| -> Result<Value, Fail> {
                let v = eng.ev(&expr, &sc)?;
                let v = eng.materialize(v, &[Seg::Name("_".into())])?;
                eng.force_all(&v);
                Ok(v)
            })();
            match result {
                Ok(v) => {
                    let text = eng.serialize(&v, "", false);
                    if text.is_empty() {
                        return Err(SessionError::new("the value is not data"));
                    }
                    read_json(&text).map_err(|_| SessionError::new("the value is not data"))
                }
                Err(Fail::Eval(e)) => Err(SessionError::new(e.msg)),
                Err(_) => Err(SessionError::new("the value is invalid")),
            }
        })
    }

    fn edit(&self, st: &mut State, kind: EditKind, path: &str, expr: Option<&str>) -> SResult<()> {
        let segs: SegPath =
            parse_path(path, "").map_err(|_| SessionError::new(format!("bad path {path}")))?;
        let root = match segs.first() {
            Some(Seg::Name(n)) if !n.is_empty() => n.clone(),
            _ => return Err(SessionError::new(format!("bad path {path}"))),
        };
        if segs.len() < 2 {
            return Err(SessionError::new(format!(
                "a path below a root is required, got {path}"
            )));
        }
        let value = match kind {
            EditKind::Remove => None,
            _ => Some(self.eval_to_doc(st, expr.unwrap_or(""))?),
        };
        self.document_of(st, &root)?;
        let doc = st.document(&root).cloned().unwrap();
        let new_doc = edit_value(&doc.doc, &segs, 1, kind, value.as_ref(), path)?;
        let d = st.document_mut(&root).unwrap();
        d.doc = new_doc;
        d.edited = true;
        Ok(())
    }

    // the document of a root, made if the root has none yet: an unbound
    // input's fallback, or an output detached into its settable projection
    fn document_of(&self, st: &mut State, root: &str) -> SResult<()> {
        if st.document(root).is_some() {
            return Ok(());
        }
        let (modules, _, _) = self.build(st);
        let input_mod = modules
            .iter()
            .any(|m| m.env.inputs.borrow().contains_key(root));
        let output_mod = modules
            .iter()
            .any(|m| m.env.outputs.borrow().iter().any(|(o, _, _)| o == root));
        if !input_mod && !output_mod {
            return Err(SessionError::new(if st.output(root).is_some() {
                format!("{root} is a session output; edit the roots it reads")
            } else {
                format!("no root named {root}")
            }));
        }
        let r = self.run_state(st, Mode::Full);
        let (Some(eng), Some(entry)) = (&r.eng, &r.entry) else {
            return Err(SessionError::new(self.load_failure(&r)));
        };
        let v = entry.env.root(root);
        let bad = v.is_none()
            || r.diags
                .iter()
                .any(|d| d.severity == "error" && is_root_diag(d, root));
        if bad {
            return Err(SessionError::new(format!(
                "{root} is invalid; fix it before editing"
            )));
        }
        let text = eng.serialize(&v.unwrap(), root, true);
        let doc = read_json(&text)
            .map_err(|_| SessionError::new(format!("{root} is invalid; fix it before editing")))?;
        st.set_document(
            root,
            Document {
                origin: if input_mod {
                    Origin::Fallback
                } else {
                    Origin::Detached
                },
                file: None,
                base: doc.clone(),
                doc,
                edited: false,
            },
        );
        Ok(())
    }

    // ---- building the universe ----
    fn build(&self, st: &State) -> (Vec<Rc<Module>>, Option<Rc<Module>>, Vec<Diag>) {
        let entry_abs = self.entry_abs();
        let mut overlay = st.snapshot.clone();
        let text = st.snapshot.get(&entry_abs).cloned().or_else(|| {
            if self.entry_path.is_none() {
                Some(String::new())
            } else {
                None
            }
        });
        if let Some(mut text) = text {
            let detached: Vec<String> = st
                .documents
                .iter()
                .filter(|(_, d)| d.origin == Origin::Detached)
                .map(|(n, _)| n.clone())
                .collect();
            text = detach_outputs(&text, &detached);
            let extra: Vec<&str> = st.decls.iter().map(|(_, t)| t.as_str()).collect();
            if !extra.is_empty() {
                if text.ends_with('\n') {
                    text.pop();
                }
                text.push('\n');
                text.push_str(&extra.join("\n"));
                text.push('\n');
            }
            overlay.insert(entry_abs.clone(), text);
        }
        let pkg = if self.entry_path.is_some() {
            open_package_universe(&entry_abs)
        } else {
            None
        };
        let mut diags: Vec<Diag> = vec![];
        if let Some(u) = &pkg {
            diags.extend(u.diags.clone());
            diags.extend(verify_lock(u));
        }
        let r = load_modules(
            &entry_abs,
            pkg.as_ref().map(|u| &u.resolver),
            Some(&overlay),
        );
        diags.extend(r.diags);
        (r.modules, r.entry, diags)
    }

    fn load_failure(&self, r: &Run) -> String {
        match r.load_diags.first() {
            Some(d) => format!(
                "{}{}",
                d.code
                    .as_ref()
                    .map(|c| format!("[{c}] "))
                    .unwrap_or_default(),
                d.message
            ),
            None => "the universe did not load".into(),
        }
    }

    // an inference context over the entry's scope in which the session's
    // outputs are variables of their inferred types, in declaration order
    fn session_ctx(
        &self,
        st: &State,
        env: &Rc<Env>,
        report: Rc<dyn Fn(&str, String)>,
        up_to: Option<&str>,
    ) -> Ctx {
        let mut cx = make_ctx(env.clone(), report);
        for (name, ty_text, expr_text) in &st.outputs {
            if Some(name.as_str()) == up_to {
                break;
            }
            let Ok(expr) = parse_expr(expr_text) else {
                continue;
            }; // a session output that does not parse is not in scope
            let mut quiet = make_ctx(env.clone(), Rc::new(|_, _| {}));
            quiet.vars = cx.vars.clone();
            let mut rt = infer(&quiet, &expr).rt;
            if let Some(t) = ty_text {
                if let Ok((d, _)) = parse_decl(&format!("output {name}: {t} = 0")) {
                    if let DeclBody::Output { ty, .. } = &d.body {
                        rt = env.resolve(ty, None).ok();
                    }
                }
            }
            cx.vars.insert(name.clone(), Ty { rt, abs: false });
        }
        cx
    }

    /// load, check, and (unless `mode` says otherwise) evaluate the state
    pub fn run(&self, mode: Mode) -> Run {
        self.run_state(&self.state, mode)
    }
    fn run_state(&self, st: &State, mode: Mode) -> Run {
        if mode == Mode::Full && !full_recompute() {
            if let Some(r) = self.step_from(st) {
                return r;
            }
        }
        let r = self.run_fresh(st, mode);
        if mode == Mode::Full {
            *self.last.borrow_mut() = if r.eng.is_some() {
                Some(Last {
                    key: self.universe_key(st),
                    docs: self.doc_keys(st),
                    run: r.clone(),
                })
            } else {
                None
            };
        }
        r
    }
    fn universe_key(&self, st: &State) -> String {
        let mut snap: Vec<(String, &String)> = st
            .snapshot
            .iter()
            .map(|(p, t)| (p.display().to_string(), t))
            .collect();
        snap.sort();
        let mut detached: Vec<&String> = st
            .documents
            .iter()
            .filter(|(_, d)| d.origin == Origin::Detached)
            .map(|(n, _)| n)
            .collect();
        detached.sort();
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}",
            self.entry_abs(),
            snap,
            st.decls,
            st.outputs,
            detached
        )
    }
    fn doc_keys(&self, st: &State) -> Vec<(String, String)> {
        st.documents
            .iter()
            .map(|(n, d)| (n.clone(), doc_json(&d.doc)))
            .collect()
    }
    // the incremental step: the same universe, some documents changed
    fn step_from(&self, st: &State) -> Option<Run> {
        let last = self.last.borrow().clone()?;
        let (Some(eng), Some(entry)) = (last.run.eng.clone(), last.run.entry.clone()) else {
            return None;
        };
        if last.key != self.universe_key(st) {
            return None;
        }
        let docs = self.doc_keys(st);
        let mut changed: Vec<String> = vec![];
        for (n, k) in &docs {
            if last.docs.iter().find(|(m, _)| m == n).map(|(_, v)| v) != Some(k) {
                changed.push(n.clone());
            }
        }
        for (n, _) in &last.docs {
            if !docs.iter().any(|(m, _)| m == n) {
                changed.push(n.clone());
            }
        }
        if changed.is_empty() {
            self.last_timing.set(Some(last.run.timing));
            return Some(last.run.clone());
        }
        let t0 = Instant::now();
        let r = &last.run;
        let env = entry.env.clone();
        // 1. what the change touches: the roots themselves, every slot under
        //    them, and the `$referrers` queries over types instantiated under them
        let mut seeds: Vec<String> = vec![];
        let read_keys: Vec<String> = eng.reads.borrow().keys().cloned().collect();
        let registry = env.registry_snapshot();
        for root in &changed {
            seeds.push(format!("root:{root}"));
            for k in &read_keys {
                if !k.starts_with("root:") && under(k.strip_prefix("assert:").unwrap_or(k), root) {
                    seeds.push(k.clone());
                }
            }
            for inst in &registry {
                let b = inst.borrow();
                if under(&path_str(&b.path, None), root) {
                    if let Some(tn) = &b.type_name {
                        seeds.push(format!("referrers:{tn}"));
                    }
                }
            }
        }
        // 2. everything that read them, transitively
        let mut readers: HashMap<String, HashSet<String>> = HashMap::new();
        for (reader, set) in eng.reads.borrow().iter() {
            for k in set {
                readers.entry(k.clone()).or_default().insert(reader.clone());
            }
        }
        let mut invalid: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = seeds;
        while let Some(k) = queue.pop() {
            if !invalid.insert(k.clone()) {
                continue;
            }
            if let Some(rs) = readers.get(&k) {
                for rd in rs {
                    if !invalid.contains(rd) {
                        queue.push(rd.clone());
                    }
                }
            }
        }
        // the roots to rebind: the changed ones, and every root that read them at binding
        let mut rebind: HashSet<String> = invalid
            .iter()
            .filter_map(|k| k.strip_prefix("root:").map(|s| s.to_string()))
            .collect();
        loop {
            let mut grew = false;
            for root in rebind.clone() {
                for inst in &registry {
                    let (p, tn) = {
                        let b = inst.borrow();
                        (path_str(&b.path, None), b.type_name.clone())
                    };
                    let Some(tn) = tn else { continue };
                    if !under(&p, &root) {
                        continue;
                    }
                    let rk = format!("referrers:{tn}");
                    if invalid.insert(rk.clone()) {
                        if let Some(rs) = readers.get(&rk) {
                            for rd in rs {
                                if !invalid.contains(rd) {
                                    invalid.insert(rd.clone());
                                    queue.push(rd.clone());
                                }
                            }
                        }
                    }
                }
            }
            while let Some(k) = queue.pop() {
                if let Some(rs) = readers.get(&k) {
                    for rd in rs {
                        if !invalid.contains(rd) {
                            invalid.insert(rd.clone());
                            queue.push(rd.clone());
                        }
                    }
                }
            }
            for k in &invalid {
                if let Some(r) = k.strip_prefix("root:") {
                    if rebind.insert(r.to_string()) {
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        // 3. forget: the diagnostics of the invalidated steps and of the rebound roots, the slots, the instances
        let gone = |d: &Diag| {
            d.by.as_ref().map(|b| invalid.contains(b)).unwrap_or(false)
                || rebind.iter().any(|root| under(&d.path, root))
        };
        env.diag_set(
            env.diagnostics_vec()
                .into_iter()
                .filter(|d| !gone(d))
                .collect(),
        );
        let mut recomputed = 0usize;
        for k in &invalid {
            if k.starts_with("root:") || k.starts_with("assert:") || k.starts_with("referrers:") {
                continue;
            }
            if rebind.iter().any(|root| under(k, root)) {
                eng.slots_by_key.borrow_mut().remove(k);
                eng.reads.borrow_mut().remove(k);
                continue;
            }
            if eng.reset_slot(k) {
                recomputed += 1;
            }
            eng.reads.borrow_mut().remove(k);
        }
        let mut dropped: Vec<crate::engine::Inst> = vec![];
        env.registry_retain(|inst| {
            let g = rebind
                .iter()
                .any(|root| under(&path_str(&inst.borrow().path, None), root));
            if g {
                dropped.push(inst.clone());
            }
            !g
        });
        for root in &rebind {
            env.remove_root(root);
            eng.failed_inputs.borrow_mut().remove(root);
            eng.reads.borrow_mut().remove(&format!("root:{root}"));
        }
        eng.deferred_slots
            .borrow_mut()
            .retain(|(i, _)| !dropped.iter().any(|d| Rc::ptr_eq(d, i)));
        let assert_keys: Vec<String> = eng
            .reads
            .borrow()
            .keys()
            .filter(|k| k.starts_with("assert:"))
            .cloned()
            .collect();
        for k in assert_keys {
            if rebind.iter().any(|root| under(&k[7..], root)) {
                eng.reads.borrow_mut().remove(&k);
            }
        }
        // 4. rebind the roots in the fresh run's order — the documents in the
        //    state's order, the modules' outputs in declaration order, the
        //    session's outputs — then force everything: what is `ok` stays
        //    (an unbound input is demanded through its fallback on first read)
        eng.set_phase(1);
        for (name, d) in &st.documents {
            if !rebind.contains(name) {
                continue;
            }
            let m = r
                .modules
                .iter()
                .find(|x| x.env.inputs.borrow().contains_key(name))
                .cloned()
                .unwrap_or_else(|| entry.clone());
            let decl = m.env.inputs.borrow().get(name).cloned();
            let Some((ty_ast, _)) = decl else { continue };
            let sc = Scope::new(name, Some(m.env.clone()));
            match m.env.resolve(&ty_ast, None) {
                Ok(rt) => eng.bind_root(name, RootSrc::Doc(d.doc.clone()), &rt, &sc),
                Err(e) => env.report(Diag::error(e, name.clone(), None)),
            }
        }
        for m in &r.modules {
            let outs = m.env.outputs.borrow().clone();
            for (name, ty_ast, expr) in outs {
                if !rebind.contains(&name) {
                    continue;
                }
                let sc = Scope::new(&name, Some(m.env.clone()));
                match m.env.resolve(&ty_ast, None) {
                    Ok(rt) => eng.bind_root(&name, RootSrc::Expr(&expr), &rt, &sc),
                    Err(e) => env.report(Diag::error(e, name.clone(), None)),
                }
            }
        }
        for (name, expr, rt) in &r.session_roots {
            if !rebind.contains(name) {
                continue;
            }
            let sc = Scope::new(name, Some(entry.env.clone()));
            eng.bind_root(name, RootSrc::Expr(expr), rt, &sc);
        }
        eng.force_roots(&env);
        eng.set_phase(2);
        let mut i = 0;
        loop {
            let item = {
                let d = eng.deferred_slots.borrow();
                if i >= d.len() {
                    break;
                }
                d[i].clone()
            };
            eng.force_slot_safe(&item.0, &item.1);
            i += 1;
        }
        eng.bind_deferred_roots();
        eng.force_roots(&env);
        // 5. the asserts of the instances that are new or whose asserts read what changed
        for inst in env.registry_snapshot() {
            let key = format!("assert:{}", path_str(&inst.borrow().path, None));
            let fresh = !eng.reads.borrow().contains_key(&key);
            if fresh || invalid.contains(&key) {
                eng.validate_inst(&inst, "");
            }
        }
        let sorted = sort_diags(env.diagnostics_vec());
        env.diag_set(sorted.clone());
        let elapsed = ms(t0);
        let timing = Timing {
            load: 0.0,
            check: 0.0,
            bind: 0.0,
            evaluate: elapsed,
            total: elapsed,
            recomputed: Some(recomputed),
            slots: Some(eng.slots_by_key.borrow().len()),
        };
        let run = Run {
            diags: sorted,
            timing,
            ..r.clone()
        };
        *self.last.borrow_mut() = Some(Last {
            key: last.key.clone(),
            docs,
            run: run.clone(),
        });
        self.last_timing.set(Some(timing));
        Some(run)
    }
    fn run_fresh(&self, st: &State, mode: Mode) -> Run {
        let t0 = Instant::now();
        let (modules, entry, load_diags) = self.build(st);
        let load = ms(t0);
        let mut out = Run {
            modules,
            entry,
            load_diags,
            checks: vec![],
            session_checks: vec![],
            session_roots: vec![],
            eng: None,
            diags: vec![],
            timing: Timing {
                load,
                check: 0.0,
                bind: 0.0,
                evaluate: 0.0,
                total: 0.0,
                recomputed: None,
                slots: None,
            },
        };
        let finish = |mut out: Run| -> Run {
            out.timing.total = ms(t0);
            self.last_timing.set(Some(out.timing));
            out
        };
        if !out.load_diags.is_empty() || out.entry.is_none() {
            return finish(out);
        }
        let entry = out.entry.clone().unwrap();
        let t1 = Instant::now();
        for m in &out.modules {
            for d in check_module(&m.decls, Some(m.env.clone()), None) {
                out.checks.push((m.path.display().to_string(), d));
            }
        }
        // session outputs: their expressions are inferred where a declared
        // output's would be checked; the inferred type is the root's type
        let mut session_roots: Vec<(String, Rc<Expr>, RT)> = vec![];
        for (name, ty_text, expr_text) in &st.outputs {
            let taken = out.modules.iter().any(|m| {
                m.env.inputs.borrow().contains_key(name)
                    || m.env.outputs.borrow().iter().any(|(o, _, _)| o == name)
            });
            if taken {
                out.session_checks.push(Diag {
                    severity: "error".into(),
                    id: None,
                    code: Some("E3018".into()),
                    message: format!("root {name} is already declared by the universe"),
                    path: name.clone(),
                    loc: None,
                    by: None,
                });
                continue;
            }
            let expr = match parse_expr(expr_text) {
                Ok(e) => e,
                Err(e) => {
                    out.session_checks.push(Diag {
                        severity: "error".into(),
                        id: None,
                        code: None,
                        message: e.0,
                        path: name.clone(),
                        loc: None,
                        by: None,
                    });
                    continue;
                }
            };
            let sink: Rc<RefCell<Vec<Diag>>> = Rc::new(RefCell::new(vec![]));
            let sink2 = sink.clone();
            let n2 = name.clone();
            let cx = self.session_ctx(
                st,
                &entry.env,
                Rc::new(move |code, msg| {
                    sink2.borrow_mut().push(Diag {
                        severity: "error".into(),
                        id: None,
                        code: Some(code.to_string()),
                        message: msg,
                        path: n2.clone(),
                        loc: None,
                        by: None,
                    })
                }),
                Some(name),
            );
            let ty = infer(&cx, &expr);
            let found: Vec<Diag> = sink.borrow().clone();
            if !found.is_empty() {
                out.session_checks.extend(found);
                continue;
            }
            let rt: RT = match ty_text {
                Some(t) => {
                    let resolved =
                        parse_decl(&format!("output {name}: {t} = 0")).and_then(|(d, _)| match &d
                            .body
                        {
                            DeclBody::Output { ty, .. } => {
                                entry.env.resolve(ty, None).map_err(SessionError::new)
                            }
                            _ => Err(SessionError::new("not an output")),
                        });
                    match resolved {
                        Ok(rt) => rt,
                        Err(e) => {
                            out.session_checks.push(Diag {
                                severity: "error".into(),
                                id: None,
                                code: None,
                                message: e.0,
                                path: name.clone(),
                                loc: None,
                                by: None,
                            });
                            continue;
                        }
                    }
                }
                None => ty.rt.unwrap_or_else(|| crate::semantics::ty(RTk::Any)),
            };
            session_roots.push((name.clone(), expr, rt));
        }
        out.timing.check = ms(t1);
        // a static error in a module stops full evaluation as it stops `decl
        // evaluate`; a session output that does not check is left out, and a
        // bare expression (lazy) evaluates over what loaded regardless
        if mode == Mode::Check
            || (mode == Mode::Full && out.checks.iter().any(|(_, d)| d.severity == "error"))
        {
            return finish(out);
        }

        let t2 = Instant::now();
        let eng = Engine::new(entry.env.clone());
        for m in &out.modules {
            eng.install_hooks(&m.env, true);
        }
        // documents first (an output may read an input, §5.5), then the
        // modules' outputs, then the session's
        for (name, d) in &st.documents {
            let m = out
                .modules
                .iter()
                .find(|x| x.env.inputs.borrow().contains_key(name))
                .cloned()
                .unwrap_or_else(|| entry.clone());
            let decl = m.env.inputs.borrow().get(name).cloned();
            let Some((ty_ast, _)) = decl else { continue };
            let sc = Scope::new(name, Some(m.env.clone()));
            match m.env.resolve(&ty_ast, None) {
                Ok(rt) => eng.bind_root(name, RootSrc::Doc(d.doc.clone()), &rt, &sc),
                Err(e) => entry.env.report(Diag::error(e, name.clone(), None)),
            }
        }
        for m in &out.modules {
            let outs = m.env.outputs.borrow().clone();
            for (name, ty_ast, expr) in outs {
                let sc = Scope::new(&name, Some(m.env.clone()));
                match m.env.resolve(&ty_ast, None) {
                    Ok(rt) => eng.bind_root(&name, RootSrc::Expr(&expr), &rt, &sc),
                    Err(e) => entry.env.report(Diag::error(e, name.clone(), None)),
                }
            }
        }
        for (name, expr, rt) in &session_roots {
            let sc = Scope::new(name, Some(entry.env.clone()));
            eng.bind_root(name, RootSrc::Expr(expr), rt, &sc);
        }
        out.session_roots = session_roots;
        out.eng = Some(eng.clone());
        out.timing.bind = ms(t2);
        if mode == Mode::Lazy {
            eng.set_phase(2);
            out.diags = entry.env.diagnostics_vec();
            return finish(out);
        }
        let t3 = Instant::now();
        eng.drive(&entry.env);
        out.diags = entry.env.diagnostics_vec(); // sorted by drive's caller? no: sorted here (§6.7)
        let sorted = sort_diags(out.diags.clone());
        entry.env.diag_set(sorted.clone());
        out.diags = sorted;
        out.timing.evaluate = ms(t3);
        finish(out)
    }

    // ---- questions ----
    // the engine an expression evaluates over: the last full run's when the
    // universe evaluates (complete, so `$referrers` answers over every
    // instance, and nothing is rebuilt), else a lazy run's (bound, unforced)
    fn engine_for(&self, st: &State) -> Run {
        let full = self.run_state(st, Mode::Full);
        if full.eng.is_some() {
            full
        } else {
            self.run_state(st, Mode::Lazy)
        }
    }
    // evaluate `f` over the run's engine and leave the run as it was: the
    // diagnostics the expression added and the instances it materialized
    // under `_` are removed, forced slots keep the values a full run gives
    fn scratch<T>(&self, r: &Run, f: impl FnOnce(&Rc<Engine>, &Rc<Env>) -> T) -> T {
        let (eng, env) = (r.eng.clone().unwrap(), r.entry.clone().unwrap().env.clone());
        let n = env.diag_len();
        let reg = env.registry_snapshot().len();
        let roots: HashSet<String> = env.root_names().into_iter().collect();
        let out = f(&eng, &env);
        // an input demanded through its fallback by the expression alone is not a root of the run
        let demanded: Vec<String> = env
            .root_names()
            .into_iter()
            .filter(|k| !roots.contains(k))
            .collect();
        let under_demanded = |p: &str| demanded.iter().any(|k| under(p, k));
        for k in &demanded {
            env.remove_root(k);
            eng.failed_inputs.borrow_mut().remove(k);
            eng.reads.borrow_mut().remove(&format!("root:{k}"));
        }
        let keys: Vec<String> = eng.reads.borrow().keys().cloned().collect();
        for k in keys {
            if under_demanded(k.strip_prefix("assert:").unwrap_or(&k)) {
                eng.reads.borrow_mut().remove(&k);
            }
        }
        let skeys: Vec<String> = eng.slots_by_key.borrow().keys().cloned().collect();
        for k in skeys {
            if under_demanded(&k) {
                eng.slots_by_key.borrow_mut().remove(&k);
            }
        }
        env.diag_truncate(n);
        let mut i = 0usize;
        env.registry_retain(|inst| {
            let idx = i;
            i += 1;
            let p = path_str(&inst.borrow().path, None);
            let scratch_root = matches!(inst.borrow().path.first(), Some(Seg::Name(n)) if n == "_");
            if idx < reg {
                !under_demanded(&p)
            } else {
                !scratch_root && !under_demanded(&p)
            }
        });
        eng.computing.borrow_mut().clear();
        out
    }

    // ---- questions ----
    /// partial evaluation of one expression (§2.1)
    pub fn evaluate_expr(&self, text: &str) -> SResult<ExprResult> {
        let expr = parse_expr(text)?;
        let r = self.engine_for(&self.state);
        if r.eng.is_none() || r.entry.is_none() {
            return Ok(ExprResult {
                value: None,
                diags: r.load_diags.clone(),
                error: Some((None, String::new())),
            });
        }
        let entry_env = r.entry.as_ref().unwrap().env.clone();
        let sc = Scope::new("", Some(entry_env.clone()));
        let named = identifiers(text);
        Ok(self.scratch(&r, |eng, env| {
            // the run may already have reported (a root whose binding failed); the
            // expression's own diagnostics are the ones that arise from here on,
            // plus the diagnostics of the roots it names
            let from = env.diag_len();
            let arising = |env: &Env| -> Vec<Diag> {
                let all = env.diagnostics_vec();
                let mut out: Vec<Diag> = all[..from.min(all.len())]
                    .iter()
                    .filter(|d| named.contains(&d.path))
                    .cloned()
                    .collect();
                out.extend(all[from.min(all.len())..].iter().cloned());
                sort_diags(out)
            };
            let result = (|| -> Result<Value, Fail> {
                let v = eng.ev(&expr, &sc)?;
                let v = eng.materialize(v, &[Seg::Name("_".into())])?;
                eng.force_all(&v);
                Ok(v)
            })();
            match result {
                Ok(v) => ExprResult {
                    value: Some(self.value_text(eng, &v)),
                    diags: arising(env),
                    error: None,
                },
                Err(Fail::Eval(e)) => ExprResult {
                    value: None,
                    diags: arising(env),
                    error: Some((e.code, e.msg)),
                },
                Err(_) => ExprResult {
                    value: None,
                    diags: arising(env),
                    error: Some((None, String::new())),
                },
            }
        }))
    }
    fn value_text(&self, eng: &Engine, v: &Value) -> String {
        match v {
            Value::Absent | Value::Undef => "absent".into(),
            Value::Clo(_) | Value::Nat(_) | Value::Std(_) => "<function>".into(),
            Value::NsRef(_) => "<namespace>".into(),
            Value::Pat(re) => format!("/{re}/"),
            _ => eng.serialize(v, "", false),
        }
    }

    /// the roots of the universe and of the session (`:roots`)
    pub fn roots(&self) -> Vec<RootInfo> {
        let (modules, _, _) = self.build(&self.state);
        let entry_abs = self.entry_abs();
        let entry_dir = entry_abs
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let rel = |p: &Path| {
            if p == entry_abs {
                self.entry_name()
            } else {
                relative_path(&entry_dir, p)
            }
        };
        let mut out = vec![];
        for m in &modules {
            // the module's roots in declaration order, from its text as loaded
            // (a detached output is blanked from the universe but still a root)
            let parsed;
            let decls: &Vec<Decl> = if m.path == entry_abs {
                parsed = parse_source(
                    self.state
                        .snapshot
                        .get(&m.path)
                        .map(|s| s.as_str())
                        .unwrap_or(""),
                )
                .decls;
                &parsed
            } else {
                &m.decls
            };
            for decl in decls {
                match &decl.body {
                    DeclBody::Output { name, .. } => {
                        let d = self.state.document(name);
                        out.push(RootInfo {
                            kind: "output",
                            name: name.clone(),
                            module: rel(&m.path),
                            exported: decl.exported,
                            session: false,
                            binding: if d.map(|d| d.origin) == Some(Origin::Detached) {
                                "detached".into()
                            } else {
                                String::new()
                            },
                            detail: String::new(),
                            edited: d.map(|d| d.edited).unwrap_or(false),
                        });
                    }
                    DeclBody::Input { name, fallback, .. } => {
                        let d = self.state.document(name);
                        let binding = match d {
                            Some(d) if d.origin == Origin::Fallback => "fallback",
                            Some(_) => "bound",
                            None => {
                                if fallback.is_some() {
                                    "fallback"
                                } else {
                                    "unbound"
                                }
                            }
                        };
                        let detail = match d {
                            Some(d) => match d.origin {
                                Origin::File => d.file.clone().unwrap_or_default(),
                                Origin::Inline => "(inline)".into(),
                                Origin::Expr => "(expression)".into(),
                                _ => String::new(),
                            },
                            None => String::new(),
                        };
                        out.push(RootInfo {
                            kind: "input",
                            name: name.clone(),
                            module: rel(&m.path),
                            exported: false,
                            session: false,
                            binding: binding.into(),
                            detail,
                            edited: d.map(|d| d.edited).unwrap_or(false),
                        });
                    }
                    _ => {}
                }
            }
        }
        for (name, _, _) in &self.state.outputs {
            out.push(RootInfo {
                kind: "output",
                name: name.clone(),
                module: String::new(),
                exported: false,
                session: true,
                binding: String::new(),
                detail: String::new(),
                edited: false,
            });
        }
        out
    }
    /// Every root's name, the session's included.
    pub fn all_root_names(&self) -> Vec<String> {
        self.roots().into_iter().map(|r| r.name).collect()
    }
    /// Whether a root of that name exists.
    pub fn has_root(&self, name: &str) -> bool {
        self.all_root_names().iter().any(|n| n == name)
    }

    /// static diagnostics of every module, with the file each is reported against
    pub fn check(&self) -> Vec<(String, Diag)> {
        let r = self.run(Mode::Check);
        let entry = self.entry_abs().display().to_string();
        let mut out: Vec<(String, Diag)> = r
            .load_diags
            .iter()
            .map(|d| (entry.clone(), d.clone()))
            .collect();
        out.extend(r.checks);
        out.extend(r.session_checks.into_iter().map(|d| (entry.clone(), d)));
        out
    }

    /// full evaluation of the named roots (`:evaluate`), or of the exported outputs
    pub fn evaluate(
        &self,
        names: &[String],
    ) -> SResult<(Run, Vec<(String, Option<String>)>, bool)> {
        let r = self.run(Mode::Full);
        let exported = names.is_empty();
        let Some(entry) = r.entry.clone() else {
            return Ok((r, vec![], exported));
        };
        let want: Vec<String> = if names.is_empty() {
            entry
                .decls
                .iter()
                .filter(|d| d.exported)
                .filter_map(|d| match &d.body {
                    DeclBody::Output { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect()
        } else {
            names.to_vec()
        };
        for n in names {
            if !self.has_root(n) {
                return Err(SessionError::new(format!("no root named {n}")));
            }
        }
        let Some(eng) = r.eng.clone() else {
            return Ok((r, want.into_iter().map(|n| (n, None)).collect(), exported));
        };
        let mut docs = vec![];
        for name in want {
            let v = entry.env.root(&name);
            let bad = v.is_none()
                || r.diags
                    .iter()
                    .any(|d| d.severity == "error" && is_root_diag(d, &name));
            let json = if bad {
                None
            } else {
                Some(eng.serialize(&v.unwrap(), &name, false))
            };
            docs.push((name, json));
        }
        Ok((r, docs, exported))
    }

    /// whole-document validation of the named roots (`:validate`), or of every root
    pub fn validate(
        &self,
        names: &[String],
    ) -> SResult<(Run, Vec<(String, usize, usize)>, Vec<Diag>)> {
        for n in names {
            if !self.has_root(n) {
                return Err(SessionError::new(format!("no root named {n}")));
            }
        }
        let r = self.run(Mode::Full);
        let want: Vec<String> = if !names.is_empty() {
            names.to_vec()
        } else if let Some(entry) = &r.entry {
            entry
                .env
                .roots
                .borrow()
                .borrow()
                .iter()
                .map(|(n, _)| n.clone())
                .collect()
        } else {
            vec![]
        };
        let diags: Vec<Diag> = r
            .diags
            .iter()
            .filter(|d| {
                want.iter().any(|n| is_root_diag(d, n)) || (d.path.is_empty() && names.is_empty())
            })
            .cloned()
            .collect();
        let verdicts = want
            .iter()
            .map(|name| {
                let errors = r
                    .diags
                    .iter()
                    .filter(|d| d.severity == "error" && is_root_diag(d, name))
                    .count()
                    + if r
                        .entry
                        .as_ref()
                        .map(|e| e.env.root(name).is_some())
                        .unwrap_or(false)
                    {
                        0
                    } else if r.eng.is_some() {
                        1
                    } else {
                        0
                    };
                let warnings = r
                    .diags
                    .iter()
                    .filter(|d| d.severity == "warn" && is_root_diag(d, name))
                    .count();
                (name.clone(), errors, warnings)
            })
            .collect();
        Ok((r, verdicts, diags))
    }

    /// the static type of an expression (`:type`)
    pub fn type_of(&self, text: &str) -> SResult<(String, bool, Vec<Diag>)> {
        let expr = parse_expr(text)?;
        let (_, entry, diags) = self.build(&self.state);
        let Some(entry) = entry else {
            return Err(SessionError::new(
                diags
                    .first()
                    .map(|d| d.message.clone())
                    .unwrap_or_else(|| "the universe did not load".into()),
            ));
        };
        let sink: Rc<RefCell<Vec<Diag>>> = Rc::new(RefCell::new(vec![]));
        let sink2 = sink.clone();
        let cx = self.session_ctx(
            &self.state,
            &entry.env,
            Rc::new(move |code, message| {
                sink2.borrow_mut().push(Diag {
                    severity: "error".into(),
                    id: None,
                    code: Some(code.to_string()),
                    message,
                    path: String::new(),
                    loc: None,
                    by: None,
                })
            }),
            None,
        );
        let ty = infer(&cx, &expr);
        let found = sink.borrow().clone();
        Ok((type_text(ty.rt.as_ref()), ty.abs, found))
    }

    /// the canonical path of the place a navigation names (`:path`)
    pub fn path_of(&self, text: &str) -> SResult<String> {
        let expr = parse_expr(text)?;
        let r = self.engine_for(&self.state);
        if r.eng.is_none() || r.entry.is_none() {
            return Err(SessionError::new(self.load_failure(&r)));
        }
        let entry = r.entry.clone().unwrap();
        let sc = Scope::new("", Some(entry.env.clone()));
        self.scratch(&r, |eng, _| {
            let result = (|| -> Result<Option<SegPath>, Fail> {
                let mut segs = eng.eval_place(&expr, &sc)?;
                // a scalar member or element is a place too: its container's place, one step down
                if segs.is_none() {
                    match &*expr {
                        Expr::Member { x, name, .. } => {
                            if let Some(mut base) = eng.eval_place(x, &sc)? {
                                base.push(Seg::Name(name.clone()));
                                segs = Some(base);
                            }
                        }
                        Expr::Index { x, i } => {
                            if let Some(mut base) = eng.eval_place(x, &sc)? {
                                let iv = eng.ev(i, &sc)?;
                                base.push(match iv {
                                    Value::Int(n) => Seg::Idx(n.to_string().parse().unwrap_or(0)),
                                    Value::Str(s) => Seg::Key(s),
                                    other => Seg::Key(crate::infer::js_str(&other)),
                                });
                                segs = Some(base);
                            }
                        }
                        Expr::Name(n) if entry.env.root(n).is_some() => {
                            segs = Some(vec![Seg::Name(n.clone())])
                        }
                        _ => {}
                    }
                }
                Ok(segs)
            })();
            match result {
                Ok(Some(segs)) => Ok(path_str(&segs, None)),
                Ok(None) => Err(SessionError::new("the expression does not name a place")),
                Err(Fail::Eval(e)) => Err(SessionError::new(e.msg)),
                Err(_) => Err(SessionError::new("the place is invalid")),
            }
        })
    }

    /// the declaration a name resolves to, with its documentation (`:doc`)
    pub fn doc_of(&self, name: &str) -> SResult<Vec<String>> {
        let mut parts = name.splitn(2, '.');
        let head = parts.next().unwrap_or("");
        let member = parts.next();
        // a session declaration first
        if member.is_none() {
            if let Some(t) = self.state.decl(head) {
                return Ok(t.split('\n').map(|s| s.to_string()).collect());
            }
            if let Some((ty, expr)) = self.state.output(head) {
                return Ok(vec![format!(
                    "{head}{} = {expr}",
                    ty.as_ref().map(|t| format!(": {t}")).unwrap_or_default()
                )]);
            }
        }
        let (modules, entry, diags) = self.build(&self.state);
        let Some(entry) = entry else {
            return Err(SessionError::new(
                diags
                    .first()
                    .map(|d| d.message.clone())
                    .unwrap_or_else(|| "the universe did not load".into()),
            ));
        };
        let mut module: Option<Rc<Module>> = Some(entry.clone());
        let mut target = head.to_string();
        if !entry.decls.iter().any(|d| d.name() == Some(head)) {
            let im = entry.env.imports.borrow().get(head).cloned();
            match im {
                Some(im) => {
                    module = modules
                        .iter()
                        .find(|m| Rc::ptr_eq(&m.env, &im.env))
                        .cloned();
                    target = im.name.clone();
                }
                None => module = None,
            }
        }
        let decl = module.as_ref().and_then(|m| {
            m.decls
                .iter()
                .find(|d| d.name() == Some(target.as_str()) && d.loc.is_some())
                .cloned()
        });
        let (Some(module), Some(decl)) = (module, decl) else {
            return Err(SessionError::new(format!("no declaration named {head}")));
        };
        let text = self
            .state
            .snapshot
            .get(&module.path)
            .cloned()
            .unwrap_or_default();
        let lines: Vec<&str> = text.split('\n').collect();
        let loc = decl.loc.unwrap();
        let mut from = loc.sl;
        let mut doc_lines: Vec<String> = vec![];
        let is_doc = |l: &str| l.trim_start().starts_with("///");
        while from > 0 && is_doc(lines[from - 1]) {
            from -= 1;
            doc_lines.insert(0, lines[from].to_string());
        }
        let body: Vec<&str> = lines[loc.sl..=loc.el.min(lines.len() - 1)].to_vec();
        if let Some(member) = member {
            let re = Regex::new(&format!(r"^\s*{}\$?\??\s*[:=]", regex::escape(member))).unwrap();
            let mut picked: Vec<String> = vec![];
            for (i, l) in body.iter().enumerate() {
                if re.is_match(l) {
                    let mut j = i;
                    let mut ds: Vec<String> = vec![];
                    while j > 0 && is_doc(body[j - 1]) {
                        j -= 1;
                        ds.insert(0, body[j].trim().to_string());
                    }
                    picked.extend(ds);
                    picked.push(l.trim().to_string());
                }
            }
            if picked.is_empty() {
                return Err(SessionError::new(format!("{head} has no member {member}")));
            }
            return Ok(picked);
        }
        doc_lines.extend(body.iter().map(|s| s.to_string()));
        Ok(doc_lines)
    }

    /// the derivation of a valid place, or the root cause of an invalid one (`:trace`)
    pub fn trace(&self, path_text: &str) -> SResult<Vec<String>> {
        let segs: SegPath = parse_path(path_text, "")
            .map_err(|_| SessionError::new(format!("bad path {path_text}")))?;
        let root = seg_text(&segs[0]);
        if !self.has_root(&root) {
            return Err(SessionError::new(format!("no root named {root}")));
        }
        let r = self.run(Mode::Full);
        let (Some(eng), Some(entry)) = (r.eng.clone(), r.entry.clone()) else {
            return Err(SessionError::new(self.load_failure(&r)));
        };
        let mut lines: Vec<String> = vec![];
        let mut seen: HashSet<String> = HashSet::new();
        let has_doc = self.state.document(&root).is_some();
        self.walk(&mut lines, &mut seen, &r, &eng, &entry, &segs, 0, has_doc);
        Ok(lines)
    }
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        lines: &mut Vec<String>,
        seen: &mut HashSet<String>,
        r: &Run,
        eng: &Rc<Engine>,
        entry: &Rc<Module>,
        segs: &[Seg],
        depth: usize,
        has_doc: bool,
    ) {
        let short = |v: &Value| {
            let t = self.value_text(eng, v);
            if t.chars().count() > 60 {
                format!("{}...", t.chars().take(57).collect::<String>())
            } else {
                t
            }
        };
        let path = path_str(segs, None);
        let ind = "  ".repeat(depth);
        if seen.contains(&path) {
            lines.push(format!("{ind}{path}  (above)"));
            return;
        }
        seen.insert(path.clone());
        let own: Vec<&Diag> = r.diags.iter().filter(|d| d.path == path).collect();
        let parent = if segs.len() > 1 {
            self.value_at(eng, entry, &segs[..segs.len() - 1])
        } else {
            None
        };
        let last = segs.last().unwrap();
        let slot_info = match (&parent, last) {
            (Some(Value::Rec(inst)), Seg::Name(n)) => {
                let b = inst.borrow();
                b.slot(n).map(|s| {
                    (
                        s.kind,
                        s.state,
                        s.value.clone(),
                        b.entry_order.contains(n),
                        rec_members(&b.rt).into_iter().find(|m| &m.name == n),
                    )
                })
            }
            _ => None,
        };
        if let Some((kind, state, value, in_entry, m)) = slot_info {
            let inst = match &parent {
                Some(Value::Rec(i)) => i.clone(),
                _ => unreachable!(),
            };
            let kind_word = match kind {
                MKind::Der => "derived",
                MKind::Dflt => "defaulted",
                MKind::Opt => "optional",
                MKind::Req => "required",
            };
            let supplied =
                matches!(kind, MKind::Req | MKind::Opt) || (kind == MKind::Dflt && in_entry);
            let m_expr = m.as_ref().and_then(|m| m.expr.clone());
            if state == SlotState::Invalid {
                lines.push(format!("{ind}{path}  (invalid)"));
                for d in &own {
                    lines.push(format!("{ind}  {}", fmt_diag(d, None)));
                }
                if own.is_empty() {
                    if let Some(e) = &m_expr {
                        for rd in reads_of(e) {
                            if let Some(s) = self.read_segs(eng, &inst, &rd, entry) {
                                self.walk(lines, seen, r, eng, entry, &s, depth + 1, has_doc);
                            }
                        }
                    }
                }
                return;
            }
            if state == SlotState::Absent {
                lines.push(format!("{ind}{path}  absent"));
                return;
            }
            let how = if supplied {
                "supplied".to_string()
            } else {
                kind_word.to_string()
            };
            let ex = match (&m_expr, supplied) {
                (Some(e), false) => format!(": {}", expr_text(e)),
                _ => String::new(),
            };
            lines.push(format!("{ind}{path} = {}  ({how}{ex})", short(&value)));
            if !supplied && depth < 6 {
                if let Some(e) = &m_expr {
                    for rd in reads_of(e) {
                        match self.read_segs(eng, &inst, &rd, entry) {
                            Some(s) => {
                                self.walk(lines, seen, r, eng, entry, &s, depth + 1, has_doc)
                            }
                            None => lines.push(format!("{ind}  {}  (not a place)", expr_text(&rd))),
                        }
                    }
                }
            }
            return;
        }
        match self.value_at(eng, entry, segs) {
            None => {
                if r.diags
                    .iter()
                    .any(|d| d.severity == "error" && is_root_diag(d, &path))
                {
                    lines.push(format!("{ind}{path}  (invalid)"));
                    for d in r.diags.iter().filter(|d| is_root_diag(d, &path)) {
                        lines.push(format!("{ind}  {}", fmt_diag(d, None)));
                    }
                } else {
                    lines.push(format!("{ind}{path}  nothing there"));
                }
            }
            Some(v) => {
                let how = if segs.len() == 1 {
                    if has_doc {
                        "document"
                    } else {
                        "root literal"
                    }
                } else {
                    "supplied"
                };
                lines.push(format!("{ind}{path} = {}  ({how})", short(&v)));
                for d in &own {
                    lines.push(format!("{ind}  {}", fmt_diag(d, None)));
                }
            }
        }
    }
    fn value_at(&self, eng: &Engine, entry: &Module, segs: &[Seg]) -> Option<Value> {
        let mut v = entry.env.root(&seg_text(&segs[0]))?;
        for s in &segs[1..] {
            v = eng.deref(v).ok()?;
            v = match (&v, s) {
                (Value::Rec(inst), Seg::Name(n)) => {
                    let st = eng.force_state(inst, n);
                    if st == SlotState::Ok {
                        inst.borrow().slot(n).map(|s| s.value.clone())?
                    } else {
                        return None;
                    }
                }
                (Value::Rec(_), _) => return None,
                (Value::Arr(a), Seg::Idx(i)) => a.borrow().items.get(*i).cloned()?,
                (Value::Arr(_), _) => return None,
                (Value::Map(m), _) => m.borrow().get(&seg_text(s)).cloned()?,
                _ => return None,
            };
            if v.is_undef() || v.is_absent() {
                return None;
            }
        }
        Some(v)
    }
    fn read_segs(
        &self,
        eng: &Engine,
        inst: &Rc<RefCell<crate::semantics::RecInst>>,
        rd: &Rc<Expr>,
        entry: &Module,
    ) -> Option<SegPath> {
        // a bare name read inside a record is a sibling member (§4.4's scope
        // chain), else a root; a chain is navigated to the place it names
        if let Expr::Name(n) = &**rd {
            let mut cur = Some(inst.clone());
            while let Some(c) = cur {
                if c.borrow().has_slot(n) {
                    let mut p = c.borrow().path.clone();
                    p.push(Seg::Name(n.clone()));
                    return Some(p);
                }
                cur = c.borrow().parent.clone();
            }
            return if entry.env.root(n).is_some() {
                Some(vec![Seg::Name(n.clone())])
            } else {
                None
            };
        }
        let root_name = inst.borrow().path.first().map(seg_text).unwrap_or_default();
        let sc = Scope {
            inst: Some(inst.clone()),
            locals: Rc::new(HashMap::new()),
            root_name,
            menv: Some(entry.env.clone()),
        };
        eng.eval_place(rd, &sc).ok().flatten()
    }

    /// the candidates completion offers at the end of `text` (`:complete`)
    pub fn complete(&self, text: &str, commands: &[&str]) -> Vec<String> {
        let uniq = |xs: Vec<String>| -> Vec<String> {
            let mut v: Vec<String> = xs.into_iter().collect::<HashSet<_>>().into_iter().collect();
            v.sort();
            v
        };
        if text.starts_with(':') {
            let Some(sp) = text.find(' ') else {
                return uniq(
                    commands
                        .iter()
                        .filter(|c| c.starts_with(text))
                        .map(|c| c.to_string())
                        .collect(),
                );
            };
            let cmd = &text[..sp];
            let rest = &text[sp + 1..];
            let last = Regex::new(r"[\s,=]+")
                .unwrap()
                .split(rest)
                .last()
                .unwrap_or("")
                .to_string();
            let by =
                |xs: Vec<String>| uniq(xs.into_iter().filter(|x| x.starts_with(&last)).collect());
            return match cmd {
                ":evaluate" | ":validate" | ":unbind" | ":diff" | ":save" | ":bind" => {
                    by(self.all_root_names())
                }
                ":drop" => by(self
                    .state
                    .decls
                    .iter()
                    .map(|(n, _)| n.clone())
                    .chain(self.state.outputs.iter().map(|(n, _, _)| n.clone()))
                    .collect()),
                ":set" => by(vec!["pretty".into(), "compact".into()]),
                ":help" => by(commands.iter().map(|c| c.to_string()).collect()),
                ":trace" | ":path" | ":create" | ":update" | ":remove" => self.complete_path(&last),
                _ => vec![],
            };
        }
        let member_re = Regex::new(
            r"([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_][A-Za-z0-9_$]*|\[[^\]]*\])*)\.([A-Za-z_]*)$",
        )
        .unwrap();
        if let Some(m) = member_re.captures(text) {
            let base = m.get(1).unwrap().as_str();
            let prefix = m.get(2).unwrap().as_str();
            if base == "std" || base.starts_with("std.") {
                let ns = if base == "std" {
                    String::new()
                } else {
                    format!("{}.", &base[4..])
                };
                return uniq(
                    std_names()
                        .filter(|k| k.starts_with(&ns))
                        .map(|k| k[ns.len()..].split('.').next().unwrap_or("").to_string())
                        .filter(|k| k.starts_with(prefix))
                        .collect(),
                );
            }
            let (_, entry, _) = self.build(&self.state);
            let Some(entry) = entry else { return vec![] };
            let Ok(expr) = parse_expr(base) else {
                return vec![];
            };
            let cx = self.session_ctx(&self.state, &entry.env, Rc::new(|_, _| {}), None);
            let rt = infer(&cx, &expr).rt;
            fn members(t: Option<&RT>) -> Option<Vec<crate::semantics::Member>> {
                let t = t?;
                match &t.k {
                    RTk::Rec(_) => Some(rec_members(t)),
                    RTk::Union(arms) => {
                        let sets: Vec<Option<Vec<crate::semantics::Member>>> =
                            arms.iter().map(|a| members(Some(a))).collect();
                        if sets.iter().any(|s| s.is_none()) {
                            return None;
                        }
                        let sets: Vec<Vec<crate::semantics::Member>> =
                            sets.into_iter().map(|s| s.unwrap()).collect();
                        let first = sets.first().cloned().unwrap_or_default();
                        Some(
                            first
                                .into_iter()
                                .filter(|m| sets.iter().all(|s| s.iter().any(|x| x.name == m.name)))
                                .collect(),
                        )
                    }
                    RTk::Pred { base, .. } => members(Some(base)),
                    _ => None,
                }
            }
            let ms = members(rt.as_ref()).unwrap_or_default();
            return uniq(
                ms.iter()
                    .filter(|x| x.name.starts_with(prefix))
                    .map(|x| {
                        let kind = match x.kind {
                            MKind::Der => "derived",
                            MKind::Dflt => "defaulted",
                            MKind::Opt => "optional",
                            MKind::Req => "required",
                        };
                        format!(
                            "{}{}  {}{}",
                            x.name,
                            if x.hidden { "$" } else { "" },
                            kind,
                            x.ty.as_ref()
                                .map(|t| format!(": {}", type_text(Some(t))))
                                .unwrap_or_default()
                        )
                    })
                    .collect(),
            );
        }
        let word_re = Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)$").unwrap();
        let prefix = word_re
            .captures(text)
            .map(|m| m.get(1).unwrap().as_str().to_string())
            .unwrap_or_default();
        if prefix.starts_with('$') {
            return uniq(
                ["$this", "$parent", "$root", "$key", "$path", "$referrers"]
                    .iter()
                    .filter(|x| x.starts_with(&prefix))
                    .map(|x| x.to_string())
                    .collect(),
            );
        }
        let mut names: Vec<String> = vec!["std".into()];
        let (_, entry, _) = self.build(&self.state);
        if let Some(entry) = entry {
            let e = &entry.env;
            names.extend(e.type_asts.borrow().keys().cloned());
            names.extend(e.consts.borrow().keys().cloned());
            names.extend(e.funcs.borrow().keys().cloned());
            names.extend(e.inputs.borrow().keys().cloned());
            names.extend(e.outputs.borrow().iter().map(|(o, _, _)| o.clone()));
            names.extend(e.imports.borrow().keys().cloned());
            names.extend(e.namespaces.borrow().keys().cloned());
            names.extend(e.diags.borrow().keys().cloned());
        }
        names.extend(self.state.outputs.iter().map(|(n, _, _)| n.clone()));
        let kw = [
            "if", "then", "else", "for", "in", "match", "with", "matches", "true", "false", "null",
        ];
        names.extend(kw.iter().map(|k| k.to_string()));
        uniq(
            names
                .into_iter()
                .filter(|n| n.starts_with(&prefix))
                .collect(),
        )
    }
    fn complete_path(&self, partial: &str) -> Vec<String> {
        if !partial.contains('.') && !partial.contains('[') {
            let mut v: Vec<String> = self
                .all_root_names()
                .into_iter()
                .filter(|n| n.starts_with(partial))
                .collect();
            v.sort();
            return v;
        }
        // the base is the shortest prefix whose remainder is `.ident?` or `[…` (the reference's lazy match)
        let tail_dot = Regex::new(r"^\.([A-Za-z_][A-Za-z0-9_]*)?$").unwrap();
        let tail_bracket = Regex::new(r#"^\["?[^\]]*$"#).unwrap();
        let mut base = partial.to_string();
        for (i, c) in partial.char_indices() {
            if c == '.' || c == '[' {
                let rest = &partial[i..];
                if tail_dot.is_match(rest) || tail_bracket.is_match(rest) {
                    base = partial[..i].to_string();
                    break;
                }
            }
        }
        let r = self.run(Mode::Full);
        let (Some(eng), Some(entry)) = (&r.eng, &r.entry) else {
            return vec![];
        };
        let Ok(segs) = parse_path(&base, "") else {
            return vec![];
        };
        let Some(v) = self.value_at(eng, entry, &segs) else {
            return vec![];
        };
        let Ok(v) = eng.deref(v) else { return vec![] };
        let mut out: Vec<String> = vec![];
        match &v {
            Value::Rec(inst) => {
                for (n, s) in &inst.borrow().slots {
                    if s.hidden {
                        continue;
                    }
                    out.push(format!("{base}.{n}"));
                }
            }
            Value::Map(m) => {
                for (k, _) in &m.borrow().entries {
                    out.push(format!("{base}[{}]", json_str(k)));
                }
            }
            Value::Arr(a) => {
                for i in 0..a.borrow().items.len() {
                    out.push(format!("{base}[{i}]"));
                }
            }
            _ => {}
        }
        let mut out: Vec<String> = out.into_iter().filter(|x| x.starts_with(partial)).collect();
        out.sort();
        out
    }

    // ---- the scratch module (§4) ----
    /// The session's declarations as a module's text.
    pub fn scratch_text(&self) -> String {
        let mut parts: Vec<String> = self
            .state
            .decls
            .iter()
            .map(|(_, t)| t.trim().to_string())
            .collect();
        for (n, ty, expr) in &self.state.outputs {
            parts.push(format!(
                "output {n}: {} = {expr}",
                ty.clone().unwrap_or_else(|| self.inferred_type_text(expr))
            ));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("{}\n", parts.join("\n"))
        }
    }
    fn inferred_type_text(&self, expr: &str) -> String {
        self.type_of(expr)
            .map(|(t, _, _)| t)
            .unwrap_or_else(|_| "any".into())
    }
    /// the scratch module as a file: imports of the entry's exports it uses, then the declarations
    pub fn module_text(&self) -> String {
        let body = self.scratch_text();
        let (_, entry, _) = self.build(&self.state);
        let used = identifiers(&body);
        let mut names: Vec<String> = entry
            .map(|e| {
                e.exports
                    .borrow()
                    .keys()
                    .filter(|n| used.contains(*n))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let header = match (&self.entry_path, names.is_empty()) {
            (Some(p), false) => format!(
                "import {{ {} }} from \"./{}\"\n\n",
                names.join(", "),
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            _ => String::new(),
        };
        header + &body
    }
    /// The scratch module, formatted.
    pub fn fmt(&self) -> SResult<String> {
        let t = self.scratch_text();
        if t.is_empty() {
            return Ok(String::new());
        }
        format(&t).map_err(SessionError::new)
    }
    /// Write the scratch module to a file.
    pub fn write(&self, file: &str) -> SResult<()> {
        std::fs::write(file, self.module_text())
            .map_err(|_| SessionError::new(format!("cannot write {file}")))
    }

    // ---- documents out (§3) ----
    /// A root's document as JSON: the bound document, or the root evaluated.
    pub fn document_text(&self, name: &str) -> SResult<String> {
        if let Some(d) = self.state.document(name) {
            return Ok(doc_json(&d.doc));
        }
        if !self.has_root(name) {
            return Err(SessionError::new(format!("no root named {name}")));
        }
        let (_, docs, _) = self.evaluate(&[name.to_string()])?;
        docs.into_iter()
            .next()
            .and_then(|(_, j)| j)
            .ok_or_else(|| SessionError::new(format!("{name} is invalid")))
    }
    /// Save a root's document to a file.
    pub fn save(&self, name: &str, file: &str) -> SResult<()> {
        let text = self.document_text(name)?;
        std::fs::write(file, format!("{text}\n"))
            .map_err(|_| SessionError::new(format!("cannot write {file}")))
    }
    /// The lines of a document's diff against what it started from.
    pub fn diff(&self, name: &str) -> SResult<Vec<String>> {
        let Some(d) = self.state.document(name) else {
            return Err(SessionError::new(if self.has_root(name) {
                format!("{name} holds no document")
            } else {
                format!("no root named {name}")
            }));
        };
        let (before, after) = (doc_json(&d.base), doc_json(&d.doc));
        if before == after {
            return Ok(vec!["(no changes)".to_string()]);
        }
        let a: Vec<String> = pretty_json(&before)
            .split('\n')
            .map(|s| s.to_string())
            .collect();
        let b: Vec<String> = pretty_json(&after)
            .split('\n')
            .map(|s| s.to_string())
            .collect();
        Ok(line_diff(&a, &b))
    }

    // ---- introspection ----
    /// The session's declarations and bindings, as `:session` prints them.
    pub fn session_lines(&self) -> Vec<String> {
        let mut out = vec![];
        for (n, t) in &self.state.decls {
            out.push(format!(
                "declaration  {:<16} {}",
                n,
                t.trim().lines().next().unwrap_or("")
            ));
        }
        for (n, ty, expr) in &self.state.outputs {
            out.push(format!(
                "output       {:<16} {n}{} = {expr}",
                n,
                ty.as_ref().map(|t| format!(": {t}")).unwrap_or_default()
            ));
        }
        for (n, d) in &self.state.documents {
            out.push(format!(
                "document     {:<16} {}{}{}",
                n,
                d.origin.word(),
                d.file.as_ref().map(|f| format!(" {f}")).unwrap_or_default(),
                if d.edited { " (edited)" } else { "" }
            ));
        }
        out
    }
    /// The log with its cursor, as `:history` prints it.
    pub fn history_lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{} 0  (start)",
            if self.cursor == 0 { "*" } else { " " }
        )];
        for (i, op) in self.log.iter().enumerate() {
            out.push(format!(
                "{} {}  {}",
                if self.cursor == i + 1 { "*" } else { " " },
                i + 1,
                op_text(op)
            ));
        }
        out
    }
    /// The log as a session script, replayable with `--script`.
    pub fn script_lines(&self) -> Vec<String> {
        self.log[..self.cursor].iter().map(op_text).collect()
    }
}

// ---------------- helpers ----------------
/// A diagnostic as the REPL prints it, with the file when it is another module's.
pub fn fmt_diag(d: &Diag, in_file: Option<&str>) -> String {
    format!(
        "{}{}{}{}: {}{}",
        d.severity,
        d.code
            .as_ref()
            .map(|c| format!(" [{c}]"))
            .unwrap_or_default(),
        d.id.as_ref().map(|i| format!(" {i}")).unwrap_or_default(),
        if d.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", d.path)
        },
        d.message,
        in_file.map(|f| format!(" (in {f})")).unwrap_or_default()
    )
}

/// An operation as the session's input spells it.
pub fn op_text(op: &Op) -> String {
    match op {
        Op::Bind { name, src } => match src {
            BindSource::File { file, .. } => format!(":bind {name}={file}"),
            BindSource::Inline { text } => format!(
                ":bind {name} {}",
                read_json(text).map(|v| doc_json(&v)).unwrap_or_default()
            ),
            BindSource::Expr { text } => format!(":bind {name} = {}", text.trim()),
        },
        Op::Unbind { name } => format!(":unbind {name}"),
        Op::Edit { kind, path, expr } => format!(
            ":{} {path}{}",
            kind.word(),
            expr.as_ref()
                .map(|e| format!(" = {}", e.trim()))
                .unwrap_or_default()
        ),
        Op::Declare { text, .. } => text.trim().to_string(),
        Op::Output { name, ty, expr } => format!(
            "{name}{} = {}",
            ty.as_ref().map(|t| format!(": {t}")).unwrap_or_default(),
            expr.trim()
        ),
        Op::Drop { name } => format!(":drop {name}"),
        Op::Reload { .. } => ":reload".into(),
        Op::Reset => ":reset".into(),
    }
}

// a functional edit of a document at a path (read_json's shape)
fn edit_value(
    node: &Value,
    segs: &[Seg],
    i: usize,
    kind: EditKind,
    value: Option<&Value>,
    path: &str,
) -> SResult<Value> {
    if i < segs.len() - 1 {
        let s = &segs[i];
        let Some(child) = doc_step(node, s) else {
            return Err(SessionError::new(format!(
                "nothing at {}",
                path_str(&segs[..=i], None)
            )));
        };
        let new_child = edit_value(&child, segs, i + 1, kind, value, path)?;
        return Ok(replace_child(node, s, new_child));
    }
    let last = &segs[segs.len() - 1];
    let k = seg_text(last);
    match (node, last) {
        (Value::JObj(es), _) => {
            let idx = es.iter().position(|(kk, _)| *kk == k);
            let mut es: Vec<(String, Value)> = (**es).clone();
            match kind {
                EditKind::Create => {
                    if idx.is_some() {
                        return Err(SessionError::new(format!("{path} already holds a value")));
                    }
                    es.push((k, value.cloned().unwrap_or(Value::Null)));
                }
                EditKind::Update => match idx {
                    Some(i) => es[i].1 = value.cloned().unwrap_or(Value::Null),
                    None => return Err(SessionError::new(format!("nothing at {path}"))),
                },
                EditKind::Remove => match idx {
                    Some(i) => {
                        es.remove(i);
                    }
                    None => return Err(SessionError::new(format!("nothing at {path}"))),
                },
            }
            Ok(Value::JObj(Rc::new(es)))
        }
        (Value::JArr(items), Seg::Idx(k)) => {
            let mut items: Vec<Value> = (**items).clone();
            match kind {
                EditKind::Create => {
                    if *k < items.len() {
                        return Err(SessionError::new(format!("{path} already holds a value")));
                    }
                    if *k > items.len() {
                        return Err(SessionError::new(format!(
                            "{path} is past the end of the array"
                        )));
                    }
                    items.push(value.cloned().unwrap_or(Value::Null));
                }
                EditKind::Update => {
                    if *k >= items.len() {
                        return Err(SessionError::new(format!("nothing at {path}")));
                    }
                    items[*k] = value.cloned().unwrap_or(Value::Null);
                }
                EditKind::Remove => {
                    if *k >= items.len() {
                        return Err(SessionError::new(format!("nothing at {path}")));
                    }
                    items.remove(*k);
                }
            }
            Ok(Value::JArr(Rc::new(items)))
        }
        _ => Err(SessionError::new(format!(
            "{} is not a record, map, or array",
            path_str(&segs[..segs.len() - 1], None)
        ))),
    }
}
fn replace_child(node: &Value, s: &Seg, child: Value) -> Value {
    match (node, s) {
        (Value::JObj(es), _) => {
            let k = seg_text(s);
            let es: Vec<(String, Value)> = es
                .iter()
                .map(|(kk, v)| {
                    if *kk == k {
                        (kk.clone(), child.clone())
                    } else {
                        (kk.clone(), v.clone())
                    }
                })
                .collect();
            Value::JObj(Rc::new(es))
        }
        (Value::JArr(items), Seg::Idx(i)) => {
            let items: Vec<Value> = items
                .iter()
                .enumerate()
                .map(|(j, v)| if j == *i { child.clone() } else { v.clone() })
                .collect();
            Value::JArr(Rc::new(items))
        }
        _ => node.clone(),
    }
}

// a detached output (§3): its declaration becomes `input name: T` in the
// session's copy of the module — the name stays declared, the checker
// sees a root of the same type, and the session binds the projected
// document to it; line numbers are kept
fn detach_outputs(text: &str, names: &[String]) -> String {
    if names.is_empty() {
        return text.to_string();
    }
    let decls = parse_source(text).decls;
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    for d in &decls {
        let (DeclBody::Output { name, .. }, Some(loc)) = (&d.body, d.loc) else {
            continue;
        };
        if !names.contains(name) {
            continue;
        }
        let src = lines[loc.sl..=loc.el.min(lines.len() - 1)].join("\n");
        let name_at = src.find(name.as_str()).unwrap_or(0);
        let colon = src[name_at..]
            .find(':')
            .map(|i| i + name_at)
            .unwrap_or(src.len());
        // the type text: from the colon to the `=` at bracket depth 0
        let bytes = src.as_bytes();
        let mut depth = 0i32;
        let mut eq: Option<usize> = None;
        let mut i = colon + 1;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if "{[(<".contains(c) {
                depth += 1;
            } else if "}])>".contains(c) {
                depth -= 1;
            } else if c == '='
                && depth == 0
                && bytes.get(i + 1) != Some(&b'=')
                && i > 0
                && !matches!(bytes[i - 1], b'!' | b'<' | b'>')
            {
                eq = Some(i);
                break;
            }
            i += 1;
        }
        let type_text = match eq {
            Some(e) => src[colon + 1..e].trim().to_string(),
            None => src[(colon + 1).min(src.len())..].trim().to_string(),
        };
        let squeezed = SQUEEZE_WS.replace_all(&type_text, " ").to_string();
        lines[loc.sl] = format!("input {name}: {squeezed}");
        for l in lines.iter_mut().take(loc.el + 1).skip(loc.sl + 1) {
            l.clear();
        }
    }
    lines.join("\n")
}

// the places an expression reads, as navigation chains (a static
// approximation of the engine's read set: names, members, indexes)
fn reads_of(e: &Rc<Expr>) -> Vec<Rc<Expr>> {
    fn is_chain(x: &Expr) -> bool {
        match x {
            Expr::Name(_) | Expr::Ctx(_) => true,
            Expr::Member { x, .. } | Expr::Index { x, .. } => is_chain(x),
            _ => false,
        }
    }
    fn go(x: &Rc<Expr>, out: &mut Vec<Rc<Expr>>) {
        match &**x {
            Expr::Member { .. } | Expr::Index { .. } if is_chain(x) => {
                out.push(x.clone());
                if let Expr::Index { i, .. } = &**x {
                    go(i, out);
                }
            }
            Expr::Name(_) => out.push(x.clone()),
            Expr::Lit(_)
            | Expr::UnitLit { .. }
            | Expr::Ctx(_)
            | Expr::Pattern(_)
            | Expr::Referrers { .. } => {}
            Expr::Template(parts) => {
                for p in parts {
                    if let TPart::Expr(e) = p {
                        go(e, out);
                    }
                }
            }
            Expr::Obj(es) => es.iter().for_each(|(_, v)| go(v, out)),
            Expr::Arr(items) => items.iter().for_each(|(_, v)| go(v, out)),
            Expr::Comp { head, clauses } => {
                go(head, out);
                for c in clauses {
                    go(&c.iter, out);
                    c.filters.iter().for_each(|f| go(f, out));
                }
            }
            Expr::MapComp { key, val, clauses } => {
                go(key, out);
                go(val, out);
                for c in clauses {
                    go(&c.iter, out);
                    c.filters.iter().for_each(|f| go(f, out));
                }
            }
            Expr::Bin { l, r, .. } => {
                go(l, out);
                go(r, out);
            }
            Expr::Un { x, .. } | Expr::Paren(x) => go(x, out),
            Expr::If { c, t, f } => {
                go(c, out);
                go(t, out);
                go(f, out);
            }
            Expr::Lambda { body, .. } => go(body, out),
            Expr::Call { fun, args } => {
                go(fun, out);
                args.iter().for_each(|a| go(a, out));
            }
            Expr::Member { x, .. } => go(x, out),
            Expr::Index { x, i } => {
                go(x, out);
                go(i, out);
            }
            Expr::With { base, patch } => {
                go(base, out);
                go(patch, out);
            }
            Expr::Match { subject, arms } => {
                go(subject, out);
                arms.iter().for_each(|a| go(&a.body, out));
            }
        }
    }
    let mut out = vec![];
    go(e, &mut out);
    out.into_iter()
        .filter(|x| !matches!(&**x, Expr::Name(n) if n == "true" || n == "false" || n == "null"))
        .collect()
}

/// an expression's text, for chains and simple forms (the trace view)
pub fn expr_text(e: &Expr) -> String {
    match e {
        Expr::Lit(v) => match v {
            Value::Str(s) => json_str(s),
            other => crate::infer::js_str(other),
        },
        Expr::UnitLit { num, unit } => format!("{}{unit}", crate::semantics::js_num_str(*num)),
        Expr::Name(n) | Expr::Ctx(n) => n.clone(),
        Expr::Member { x, name, safe } => {
            format!("{}{}{name}", expr_text(x), if *safe { "?." } else { "." })
        }
        Expr::Index { x, i } => format!("{}[{}]", expr_text(x), expr_text(i)),
        Expr::Paren(x) => format!("({})", expr_text(x)),
        Expr::Bin { op, l, r } => format!("{} {op} {}", expr_text(l), expr_text(r)),
        Expr::Un { op, x } => format!("{op}{}", expr_text(x)),
        Expr::Call { fun, args } => format!(
            "{}({})",
            expr_text(fun),
            args.iter()
                .map(|a| expr_text(a))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::If { c, t, f } => format!(
            "if {} then {} else {}",
            expr_text(c),
            expr_text(t),
            expr_text(f)
        ),
        Expr::Referrers { ty, member } => format!("$referrers({ty}, {})", json_str(member)),
        Expr::Template(parts) => format!(
            "`{}`",
            parts
                .iter()
                .map(|p| match p {
                    TPart::Text(s) => s.clone(),
                    TPart::Expr(e) => format!("${{{}}}", expr_text(e)),
                })
                .collect::<String>()
        ),
        Expr::Obj(es) => format!(
            "{{ {} }}",
            es.iter()
                .map(|(k, v)| format!("{k}: {}", expr_text(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Arr(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|(spread, v)| format!("{}{}", if *spread { "..." } else { "" }, expr_text(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Comp { clauses, .. } => format!(
            "[for {} … ]",
            clauses
                .iter()
                .map(|c| format!("{} in {}", c.v, expr_text(&c.iter)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Lambda { params, .. } => format!("({}) => …", params.join(", ")),
        Expr::With { base, .. } => format!("{} with …", expr_text(base)),
        Expr::Match { subject, .. } => format!("match {} {{ … }}", expr_text(subject)),
        _ => "…".into(),
    }
}

// a minimal line diff (longest common subsequence)
fn line_diff(a: &[String], b: &[String]) -> Vec<String> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = vec![];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("- {}", a[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+ {}", b[j]));
        j += 1;
    }
    out
}
