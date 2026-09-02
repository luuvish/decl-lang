//! Minimal LSP server over stdio — a port of the reference
//! implementation's lsp.ts (ROADMAP Phase 4): diagnostics first, then
//! hover, then definition — module-aware through the same loader the CLI
//! uses, with open buffers overriding the disk. Messages are handled
//! strictly in order; the server also exits when its stdin closes.
use crate::ast::DeclBody;
use crate::checker::check_module;
use crate::fmt::u16len;
use crate::module::load_modules;
use crate::package::open_package_universe;
use crate::parse::{parse_source, LANGUAGE};
use crate::semantics::{json_str, read_json, Diag, Value};
use regex::Regex;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser, Point, Tree};

// ---------------- JSON helpers over the runtime's Value ----------------
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
fn reply(id: &Value, result: &str) {
    send(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", json_of(id), result));
}
fn notify(method: &str, params: &str) {
    send(&format!("{{\"jsonrpc\":\"2.0\",\"method\":{},\"params\":{}}}", json_str(method), params));
}

// ---------------- documents & analysis ----------------
#[derive(Default)]
struct State {
    /// uri -> text, in open order (like the reference's Map)
    docs: Vec<(String, String)>,
}
impl State {
    fn set(&mut self, uri: &str, text: String) {
        if let Some(d) = self.docs.iter_mut().find(|(u, _)| u == uri) {
            d.1 = text;
        } else {
            self.docs.push((uri.to_string(), text));
        }
    }
    fn text(&self, uri: &str) -> Option<&String> {
        self.docs.iter().find(|(u, _)| u == uri).map(|(_, t)| t)
    }
}

pub fn path_of(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let raw = raw.split(['?', '#']).next().unwrap_or(raw);
    // percent-decode
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

fn parse_tree(src: &str) -> Tree {
    let mut p = Parser::new();
    let lang: Language = LANGUAGE.into();
    p.set_language(&lang).expect("grammar");
    p.parse(src, None).expect("parse")
}

/// find an identifier's position to anchor a position-less diagnostic
fn anchor_for(src: &str, message: &str) -> (usize, usize, usize) {
    let names = Regex::new(r"[A-Za-z_][A-Za-z0-9_.]*").unwrap();
    let lines: Vec<&str> = src.split('\n').collect();
    for m in names.find_iter(message) {
        let n = m.as_str();
        if ["error", "in", "the", "a", "is", "not", "std"].contains(&n) {
            continue;
        }
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(n))).unwrap();
        for (i, line) in lines.iter().enumerate() {
            if let Some(mm) = re.find(line) {
                let a = u16len(&line[..mm.start()]);
                return (i, a, a + u16len(n));
            }
        }
    }
    (0, 0, 1)
}

fn analyze(st: &State, uri: &str) {
    let src = st.text(uri).cloned().unwrap_or_default();
    let path = path_of(uri);
    let mut diags: Vec<String> = vec![];
    let mut push = |line: usize, a: usize, b: usize, message: &str, code: Option<&str>| {
        let code_part = code.map(|c| format!(",\"code\":{}", json_str(c))).unwrap_or_default();
        diags.push(format!(
            "{{\"range\":{{\"start\":{{\"line\":{line},\"character\":{a}}},\"end\":{{\"line\":{line},\"character\":{b}}}}},\"severity\":1,\"source\":\"decl\"{code_part},\"message\":{}}}",
            json_str(message)
        ));
    };
    let parsed = parse_source(&src);
    if !parsed.errors.is_empty() {
        for (row, col) in &parsed.errors {
            push(*row, *col, col + 1, "syntax error", Some("E2001"));
        }
    } else {
        let pkg = open_package_universe(&path);
        let overrides: HashMap<PathBuf, String> = st.docs.iter().map(|(u, t)| (path_of(u), t.clone())).collect();
        let r = load_modules(&path, pkg.as_ref().map(|u| &u.resolver), Some(&overrides));
        let mine = r.modules.iter().find(|m| m.path == path);
        let mut all: Vec<Diag> = pkg.as_ref().map(|u| u.diags.clone()).unwrap_or_default();
        all.extend(r.diags.iter().cloned());
        if let Some(m) = mine {
            all.extend(check_module(&m.decls, Some(m.env.clone())));
        }
        for d in all.iter().filter(|d| d.severity == "error") {
            let (line, a, b) = anchor_for(&src, &d.message);
            push(line, a, b, &d.message, d.code.as_deref());
        }
    }
    notify("textDocument/publishDiagnostics", &format!("{{\"uri\":{},\"diagnostics\":[{}]}}", json_str(uri), diags.join(",")));
}

// ---------------- declarations index (hover / definition) ----------------
struct DeclSite {
    path: PathBuf,
    row: usize,
    a: usize,
    b: usize,
    line: String,
    kind: String,
}

fn u16_col(line: &str, byte_col: usize) -> usize {
    u16len(line.get(..byte_col).unwrap_or(line))
}
/// an LSP character offset (UTF-16 units) as a byte offset into the line
fn byte_col(line: &str, u16_col: usize) -> usize {
    let mut units = 0;
    for (i, ch) in line.char_indices() {
        if units >= u16_col {
            return i;
        }
        units += ch.len_utf16();
    }
    line.len()
}

fn decl_index(path: &Path, src: &str) -> Vec<(String, DeclSite)> {
    let mut out = vec![];
    let tree = parse_tree(src);
    let lines: Vec<&str> = src.split('\n').collect();
    let root = tree.root_node();
    let mut cur = root.walk();
    for c in root.named_children(&mut cur) {
        if !c.kind().ends_with("_declaration") {
            continue;
        }
        let Some(name_node) = c.child_by_field_name("name") else { continue };
        let row = name_node.start_position().row;
        let line = lines.get(row).copied().unwrap_or("");
        let name = name_node.utf8_text(src.as_bytes()).unwrap_or("").to_string();
        let site = DeclSite {
            path: path.to_path_buf(),
            row,
            a: u16_col(line, name_node.start_position().column),
            b: u16_col(line, name_node.end_position().column),
            line: lines.get(c.start_position().row).copied().unwrap_or("").trim().to_string(),
            kind: c.kind().replace("_declaration", ""),
        };
        if let Some(e) = out.iter_mut().find(|(n, _): &&mut (String, DeclSite)| *n == name) {
            e.1 = site;
        } else {
            out.push((name, site));
        }
    }
    out
}
fn lookup<'a>(idx: &'a [(String, DeclSite)], name: &str) -> Option<&'a DeclSite> {
    idx.iter().find(|(n, _)| n == name).map(|(_, s)| s)
}

fn read_src(st: &State, path: &Path) -> Option<String> {
    if let Some((_, t)) = st.docs.iter().find(|(u, _)| path_of(u) == path) {
        return Some(t.clone());
    }
    std::fs::read_to_string(path).ok()
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// resolve the name under the cursor to its declaration site, following
/// one import hop (named, renamed, or namespace member)
fn find_decl(st: &State, uri: &str, line: usize, character: usize) -> Option<DeclSite> {
    let src = st.text(uri)?.clone();
    let path = path_of(uri);
    let tree = parse_tree(&src);
    let lines: Vec<&str> = src.split('\n').collect();
    let col = lines.get(line).map(|l| byte_col(l, character)).unwrap_or(0);
    let pt = Point { row: line, column: col };
    let node: Node = tree.root_node().descendant_for_point_range(pt, pt)?;
    if node.kind() != "identifier" {
        return None;
    }
    let word = node.utf8_text(src.as_bytes()).unwrap_or("").to_string();

    let local = decl_index(&path, &src);
    if let Some(s) = lookup(&local, &word) {
        return Some(DeclSite { path: s.path.clone(), row: s.row, a: s.a, b: s.b, line: s.line.clone(), kind: s.kind.clone() });
    }

    // namespace member: ns.word — look at the sibling chain
    let prev_sib = node.parent().filter(|p| p.kind() == "qualified_name").and_then(|p| p.named_child(0));
    let ns_name: Option<String> = prev_sib.filter(|p| p.id() != node.id()).map(|p| p.utf8_text(src.as_bytes()).unwrap_or("").to_string());

    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for d in &parse_source(&src).decls {
        let DeclBody::Import { from, names, ns } = &d.body else { continue };
        let target: Option<PathBuf> = if from.starts_with('.') {
            Some(normalize(&dir.join(from)))
        } else {
            open_package_universe(&path).and_then(|u| (u.resolver)(from, &dir).ok())
        };
        let Some(target) = target else { continue };
        let Some(tsrc) = read_src(st, &target) else { continue };
        if ns.is_some() && ns.as_deref() == ns_name.as_deref() {
            let tidx = decl_index(&target, &tsrc);
            if let Some(s) = lookup(&tidx, &word) {
                return Some(DeclSite { path: s.path.clone(), row: s.row, a: s.a, b: s.b, line: s.line.clone(), kind: s.kind.clone() });
            }
        }
        for it in names.iter().flatten() {
            if it.alias.clone().unwrap_or_else(|| it.name.clone()) != word {
                continue;
            }
            let tidx = decl_index(&target, &tsrc);
            if let Some(s) = lookup(&tidx, &it.name) {
                return Some(DeclSite { path: s.path.clone(), row: s.row, a: s.a, b: s.b, line: s.line.clone(), kind: s.kind.clone() });
            }
        }
    }
    None
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
        (as_usize(pos.and_then(|p| get(p, "line"))).unwrap_or(0), as_usize(pos.and_then(|p| get(p, "character"))).unwrap_or(0))
    };
    match method {
        "initialize" => {
            if let Some(id) = id {
                reply(id, "{\"capabilities\":{\"textDocumentSync\":1,\"hoverProvider\":true,\"definitionProvider\":true},\"serverInfo\":{\"name\":\"decl-lsp\",\"version\":\"0.2.0\"}}");
            }
        }
        "initialized" => {}
        "textDocument/didOpen" => {
            let uri = td_uri();
            let text = as_str(params.and_then(|p| get(p, "textDocument")).and_then(|t| get(t, "text"))).unwrap_or("").to_string();
            st.set(&uri, text);
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
            analyze(st, &uri);
        }
        "textDocument/didClose" => {
            let uri = td_uri();
            st.docs.retain(|(u, _)| *u != uri);
        }
        "textDocument/hover" => {
            let (line, ch) = position();
            let site = find_decl(st, &td_uri(), line, ch);
            if let Some(id) = id {
                match site {
                    Some(s) => reply(id, &format!("{{\"contents\":{{\"kind\":\"markdown\",\"value\":{}}}}}", json_str(&format!("**{}** — `{}`", s.kind, s.line)))),
                    None => reply(id, "null"),
                }
            }
        }
        "textDocument/definition" => {
            let (line, ch) = position();
            let site = find_decl(st, &td_uri(), line, ch);
            if let Some(id) = id {
                match site {
                    Some(s) => reply(id, &format!(
                        "{{\"uri\":{},\"range\":{{\"start\":{{\"line\":{},\"character\":{}}},\"end\":{{\"line\":{},\"character\":{}}}}}}}",
                        json_str(&uri_of(&s.path)), s.row, s.a, s.row, s.b
                    )),
                    None => reply(id, "null"),
                }
            }
        }
        "shutdown" => {
            if let Some(id) = id {
                reply(id, "null");
            }
        }
        "exit" => return Some(0),
        _ => {
            if let Some(id) = id {
                reply(id, "null");
            }
        }
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
