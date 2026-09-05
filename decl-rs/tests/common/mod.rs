//! What the corpus drivers share (tests/README.md): the repository root,
//! JSON over the runtime's raw values, a language-server client and the
//! session replay of tests/lsp, the API corpus driver, temporary copies.
#![allow(dead_code, unused_imports)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// the repository root: the crate's parent
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap()
}

/// a fresh temporary directory, unique to this process and call
pub fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "decl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// a directory copied, recursively
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (p, t) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_dir(&p, &t);
        } else {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}

/// the codes `decl check` reports for an entry, in order
pub fn check_codes(entry: &Path) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_decl"))
        .arg("check")
        .arg(entry)
        .current_dir(root())
        .output()
        .unwrap();
    let re = regex::Regex::new(r"\[(E\d{4})\]").unwrap();
    re.captures_iter(&String::from_utf8_lossy(&out.stderr))
        .map(|c| c[1].to_string())
        .collect()
}

// ---------------- the corpora shared with the other implementations ----------------
// the API corpus driver (examples/api_corpus.rs): its JSON helpers and its run
#[path = "../../examples/api_corpus.rs"]
#[allow(dead_code)]
pub mod api_corpus;
pub use api_corpus::{get, json_eq, json_of};
pub use decl_lang::semantics::{read_json, Value};
use num_bigint::BigInt;
use std::collections::HashMap;
use std::rc::Rc;

pub fn jstr(s: &str) -> Value {
    Value::Str(s.to_string())
}
pub fn jint(n: i64) -> Value {
    Value::Int(BigInt::from(n))
}
pub fn jobj(pairs: Vec<(&str, Value)>) -> Value {
    Value::JObj(Rc::new(
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    ))
}
pub fn jarr(items: Vec<Value>) -> Value {
    Value::JArr(Rc::new(items))
}
pub fn text(v: &Value) -> &str {
    match v {
        Value::Str(s) => s,
        _ => panic!("a string was expected: {}", json_of(v)),
    }
}
pub fn int_of(v: &Value) -> i64 {
    match v {
        Value::Int(i) => i.to_string().parse().unwrap(),
        _ => panic!("an integer was expected: {}", json_of(v)),
    }
}
pub fn parse(s: &str) -> Value {
    read_json(s)
        .ok()
        .unwrap_or_else(|| panic!("not JSON: {}", &s[..s.len().min(200)]))
}

// ---------------- a language-server client over stdio ----------------
/// one server over stdio; every message it sends is logged
pub struct Client {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    next_id: i64,
    log: Vec<Value>,
}
impl Client {
    pub fn spawn() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_decl-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Client {
            child,
            stdin,
            stdout,
            next_id: 0,
            log: vec![],
        }
    }
    pub fn send(&mut self, v: &Value) {
        let body = json_of(v);
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }
    pub fn recv(&mut self) -> Value {
        let mut header: Vec<u8> = vec![];
        while !header.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert!(self.stdout.read(&mut b).unwrap() == 1, "server closed");
            header.push(b[0]);
        }
        let h = String::from_utf8_lossy(&header).to_string();
        let len: usize = h
            .split("Content-Length: ")
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut body = vec![0u8; len];
        self.stdout.read_exact(&mut body).unwrap();
        let v = parse(&String::from_utf8(body).unwrap());
        self.log.push(v.clone());
        v
    }
    /// the answer (a result, or {error}) and the methods of what arrived before it
    pub fn request(&mut self, method: &str, params: Value) -> (Value, Vec<String>) {
        self.next_id += 1;
        let my = self.next_id;
        self.send(&jobj(vec![
            ("jsonrpc", jstr("2.0")),
            ("id", jint(my)),
            ("method", jstr(method)),
            ("params", params),
        ]));
        let mut between = vec![];
        loop {
            let m = self.recv();
            // a response carries no method; a server's own request (window/workDoneProgress/create) does
            if get(&m, "method").is_none()
                && matches!(get(&m, "id"), Some(Value::Int(i)) if *i == BigInt::from(my))
            {
                let answer = match get(&m, "result") {
                    Some(r) => r.clone(),
                    None => jobj(vec![(
                        "error",
                        get(&m, "error").cloned().unwrap_or(Value::Null),
                    )]),
                };
                return (answer, between);
            }
            between.push(
                get(&m, "method")
                    .map(text)
                    .unwrap_or("response")
                    .to_string(),
            );
        }
    }
    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&jobj(vec![
            ("jsonrpc", jstr("2.0")),
            ("method", jstr(method)),
            ("params", params),
        ]));
    }
    /// the next publishDiagnostics for the document, and every message seen until it
    pub fn diagnostics(&mut self, uri: &str) -> (Value, Vec<Value>) {
        let mut seen = vec![];
        loop {
            let m = self.recv();
            seen.push(m.clone());
            if get(&m, "method").map(text) == Some("textDocument/publishDiagnostics") {
                let params = get(&m, "params").unwrap();
                if get(params, "uri").map(text) == Some(uri) {
                    return (get(params, "diagnostics").unwrap().clone(), seen);
                }
            }
        }
    }
    pub fn pending_request(&self, method: &str) -> Value {
        self.log
            .iter()
            .rev()
            .find(|m| get(m, "method").map(text) == Some(method) && get(m, "id").is_some())
            .and_then(|m| get(m, "id").cloned())
            .unwrap_or(Value::Null)
    }
    pub fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

// ---------------- the session corpus (tests/lsp/README.md) ----------------
pub fn find(text: &str, needle: &str, nth: usize, offset: i64) -> (usize, i64) {
    let mut at: Option<usize> = None;
    for _ in 0..=nth {
        let from = at.map(|i| i + 1).unwrap_or(0);
        at = Some(
            from + text[from..]
                .find(needle)
                .unwrap_or_else(|| panic!("needle not found: {needle}")),
        );
    }
    let i = at.unwrap();
    let line = text[..i].matches('\n').count();
    let start = text[..i].rfind('\n').map(|k| k + 1).unwrap_or(0);
    (line, (i - start) as i64 + offset)
}
pub fn position(line: usize, character: i64) -> Value {
    jobj(vec![
        ("line", jint(line as i64)),
        ("character", jint(character)),
    ])
}

pub struct Session {
    ws: PathBuf,
    client: Client,
    texts: HashMap<String, String>,
    versions: HashMap<String, i64>,
    diags: HashMap<String, Value>,
    answers: HashMap<String, Value>,
    open_files: Vec<String>,
    transcript: Vec<(String, Value)>,
}
impl Session {
    pub fn new(case_dir: &Path) -> Session {
        Session {
            ws: case_dir.join("ws").canonicalize().unwrap(),
            client: Client::spawn(),
            texts: HashMap::new(),
            versions: HashMap::new(),
            diags: HashMap::new(),
            answers: HashMap::new(),
            open_files: vec![],
            transcript: vec![],
        }
    }
    pub fn uri(&self, file: &str) -> String {
        decl_lang::lsp::uri_of(&self.ws.join(file))
    }
    pub fn file_of(&self, uri: &str) -> Option<String> {
        self.texts.keys().find(|f| self.uri(f) == uri).cloned()
    }
    /// the placeholders: {"$uri": file}, {"$pos"|"$at"|"$span": needle, nth, offset},
    /// {"$diagnostics": file}, {"$answer": label, index}
    pub fn resolve(&self, v: &Value, doc: Option<&str>) -> Value {
        match v {
            Value::JArr(items) => jarr(items.iter().map(|x| self.resolve(x, doc)).collect()),
            Value::JObj(es) => {
                let key = es
                    .iter()
                    .find(|(k, _)| k.starts_with('$'))
                    .map(|(k, _)| k.as_str());
                match key {
                    Some("$uri") => jstr(&self.uri(text(get(v, "$uri").unwrap()))),
                    Some(k @ ("$pos" | "$at" | "$span")) => {
                        let doc = doc.expect("a position placeholder needs a textDocument");
                        let needle = text(get(v, k).unwrap());
                        let nth = get(v, "nth").map(int_of).unwrap_or(0) as usize;
                        let offset = get(v, "offset").map(int_of).unwrap_or(0);
                        let (line, col) = find(&self.texts[doc], needle, nth, offset);
                        if k == "$pos" {
                            return position(line, col);
                        }
                        let end = if k == "$span" {
                            col + needle.len() as i64
                        } else {
                            col
                        };
                        jobj(vec![
                            ("start", position(line, col)),
                            ("end", position(line, end)),
                        ])
                    }
                    Some("$diagnostics") => self
                        .diags
                        .get(text(get(v, "$diagnostics").unwrap()))
                        .cloned()
                        .unwrap_or_else(|| jarr(vec![])),
                    Some("$answer") => {
                        let label = text(get(v, "$answer").unwrap());
                        let index = get(v, "index").map(int_of).unwrap_or(0) as usize;
                        match &self.answers[label] {
                            Value::JArr(items) => items[index].clone(),
                            other => panic!("the answer {label} is not a list: {}", json_of(other)),
                        }
                    }
                    _ => jobj(
                        es.iter()
                            .map(|(k, x)| (k.as_str(), self.resolve(x, doc)))
                            .collect(),
                    ),
                }
            }
            other => other.clone(),
        }
    }
    pub fn params_of(&self, step: &Value) -> Value {
        let params = get(step, "params").cloned().unwrap_or_else(|| jobj(vec![]));
        // the document the request addresses: its textDocument, resolved first
        let doc = get(&params, "textDocument")
            .and_then(|td| get(td, "uri"))
            .and_then(|u| self.file_of(text(&self.resolve(u, None))));
        self.resolve(&params, doc.as_deref())
    }
    /// temp paths and URI encodings normalized; the server's version too
    pub fn norm(&self, v: &Value) -> Value {
        match v {
            Value::Str(s) => jstr(
                &s.replace(&self.ws.to_string_lossy().to_string(), "<ws>")
                    .replace("%2F", "/"),
            ),
            Value::JArr(items) => jarr(items.iter().map(|x| self.norm(x)).collect()),
            Value::JObj(es) => {
                let mut out: Vec<(String, Value)> = es
                    .iter()
                    .map(|(k, x)| (text(&self.norm(&jstr(k))).to_string(), self.norm(x)))
                    .collect();
                if let Some(info) = out.iter_mut().find(|(k, _)| k == "serverInfo") {
                    if let Value::JObj(fields) = &info.1 {
                        if fields.iter().any(|(k, _)| k == "version") {
                            info.1 = Value::JObj(Rc::new(
                                fields
                                    .iter()
                                    .map(|(k, x)| {
                                        (
                                            k.clone(),
                                            if k == "version" {
                                                jstr("<version>")
                                            } else {
                                                x.clone()
                                            },
                                        )
                                    })
                                    .collect(),
                            ));
                        }
                    }
                }
                Value::JObj(Rc::new(out))
            }
            other => other.clone(),
        }
    }
    pub fn record(&mut self, label: Option<&str>, value: Value) {
        if let Some(label) = label {
            self.transcript.push((label.to_string(), self.norm(&value)));
            self.answers.insert(label.to_string(), value);
        }
    }
    pub fn observed(seen: &[Value]) -> Value {
        let rows = seen
            .iter()
            .map(|m| {
                let method = get(m, "method").map(text).unwrap_or("response");
                let id_kind = match get(m, "id") {
                    Some(Value::Int(_)) => jstr("int"),
                    Some(Value::Str(_)) => jstr("str"),
                    Some(_) => jstr("float"),
                    None => Value::Null,
                };
                let kind = get(m, "params")
                    .and_then(|p| get(p, "value"))
                    .and_then(|v| get(v, "kind"))
                    .cloned()
                    .unwrap_or(Value::Null);
                jarr(vec![jstr(method), id_kind, kind])
            })
            .collect();
        let create_is_int = seen.iter().any(|m| {
            get(m, "method").map(text) == Some("window/workDoneProgress/create")
                && matches!(get(m, "id"), Some(Value::Int(_)))
        });
        jobj(vec![
            ("seen", jarr(rows)),
            ("create id is an integer", Value::Bool(create_is_int)),
        ])
    }
    pub fn run(mut self, case_dir: &Path) -> Vec<(String, Value)> {
        let session = parse(&std::fs::read_to_string(case_dir.join("session.json")).unwrap());
        let Some(Value::JArr(steps)) = get(&session, "steps").cloned() else {
            panic!("session.json has steps")
        };
        for step in steps.iter() {
            let label = get(step, "label").map(text);
            if let Some(file) = get(step, "open").or_else(|| get(step, "change")).map(text) {
                let file = file.to_string();
                let body = text(get(step, "text").unwrap()).to_string();
                self.texts.insert(file.clone(), body.clone());
                let uri = self.uri(&file);
                if get(step, "open").is_some() {
                    self.versions.insert(file.clone(), 1);
                    self.open_files.push(file.clone());
                    self.client.notify(
                        "textDocument/didOpen",
                        jobj(vec![(
                            "textDocument",
                            jobj(vec![
                                ("uri", jstr(&uri)),
                                ("languageId", jstr("decl")),
                                ("version", jint(1)),
                                ("text", jstr(&body)),
                            ]),
                        )]),
                    );
                } else {
                    let v = self.versions.get(&file).copied().unwrap_or(0) + 1;
                    self.versions.insert(file.clone(), v);
                    self.client.notify(
                        "textDocument/didChange",
                        jobj(vec![
                            (
                                "textDocument",
                                jobj(vec![("uri", jstr(&uri)), ("version", jint(v))]),
                            ),
                            (
                                "contentChanges",
                                jarr(vec![jobj(vec![("text", jstr(&body))])]),
                            ),
                        ]),
                    );
                }
                let (diags, seen) = self.client.diagnostics(&uri);
                self.diags.insert(file, diags.clone());
                let observe = matches!(get(step, "observe"), Some(Value::Bool(true)));
                self.record(
                    label,
                    if observe {
                        Session::observed(&seen)
                    } else {
                        diags
                    },
                );
            } else if let Some(method) = get(step, "request").map(text) {
                let params = self.params_of(step);
                let (answer, between) = self.client.request(method, params);
                if matches!(get(step, "between"), Some(Value::Bool(true))) {
                    let answered = get(&answer, "error").is_none();
                    self.record(
                        label,
                        jobj(vec![
                            ("answered", Value::Bool(answered)),
                            ("between", jarr(between.iter().map(|m| jstr(m)).collect())),
                        ]),
                    );
                } else {
                    self.record(label, answer);
                }
            } else if let Some(method) = get(step, "notify").map(text) {
                let params = self.params_of(step);
                self.client.notify(method, params);
            } else if let Some(config) = get(step, "config") {
                self.client.notify(
                    "workspace/didChangeConfiguration",
                    jobj(vec![("settings", config.clone())]),
                );
                for file in self.open_files.clone() {
                    let uri = self.uri(&file);
                    let (diags, _) = self.client.diagnostics(&uri);
                    self.diags.insert(file, diags);
                }
            } else if let Some(method) = get(step, "respond").map(text) {
                let id = self.client.pending_request(method);
                let result = get(step, "result").cloned().unwrap_or(Value::Null);
                self.client.send(&jobj(vec![
                    ("jsonrpc", jstr("2.0")),
                    ("id", id),
                    ("result", result),
                ]));
            } else {
                panic!("unknown step: {}", json_of(step));
            }
        }
        self.client.close();
        self.transcript
    }
}

/// where two texts first differ, with some context on each side
pub fn first_diff(expected: &str, got: &str) -> String {
    let i = expected
        .bytes()
        .zip(got.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(got.len()));
    let from = i.saturating_sub(60);
    let cut = |s: &str| s[from.min(s.len())..(i + 120).min(s.len())].to_string();
    format!(
        "at byte {i}\n      expected …{}…\n      got      …{}…",
        cut(expected),
        cut(got)
    )
}
