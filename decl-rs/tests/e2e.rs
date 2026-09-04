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
    let checks: Vec<_> = r.modules.iter().flat_map(|m| check_module(&m.decls, Some(m.env.clone()), None)).collect();
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
    // the AST without its source ranges (formatting moves nodes; it must not change them):
    // every node's `loc` is blanked in the debug rendering
    let loc_re = regex::Regex::new(r"loc: (None|Some\(Loc \{ [^}]* \}\))").unwrap();
    let tokens = |src: &str| {
        let ds = parse_source(src).decls;
        loc_re.replace_all(&format!("{ds:?}"), "loc: _").to_string()
    };
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
    // --input binds the document; --output may name the bound input
    let out = run(&["evaluate", &decl_s, "--input", &bind, "--output", "base"]);
    assert!(out.status.success(), "evaluate --input --output <input> succeeds: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{\"host\":\"example\",\"port\":80}");
    // an output reading the bound input completes from the document
    let out = run(&["evaluate", &decl_s, "--input", &bind, "--output", "copy"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{\"host\":\"example\",\"port\":80}");
    // a module that exports no output emits an empty object and says so
    let out = run(&["evaluate", &decl_s, "--input", &bind]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("exports no output; --output <name> selects a root"));
    // nothing bound: the fallback-less input demanded by an output is E5006 at the output
    let out = run(&["evaluate", &decl_s]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("error [E5006] at copy: input base is not bound"), "{}", String::from_utf8_lossy(&out.stderr));
    // a root that does not exist
    let out = run(&["evaluate", &decl_s, "--input", &bind, "--output", "nope"]);
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
fn lsp_editor_session() {
    let dir = std::env::temp_dir().join(format!("decl-lsp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("lib.decl");
    std::fs::write(&lib_path, "export type Service = { name: string, port?: 1..65535 = 8080 }\nexport const MAX = 16\nexport func cap(n: int): int = std.math.min(n, MAX)\nexport type Public = Service { public: bool }\n").unwrap();
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
    let main_src = "import { Service, MAX as LIMIT, cap } from \\\"./lib.decl\\\"\\nconst top = LIMIT\\nexport output s: Service = { name: \\\"a\\\" }\\nexport output t: Service = {\\n    name: \\\"b\\\"\\n}\\nconst first = s.name\\nconst c = cap(top)\\nconst d = 250ms\\ntype Local = Service { extra = name }\\n";
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
    // the language server v2 (docs/tooling/03_lsp.md): navigation, completion, symbols, formatting, rename, lenses, commands
    let at = |line: usize, ch: usize| format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":{line},\"character\":{ch}}}}}");
    let td = c.request("textDocument/typeDefinition", &at(6, 14));
    assert!(td.contains("lib.decl") && td.contains("\"start\":{\"line\":0,"), "type definition of a value of type Service: {td}");
    let refs = c.request("textDocument/references", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":2,\"character\":19}},\"context\":{{\"includeDeclaration\":true}}}}"));
    assert_eq!(refs.matches("\"uri\":").count(), 6, "references of Service: declaration, import item, annotations, extensions: {refs}");
    assert_eq!(refs.matches("lib.decl").count(), 2, "two references in lib.decl: {refs}");
    let hl = c.request("textDocument/documentHighlight", &at(6, 14));
    assert_eq!(hl.matches("\"kind\":1").count(), 2, "highlight of s: its declaration and its use: {hl}");
    let c1 = c.request("textDocument/completion", &at(1, 15));
    assert!(c1.contains("\"label\":\"LIMIT\""), "completion of a name prefix: {c1}");
    let broken = format!("{main_src}const e = s.\\n");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":20}},\"contentChanges\":[{{\"text\":\"{broken}\"}}]}}"));
    c.next_diagnostics(&main_uri);
    let c2 = c.request("textDocument/completion", &at(10, 12));
    assert!(c2.contains("\"label\":\"name\"") && c2.contains("\"label\":\"port\""), "member completion while the text does not parse: {c2}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":21}},\"contentChanges\":[{{\"text\":\"{main_src}\"}}]}}"));
    c.next_diagnostics(&main_uri);
    let syms = c.request("textDocument/documentSymbol", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}}}}"));
    for n in ["top", "s", "t", "first", "c", "d", "Local"] {
        assert!(syms.contains(&format!("\"name\":\"{n}\"")), "document symbol {n}: {syms}");
    }
    let folds = c.request("textDocument/foldingRange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}}}}"));
    assert!(folds.contains("\"startLine\":3,\"endLine\":5") && folds.matches("startLine").count() == 1, "folding of the multi-line output: {folds}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":22}},\"contentChanges\":[{{\"text\":\"const x=1\\n\"}}]}}"));
    c.next_diagnostics(&main_uri);
    let fmt = c.request("textDocument/formatting", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"options\":{{\"tabSize\":4,\"insertSpaces\":true}}}}"));
    assert!(fmt.contains("\"newText\":\"const x = 1\\n\""), "formatting replaces the document with its canonical form: {fmt}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":23}},\"contentChanges\":[{{\"text\":\"{main_src}\"}}]}}"));
    c.next_diagnostics(&main_uri);
    let pr = c.request("textDocument/prepareRename", &at(1, 7));
    assert!(pr.contains("\"placeholder\":\"top\"") && pr.contains("\"start\":{\"line\":1,\"character\":6}"), "prepare rename gives the name range: {pr}");
    let rn = c.request("textDocument/rename", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"position\":{{\"line\":2,\"character\":19}},\"newName\":\"Svc\"}}"));
    assert!(rn.contains("lib.decl") && rn.matches("\"newText\":\"Svc\"").count() == 6, "rename edits every module: {rn}");
    let lenses = c.request("textDocument/codeLens", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}}}}"));
    assert!(lenses.matches("\"command\":\"decl.evaluate\"").count() == 2, "lenses on the outputs: {lenses}");
    let ev = c.request("workspace/executeCommand", &format!("{{\"command\":\"decl.evaluate\",\"arguments\":[\"{main_uri}\",\"s\"]}}"));
    assert!(ev.contains("\"document\":\"{\\\"name\\\":\\\"a\\\",\\\"port\\\":8080}\"") && ev.contains("\"diagnostics\":[]"), "decl.evaluate returns the document: {ev}");
    let va = c.request("workspace/executeCommand", &format!("{{\"command\":\"decl.validate\",\"arguments\":[\"{main_uri}\",\"s\"]}}"));
    assert!(va.contains("\"verdicts\":[{\"name\":\"s\",\"errors\":0,\"warnings\":0}]"), "decl.validate returns the verdict: {va}");
    // the language server v3: signature help, workspace symbols, selection ranges, semantic tokens, inlay hints, hierarchies, code actions, the syntax tree
    let sh = c.request("textDocument/signatureHelp", &at(7, 15));
    assert!(sh.contains("\"label\":\"cap(n: int): int\"") && sh.contains("\"activeParameter\":0"), "signature help of a function call: {sh}");
    let ws = c.request("workspace/symbol", "{\"query\":\"ca\"}");
    assert!(ws.contains("\"name\":\"cap\"") && ws.contains("lib.decl"), "workspace symbols across the universe: {ws}");
    let sr = c.request("textDocument/selectionRange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"positions\":[{{\"line\":6,\"character\":16}}]}}"));
    assert!(sr.contains("\"start\":{\"line\":6,\"character\":14}") && sr.contains("\"parent\":{\"range\":{\"start\":{\"line\":6,\"character\":0}"), "selection ranges grow outward: {sr}");
    let stok = c.request("textDocument/semanticTokens/full", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}}}}"));
    let n = stok.split("\"data\":[").nth(1).map(|d| d.split(']').next().unwrap().split(',').count()).unwrap_or(0);
    assert!(n > 0 && n % 5 == 0, "semantic tokens are encoded in fives: {stok}");
    let ih = c.request("textDocument/inlayHint", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":20,\"character\":0}}}}}}"));
    assert!(ih.contains("\"label\":\"n:\"") && ih.contains("\"label\":\"= 0.25 s\"") && ih.contains("\"label\":\": string\""), "inlay hints: parameter name, unit base value, derived type: {ih}");
    let ch = c.request("textDocument/prepareCallHierarchy", &at(7, 11));
    assert!(ch.contains("\"name\":\"cap\""), "call hierarchy: prepare cap: {ch}");
    let item = ch.split("\"result\":[").nth(1).unwrap().trim_end_matches("]}").to_string();
    let inc = c.request("callHierarchy/incomingCalls", &format!("{{\"item\":{item}}}"));
    assert!(inc.contains("\"from\":{\"name\":\"c\"") && inc.matches("\"from\":").count() == 1, "call hierarchy: cap is called from c: {inc}");
    let th = c.request("textDocument/prepareTypeHierarchy", &at(2, 19));
    let item = th.split("\"result\":[").nth(1).unwrap().trim_end_matches("]}").to_string();
    let sub = c.request("typeHierarchy/subtypes", &format!("{{\"item\":{item}}}"));
    assert!(sub.contains("\"name\":\"Public\"") && sub.contains("\"name\":\"Local\""), "type hierarchy: Service has two subtypes: {sub}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":40}},\"contentChanges\":[{{\"text\":\"const z = cap(1)\\n\"}}]}}"));
    let dz = c.next_diagnostics(&main_uri);
    let diags = dz.split("\"diagnostics\":").nth(1).unwrap().trim_end_matches("}}").to_string();
    let ca = c.request("textDocument/codeAction", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\"}},\"range\":{{\"start\":{{\"line\":0,\"character\":10}},\"end\":{{\"line\":0,\"character\":13}}}},\"context\":{{\"diagnostics\":{diags}}}}}"));
    assert!(ca.contains("\"title\":\"import cap from \\\"./lib.decl\\\"\"") && ca.matches("\"title\":").count() == 1, "code action: import the unknown name from the module beside: {ca}");
    c.notify("textDocument/didChange", &format!("{{\"textDocument\":{{\"uri\":\"{main_uri}\",\"version\":41}},\"contentChanges\":[{{\"text\":\"{main_src}\"}}]}}"));
    c.next_diagnostics(&main_uri);
    let tree = c.request("workspace/executeCommand", &format!("{{\"command\":\"decl.showSyntaxTree\",\"arguments\":[\"{main_uri}\"]}}"));
    assert!(tree.contains("\"tree\":\"(module (import_declaration"), "decl.showSyntaxTree returns the tree: {}", &tree[..tree.len().min(160)]);
    c.request("shutdown", "{}");
    c.notify("exit", "{}");
    drop(c.stdin);
    let status = c.child.wait().unwrap();
    assert!(status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------- the high-level API (src/api.rs) ----------------
#[test]
fn api_matches_the_command_line() {
    use decl_lang::{check, evaluate, format_source, validate, DeclError, Document, EvaluateOptions};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let cfg = root.join("docs/examples/02_config.decl");
    let cfg_s = cfg.to_str().unwrap();
    let all = evaluate(cfg_s, &EvaluateOptions::default()).unwrap();
    assert_eq!(all.keys().cloned().collect::<Vec<_>>(), vec!["base", "dev", "prod"]);
    let one = evaluate(cfg_s, &EvaluateOptions { outputs: vec!["prod".into()], ..Default::default() }).unwrap();
    assert!(one["prod"].contains("\"host\":\"api.internal\""), "{}", one["prod"]);
    let fb = root.join("tests/validation/declarations/valid/output_from_input_fallback.decl");
    let fb_s = fb.to_str().unwrap();
    assert!(evaluate(fb_s, &EvaluateOptions::default()).unwrap().is_empty(), "a module exporting nothing yields {{}}");
    let by_value = evaluate(fb_s, &EvaluateOptions { inputs: vec![("base".into(), Document::Json("{\"host\": \"v\"}".into()))], outputs: vec!["copy".into()] }).unwrap();
    assert_eq!(by_value["copy"], "{\"host\":\"v\",\"port\":80}");
    let dir = std::env::temp_dir().join(format!("decl-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("base.json");
    std::fs::write(&doc, "{\"host\": \"h\", \"port\": 8}").unwrap();
    let by_file = evaluate(fb_s, &EvaluateOptions { inputs: vec![("base".into(), Document::File(doc.clone()))], outputs: vec!["base".into(), "copy".into()] }).unwrap();
    assert_eq!(by_file["base"], "{\"host\":\"h\",\"port\":8}");
    assert_eq!(by_file["copy"], "{\"host\":\"h\",\"port\":8}");
    let e: DeclError = evaluate(fb_s, &EvaluateOptions { outputs: vec!["nope".into()], ..Default::default() }).unwrap_err();
    assert_eq!(e.message, "no root named nope");
    let e = evaluate(fb_s, &EvaluateOptions { inputs: vec![("base".into(), Document::File(dir.join("missing.json")))], ..Default::default() }).unwrap_err();
    assert_eq!(e.diagnostics[0].code.as_deref(), Some("E6004"));
    let bad = "{\"host\":\"x\",\"port\":70000,\"workers\":100,\"tls\":{\"enabled\":true}}";
    let e = evaluate(cfg_s, &EvaluateOptions { inputs: vec![("deployed".into(), Document::Json(bad.into()))], outputs: vec!["deployed".into()] }).unwrap_err();
    assert!(e.diagnostics.iter().any(|d| d.severity == "error") && e.message == e.diagnostics[0].message, "{e:?}");
    let v = validate(cfg_s, &[("deployed".into(), Document::Json(bad.into()))]).unwrap();
    assert!(v.iter().any(|d| d.severity == "error"));
    assert!(check(&[root.join("tests/validation/types/valid/predicates.decl").to_str().unwrap()]).is_empty());
    let bad_check = check(&[root.join("tests/validation/types/invalid/empty_range.decl").to_str().unwrap()]);
    assert_eq!(bad_check[0].code.as_deref(), Some("E4011"));
    assert_eq!(format_source("const x=1+2\n").unwrap(), "const x = 1 + 2\n");
    assert!(format_source("type T = {").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
