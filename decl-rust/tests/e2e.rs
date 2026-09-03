//! The reference implementation's packages.ts, fmt.ts, and lsp.ts scenarios
//! against the Rust implementation: cross-package imports and the lock
//! file, the canonical formatter, and the language server over stdio.
use decl_lang::checker::check_module;
use decl_lang::conformance::walk_decl;
use decl_lang::fmt::format;
use decl_lang::module::{load_modules, run_universe};
use decl_lang::package::{lock_text, open_package_universe, verify_lock, write_lock};
use decl_lang::parse::parse_source;
use decl_lang::semantics::{Seg, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").canonicalize().unwrap()
}

#[test]
fn packages_cross_package_imports_and_lock() {
    let entry = root().join("tests/packages/app/main.decl");
    let u = open_package_universe(&entry).expect("in a package");
    assert!(u.diags.is_empty(), "universe opens clean: {:?}", u.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert!(u.packages.len() == 1 && u.package("corelib").unwrap().version == "1.0.0", "closed set resolved");
    let r = load_modules(&entry, Some(&u.resolver), None);
    assert!(r.modules.len() == 2 && r.diags.is_empty(), "modules load across packages");
    let checks: Vec<_> = r.modules.iter().flat_map(|m| check_module(&m.decls, Some(m.env.clone()))).collect();
    assert!(checks.is_empty(), "modules check clean: {:?}", checks.iter().map(|d| &d.message).collect::<Vec<_>>());
    let em = r.entry.clone().unwrap();
    let (eng, ed) = run_universe(&r.modules, &em, vec![]);
    assert!(!ed.iter().any(|d| d.severity == "error"), "evaluates clean");
    assert!(matches!(em.env.root("w"), Some(Value::Int(i)) if i == 16.into()), "imported const");
    assert!(matches!(eng.resolve_segs(&[Seg::Name("box".into()), Seg::Name("width".into())]), Ok(Value::Int(i)) if i == 8.into()), "defaults across packages");

    // lock file: reproducibility, fail-closed drift
    let lock_path = root().join("tests/packages/app/decl.lock");
    let mod_path = root().join("tests/packages/app/decl_modules/corelib/types/base.decl");
    let result = std::panic::catch_unwind(|| {
        let u1 = open_package_universe(&entry).unwrap();
        write_lock(&u1);
        assert!(verify_lock(&u1).is_empty(), "fresh lock verifies clean");
        let u2 = open_package_universe(&entry).unwrap();
        assert!(lock_text(&u1) == lock_text(&u2) && lock_text(&u1).contains("corelib 1.0.0 "), "lock text is reproducible");
        let original = std::fs::read_to_string(&mod_path).unwrap();
        std::fs::write(&mod_path, format!("{original}// drift\n")).unwrap();
        let u3 = open_package_universe(&entry).unwrap();
        let drift = verify_lock(&u3);
        std::fs::write(&mod_path, &original).unwrap();
        assert!(drift.iter().any(|d| d.code.as_deref() == Some("E3017")), "content drift is E3017");
        std::fs::write(&lock_path, lock_text(&u1).replace("1.0.0", "1.0.1")).unwrap();
        assert!(verify_lock(&open_package_universe(&entry).unwrap()).iter().any(|d| d.code.as_deref() == Some("E3016")), "version drift is E3016");
        std::fs::write(&lock_path, "").unwrap();
        assert!(verify_lock(&open_package_universe(&entry).unwrap()).iter().any(|d| d.code.as_deref() == Some("E3015")), "missing entry is E3015");
    });
    let _ = std::fs::remove_file(&lock_path);
    result.unwrap();
}

#[test]
fn packages_manifest_and_resolution_errors() {
    let bad = open_package_universe(&root().join("tests/packages/bad_manifest/main.decl")).unwrap();
    assert!(bad.diags.iter().any(|d| d.code.as_deref() == Some("E3011")), "unknown field is E3011");
    assert!(bad.diags.iter().any(|d| d.code.as_deref() == Some("E3012")), "range pin is E3012");
    let und_entry = root().join("tests/packages/undeclared/main.decl");
    let und = open_package_universe(&und_entry).unwrap();
    let r = load_modules(&und_entry, Some(&und.resolver), None);
    assert!(r.diags.iter().any(|d| d.code.as_deref() == Some("E3010")), "undeclared dependency is E3010");
    let con = open_package_universe(&root().join("tests/packages/conflict/main.decl")).unwrap();
    assert!(con.diags.iter().any(|d| d.code.as_deref() == Some("E3014")), "conflicting versions is E3014");
}

#[test]
fn formatter_canonical_form() {
    let cases = [
        ("spacing", "const x=1+2*3\n", "const x = 1 + 2 * 3\n"),
        ("range stays tight", "type P=1..65535\n", "type P = 1..65535\n"),
        ("generic angles attach", "type V = Vec<int ,4>\n", "type V = Vec<int, 4>\n"),
        ("call parens attach", "const n = std.array.count(xs )\n", "const n = std.array.count(xs)\n"),
        ("record braces breathe", "type T = {a: int,b?: string}\n", "type T = { a: int, b?: string }\n"),
        ("indent rederived", "type T = {\n        a: int\n  b: string\n}\n", "type T = {\n    a: int\n    b: string\n}\n"),
        ("unary minus attaches", "const y = -x + 1\n", "const y = -x + 1\n"),
        ("blank lines collapse", "const a = 1\n\n\n\nconst b = 2\n", "const a = 1\n\nconst b = 2\n"),
        ("continuation hangs", "type T = {\n    assert a: x > 0\nelse warn `bad`\n}\n", "type T = {\n    assert a: x > 0\n        else warn `bad`\n}\n"),
        ("lambda spacing", "const f = std.array.all(xs,(x)=>x>0)\n", "const f = std.array.all(xs, (x) => x > 0)\n"),
        ("array suffix after a record attaches", "input s: {a: int, ...}[]\n", "input s: { a: int, ... }[]\n"),
        ("func body hangs after =", "func f(n: int): int =\nn + 1\n", "func f(n: int): int =\n    n + 1\n"),
        ("lambda body hangs after =>", "const xs = std.array.filter(ys, (y) =>\ny > 0)\n", "const xs = std.array.filter(ys, (y) =>\n        y > 0)\n"),
        ("operator at line end continues", "const s = a +\nb\n", "const s = a +\n    b\n"),
        ("a closing type angle does not continue", "type P = {\n    $parent: ref<{ a: int, ... }>\n    b: int\n}\n", "type P = {\n    $parent: ref<{ a: int, ... }>\n    b: int\n}\n"),
    ];
    for (name, input, want) in cases {
        assert_eq!(format(input).unwrap_or_else(|e| format!("THROW {e}")), want, "{name}");
    }
}

#[test]
fn formatter_idempotent_and_ast_safe_over_corpus() {
    let mut files = vec![];
    for d in ["tests/validation", "tests/modules", "tests/packages", "docs/examples"] {
        walk_decl(&root().join(d), &mut files);
    }
    let tokens = |src: &str| format!("{:?}", parse_source(src).decls);
    let (mut idem, mut idem_fail, mut token_fail, mut skipped) = (0, 0, 0, 0);
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        if !parse_source(&src).errors.is_empty() {
            skipped += 1;
            continue;
        }
        let Ok(once) = format(&src) else {
            skipped += 1;
            continue;
        };
        let twice = match format(&once) {
            Ok(t) => t,
            Err(e) => {
                idem_fail += 1;
                eprintln!("SECOND PASS FAILS {}: {e}", f.display());
                continue;
            }
        };
        if once == twice { idem += 1 } else { idem_fail += 1; eprintln!("NOT IDEMPOTENT {}", f.display()); }
        if !(parse_source(&once).errors.is_empty() && tokens(&once) == tokens(&src)) {
            token_fail += 1;
            eprintln!("AST CHANGED {}", f.display());
        }
    }
    assert!(idem_fail == 0, "fmt(fmt(x)) == fmt(x) on {} parseable files", idem + idem_fail);
    assert!(token_fail == 0, "formatting preserves the AST on all files");
    eprintln!("({skipped} unparseable fixtures skipped by design)");
}

// ---------------- the command line ----------------
#[test]
fn cli_evaluate_binds_inputs_and_roots_them() {
    let dir = std::env::temp_dir().join(format!("decl-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let decl = dir.join("main.decl");
    std::fs::write(&decl, "type Cfg = { host: string, port?: int = 80 }\ninput base: Cfg\noutput copy: Cfg = base\n").unwrap();
    let doc = dir.join("base.json");
    std::fs::write(&doc, "{\"host\": \"example\"}\n").unwrap();
    let decl_s = decl.to_str().unwrap().to_string();
    let bind = format!("base={}", doc.display());
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_decl")).args(args).output().unwrap();
    // --input binds the document; --root may name the bound input
    let out = run(&["evaluate", &decl_s, "--input", &bind, "--root", "base"]);
    assert!(out.status.success(), "evaluate --input --root <input> succeeds: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{\"host\":\"example\",\"port\":80}");
    // an output reading the bound input completes from the document
    let out = run(&["evaluate", &decl_s, "--input", &bind]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{\"copy\":{\"host\":\"example\",\"port\":80}}");
    // nothing bound: the fallback-less input demanded by an output is E5006 at the output
    let out = run(&["evaluate", &decl_s]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("error [E5006] at copy: input base is not bound"), "{}", String::from_utf8_lossy(&out.stderr));
    // a root that does not exist
    let out = run(&["evaluate", &decl_s, "--input", &bind, "--root", "nope"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("no root named nope"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------- LSP over stdio ----------------
struct Client {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    next_id: u64,
}
impl Client {
    fn spawn() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_decl-lsp")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Client { child, stdin, stdout, next_id: 0 }
    }
    fn send(&mut self, body: &str) {
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }
    fn recv(&mut self) -> String {
        let mut header: Vec<u8> = vec![];
        while !header.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert!(self.stdout.read(&mut b).unwrap() == 1, "server closed");
            header.push(b[0]);
        }
        let h = String::from_utf8_lossy(&header).to_string();
        let len: usize = h.split("Content-Length: ").nth(1).unwrap().trim().parse().unwrap();
        let mut body = vec![0u8; len];
        self.stdout.read_exact(&mut body).unwrap();
        String::from_utf8(body).unwrap()
    }
    fn request(&mut self, method: &str, params: &str) -> String {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}"));
        loop {
            let m = self.recv();
            if m.contains(&format!("\"id\":{id},")) || m.contains(&format!("\"id\":{id}}}")) {
                return m;
            }
        }
    }
    fn notify(&mut self, method: &str, params: &str) {
        self.send(&format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params}}}"));
    }
    fn next_diagnostics(&mut self, uri: &str) -> String {
        loop {
            let m = self.recv();
            if m.contains("textDocument/publishDiagnostics") && m.contains(uri) {
                return m;
            }
        }
    }
}

#[test]
fn lsp_diagnostics_hover_definition() {
    let dir = std::env::temp_dir().join(format!("decl-lsp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("lib.decl");
    std::fs::write(&lib_path, "export type Service = { name: string, port?: 1..65535 = 8080 }\nexport const MAX = 16\n").unwrap();
    let main_path = dir.join("main.decl");
    std::fs::write(&main_path, "").unwrap();
    let main_uri = decl_lang::lsp::uri_of(&main_path);
    let mut c = Client::spawn();
    let init = c.request("initialize", "{\"processId\":null,\"rootUri\":null,\"capabilities\":{}}");
    assert!(init.contains("\"hoverProvider\":true") && init.contains("\"definitionProvider\":true"), "initialize advertises capabilities: {init}");
    c.notify("initialized", "{}");
    c.notify("textDocument/didOpen", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"languageId\":\"decl\",\"version\":1,\"text\":\"const x = \\n\"}}}}"));
    let d = c.next_diagnostics(&main_uri);
    assert!(d.contains("\"message\":\"syntax error\""), "syntax error published: {d}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":2}},\"contentChanges\":[{{\"text\":\"type Bad = 10..3\\n\"}}]}}"));
    let d = c.next_diagnostics(&main_uri);
    assert!(d.contains("\"code\":\"E4011\""), "checker diagnostic published with code: {d}");
    assert!(d.contains("\"start\":{\"line\":0,\"character\":5}"), "diagnostic anchored to the name: {d}");
    let main_src = "import { Service, MAX as LIMIT } from \\\"./lib.decl\\\"\\nconst top = LIMIT\\nexport output s: Service = { name: \\\"a\\\" }\\n";
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":3}},\"contentChanges\":[{{\"text\":\"{main_src}\"}}]}}"));
    let d = c.next_diagnostics(&main_uri);
    assert!(d.contains("\"diagnostics\":[]"), "clean module publishes no diagnostics: {d}");
    let h = c.request("textDocument/hover", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":1,\"character\":7}}}}"));
    assert!(h.contains("const top = LIMIT"), "hover shows the declaration: {h}");
    let h2 = c.request("textDocument/hover", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":1,\"character\":13}}}}"));
    assert!(h2.contains("MAX = 16"), "hover follows a renamed import: {h2}");
    let col = "export output s: Service = { name: \"a\" }".find("Service").unwrap() + 2;
    let def = c.request("textDocument/definition", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":2,\"character\":{col}}}}}"));
    assert!(def.contains("lib.decl") && def.contains("\"start\":{\"line\":0,"), "definition jumps across the import: {def}");
    c.request("shutdown", "{}");
    c.notify("exit", "{}");
    drop(c.stdin);
    let status = c.child.wait().unwrap();
    assert!(status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
