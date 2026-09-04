//! The REPL (docs/tooling/02_repl.md) — a port of the reference
//! implementation's repl.ts: an interactive session over a universe —
//! expressions evaluated partially, session outputs and declarations,
//! documents bound and edited with exact undo, and the command-line verbs
//! root for root. Everything it prints goes to standard output; a scripted
//! session (`--script`) prints the transcript the terminal would show, so
//! the three implementations can be diffed.
use crate::session::{fmt_diag, parse_decl, parse_expr, pretty_json, BindSource, EditKind, Mode, Op, Session, SessionError};
use regex::Regex;
use std::io::{BufRead, Read, Write};

pub const COMMANDS: &[(&str, &str, &str)] = &[
    // the universe
    (":load file.decl", "open the universe from an entry module (a new session)", "universe"),
    (":reload", "re-read every module of the universe from disk", "universe"),
    (":roots", "the roots of the universe and of the session", "universe"),
    // documents
    (":bind name=doc.json", "bind a JSON file to an input", "documents"),
    (":bind name { … }", "bind an inline JSON document", "documents"),
    (":bind name = expr", "bind the value of an expression as the document", "documents"),
    (":unbind name", "drop the binding", "documents"),
    (":create path = expr", "add a member, entry, or element at a path of a document", "documents"),
    (":update path = expr", "replace the value at a path of a document", "documents"),
    (":remove path", "remove the value at a path of a document", "documents"),
    (":diff name", "the document against what it started from", "documents"),
    (":save name=file", "write the document of a root to a file", "documents"),
    // session declarations
    (":drop name", "remove a session declaration", "declarations"),
    (":write file.decl", "write the scratch module to a file", "declarations"),
    (":session", "the session's declarations and documents", "declarations"),
    (":reset", "drop every binding, edit, and declaration", "declarations"),
    // evaluation and validation
    (":check", "static diagnostics of every module", "evaluation"),
    (":evaluate [root…]", "full evaluation: the documents of the roots", "evaluation"),
    (":validate [root…]", "full validation: every diagnostic, then a verdict per root", "evaluation"),
    (":fmt", "the scratch module, canonically formatted", "evaluation"),
    // inspection
    (":type expr", "the static type of an expression", "inspection"),
    (":doc name", "a declaration and its documentation", "inspection"),
    (":path expr", "the canonical path of a place", "inspection"),
    (":trace path", "the derivation of a place, or its root cause", "inspection"),
    (":complete text", "the completions offered at the end of the text", "inspection"),
    // history
    (":undo [n]", "step the log back", "history"),
    (":redo [n]", "step forward again", "history"),
    (":history [file]", "the log, or write it as a session file", "history"),
    // the session
    (":time", "wall time of the last evaluation", "session"),
    (":set pretty|compact", "value printing", "session"),
    (":help [command]", "these commands", "session"),
    (":quit", "end the session", "session"),
];

fn command_names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for c in COMMANDS {
        let n = c.0.split(' ').next().unwrap_or("");
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

const KEYWORDS: &[&str] = &["if", "then", "else", "for", "in", "match", "with", "matches", "true", "false", "null", "export"];

fn is_decl_head(t: &str) -> bool {
    Regex::new(r"^\s*(?:export\s+)?(type|const|func|output|input|diagnostic|dimension|unit|import)\b").unwrap().is_match(t)
}
fn is_ident(s: &str) -> bool {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap().is_match(s)
}

/// `name = expr` / `name: T = expr` — a session output (the reference's OUTPUT_HEAD)
fn output_head(t: &str) -> Option<(String, Option<String>, String)> {
    let re = Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*").unwrap();
    let m = re.captures(t)?;
    let name = m.get(1).unwrap().as_str().to_string();
    let mut rest = &t[m.get(0).unwrap().end()..];
    let mut ty: Option<String> = None;
    if let Some(r) = rest.strip_prefix(':') {
        let r = r.trim_start();
        let eq = r.find('=')?;
        let type_part = &r[..eq];
        if type_part.trim_end().is_empty() {
            return None;
        }
        ty = Some(type_part.trim().to_string());
        rest = &r[eq..];
    }
    let rest = rest.strip_prefix('=')?;
    if rest.starts_with('=') {
        return None;
    }
    let expr = rest.trim_start();
    if expr.is_empty() {
        return None;
    }
    Some((name, ty, expr.trim().to_string()))
}

/// does the input so far leave an expression open (§2.9)?
pub fn needs_more(text: &str) -> bool {
    let cs: Vec<char> = text.chars().collect();
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = in_str {
            if c == '\\' {
                i += 1;
            } else if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '`' {
            in_str = Some(c);
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            match cs[i..].iter().position(|&x| x == '\n') {
                Some(nl) => i += nl,
                None => break,
            }
        } else if "{[(".contains(c) {
            depth += 1;
        } else if "}])".contains(c) {
            depth -= 1;
        }
        i += 1;
    }
    if depth > 0 || in_str == Some('`') {
        return true;
    }
    if text.trim_start().starts_with(':') {
        return false; // a command: only an open bracket continues it
    }
    let no_comment = Regex::new(r"//[^\n]*$").unwrap().replace(text, "").to_string();
    let tail = no_comment.trim_end();
    Regex::new(r"(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$").unwrap().is_match(tail)
}

pub struct Repl {
    pub session: Session,
    pub compact: bool,
    pub errors: usize,
    pub quit_requested: bool,
    out: Box<dyn Fn(&str)>,
    buffer: Vec<String>,
}

impl Repl {
    pub fn new(out: Box<dyn Fn(&str)>, entry: Option<&str>) -> Repl {
        Repl { session: Session::new(entry), compact: false, errors: 0, quit_requested: false, out, buffer: vec![] }
    }

    /// feed one line; returns true when the input is complete and was handled
    pub fn line(&mut self, text: &str) -> bool {
        self.buffer.push(text.to_string());
        let whole = self.buffer.join("\n");
        if needs_more(&whole) {
            return false;
        }
        self.buffer.clear();
        self.input(&whole);
        true
    }
    pub fn pending(&self) -> bool {
        !self.buffer.is_empty()
    }

    fn out(&self, line: &str) {
        (self.out)(line)
    }
    fn error(&mut self, msg: &str) {
        self.errors += 1;
        self.out(&format!("error: {msg}"));
    }
    fn diag(&self, d: &crate::semantics::Diag, in_file: Option<&str>) {
        self.out(&fmt_diag(d, in_file));
    }
    fn value(&self, json: &str) {
        if self.compact {
            self.out(json);
        } else {
            self.out(&pretty_json(json));
        }
    }

    pub fn input(&mut self, text: &str) {
        let t = text.trim();
        if t.is_empty() || (t.starts_with("//") && !t.starts_with("///")) {
            return;
        }
        let r = if t.starts_with(':') {
            self.command(t)
        } else if is_decl_head(t) {
            self.add_declaration(t)
        } else if let Some((name, ty, expr)) = output_head(t).filter(|(n, _, _)| !KEYWORDS.contains(&n.as_str())) {
            self.session_output(&name, ty, &expr)
        } else {
            self.expression(t)
        };
        if let Err(e) = r {
            self.error(&e.0);
        }
    }

    fn expression(&mut self, text: &str) -> Result<(), SessionError> {
        parse_expr(text)?;
        let r = self.session.evaluate_expr(text)?;
        for d in &r.diags {
            self.diag(d, None);
        }
        match (&r.error, &r.value) {
            (Some((code, message)), _) => {
                if !message.is_empty() {
                    self.out(&format!("error{}: {message}", code.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default()));
                }
                self.out("(invalid)");
            }
            (None, Some(v)) => self.value(v),
            (None, None) => self.out("(invalid)"),
        }
        self.out("(partial)");
        Ok(())
    }
    fn add_declaration(&mut self, text: &str) -> Result<(), SessionError> {
        let (_, name) = parse_decl(text)?;
        self.session.apply(Op::Declare { name, text: text.trim().to_string() })
    }
    fn session_output(&mut self, name: &str, ty: Option<String>, expr: &str) -> Result<(), SessionError> {
        parse_expr(expr)?;
        if let Some(t) = &ty {
            parse_decl(&format!("output {name}: {t} = 0"))?;
        }
        self.session.apply(Op::Output { name: name.to_string(), ty, expr: expr.to_string() })
    }

    fn command(&mut self, t: &str) -> Result<(), SessionError> {
        let sp = t.find(char::is_whitespace);
        let cmd = match sp { Some(i) => &t[..i], None => t };
        let rest = match sp { Some(i) => t[i + 1..].trim(), None => "" }.to_string();
        let cmd = cmd.to_string();
        let no_args = |rest: &str| -> Result<(), SessionError> {
            if !rest.is_empty() { Err(SessionError(format!("{cmd} takes no argument"))) } else { Ok(()) }
        };
        let one_name = |rest: &str| -> Result<String, SessionError> {
            if !is_ident(rest) { Err(SessionError(format!("{cmd} expects a name"))) } else { Ok(rest.to_string()) }
        };
        let entry_abs = self.session.entry_abs().display().to_string();
        let in_file = |file: &str| -> Option<String> { if file == entry_abs { None } else { Some(file.to_string()) } };
        match cmd.as_str() {
            ":load" => {
                if rest.is_empty() {
                    return Err(SessionError(":load expects a file".into()));
                }
                self.session = Session::new(Some(&rest));
                Ok(())
            }
            ":reload" => {
                no_args(&rest)?;
                let op = self.session.reload_op();
                self.session.apply(op)
            }
            ":roots" => {
                no_args(&rest)?;
                let rs = self.session.roots();
                if rs.is_empty() {
                    self.out("(no roots)");
                    return Ok(());
                }
                for r in rs {
                    let status = if r.session {
                        "session".to_string()
                    } else if r.kind == "output" {
                        if r.binding == "detached" { "detached".into() } else if r.exported { "exported".into() } else { "local".into() }
                    } else {
                        r.binding.clone()
                    };
                    let line = format!("{:<7} {:<16} {:<12} {:<16} {}{}", r.kind, r.name, status, r.module, r.detail, if r.edited { " (edited)" } else { "" });
                    self.out(line.trim_end());
                }
                Ok(())
            }
            ":bind" => {
                let eq_re = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([\s\S]+)$").unwrap();
                if let Some(m) = eq_re.captures(&rest) {
                    let name = m.get(1).unwrap().as_str().to_string();
                    let val = m.get(2).unwrap().as_str();
                    let after_name = &rest[name.len()..];
                    let starts_bracket = val.trim().starts_with('[') || val.trim().starts_with('{');
                    if !starts_bracket && !after_name.starts_with(char::is_whitespace) {
                        // name=file (no spaces around =)
                        let file = val.trim().to_string();
                        let text = std::fs::read_to_string(&file).map_err(|_| SessionError(format!("cannot read {file}")))?;
                        return self.session.apply(Op::Bind { name, src: BindSource::File { file, text } });
                    }
                    return self.session.apply(Op::Bind { name, src: BindSource::Expr { text: val.trim().to_string() } });
                }
                let inline_re = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s+([\[{][\s\S]*)$").unwrap();
                if let Some(m) = inline_re.captures(&rest) {
                    let name = m.get(1).unwrap().as_str().to_string();
                    let text = m.get(2).unwrap().as_str().to_string();
                    return self.session.apply(Op::Bind { name, src: BindSource::Inline { text } });
                }
                Err(SessionError(":bind expects name=doc.json, name { … }, or name = expr".into()))
            }
            ":unbind" => {
                let name = one_name(&rest)?;
                self.session.apply(Op::Unbind { name })
            }
            ":create" | ":update" => {
                let re = Regex::new(r"^(\S+)\s*=\s*([\s\S]+)$").unwrap();
                let Some(m) = re.captures(&rest) else { return Err(SessionError(format!("{cmd} expects path = expr"))) };
                let kind = if cmd == ":create" { EditKind::Create } else { EditKind::Update };
                self.session.apply(Op::Edit { kind, path: m.get(1).unwrap().as_str().to_string(), expr: Some(m.get(2).unwrap().as_str().trim().to_string()) })
            }
            ":remove" => {
                if rest.is_empty() || rest.contains(char::is_whitespace) {
                    return Err(SessionError(":remove expects a path".into()));
                }
                self.session.apply(Op::Edit { kind: EditKind::Remove, path: rest.clone(), expr: None })
            }
            ":diff" => {
                let name = one_name(&rest)?;
                for l in self.session.diff(&name)? {
                    self.out(&l);
                }
                Ok(())
            }
            ":save" => {
                let re = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=(\S+)$").unwrap();
                let Some(m) = re.captures(&rest) else { return Err(SessionError(":save expects name=file".into())) };
                self.session.save(m.get(1).unwrap().as_str(), m.get(2).unwrap().as_str())
            }
            ":drop" => {
                let name = one_name(&rest)?;
                self.session.apply(Op::Drop { name })
            }
            ":write" => {
                if rest.is_empty() {
                    return Err(SessionError(":write expects a file".into()));
                }
                self.session.write(&rest)
            }
            ":session" => {
                no_args(&rest)?;
                let ls = self.session.session_lines();
                if ls.is_empty() {
                    self.out("(empty session)");
                }
                for l in ls {
                    self.out(&l);
                }
                Ok(())
            }
            ":reset" => {
                no_args(&rest)?;
                self.session.apply(Op::Reset)
            }
            ":check" => {
                no_args(&rest)?;
                let cs = self.session.check();
                for (file, d) in &cs {
                    self.diag(d, in_file(file).as_deref());
                }
                if cs.is_empty() {
                    self.out("ok");
                }
                Ok(())
            }
            ":evaluate" => {
                let names: Vec<String> = if rest.is_empty() { vec![] } else { Regex::new(r"[\s,]+").unwrap().split(&rest).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect() };
                let (run, docs, exported) = self.session.evaluate(&names)?;
                for d in &run.load_diags {
                    self.diag(d, None);
                }
                for (file, d) in &run.checks {
                    self.diag(d, in_file(file).as_deref());
                }
                for d in &run.session_checks {
                    self.diag(d, None);
                }
                for d in &run.diags {
                    self.diag(d, None);
                }
                if run.entry.is_none() {
                    return Ok(());
                }
                if run.eng.is_none() {
                    self.out("(not evaluated)");
                    return Ok(());
                }
                if exported {
                    if docs.iter().any(|(_, j)| j.is_none()) {
                        self.out("(invalid)");
                        return Ok(());
                    }
                    let text = format!("{{{}}}", docs.iter().map(|(n, j)| format!("{}:{}", crate::semantics::json_str(n), j.clone().unwrap_or_default())).collect::<Vec<_>>().join(","));
                    self.value(&text);
                    return Ok(());
                }
                let many = docs.len() > 1;
                for (name, json) in &docs {
                    if many {
                        self.out(&format!("{name}:"));
                    }
                    match json {
                        None => self.out("(invalid)"),
                        Some(j) => self.value(j),
                    }
                }
                Ok(())
            }
            ":validate" => {
                let names: Vec<String> = if rest.is_empty() { vec![] } else { Regex::new(r"[\s,]+").unwrap().split(&rest).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect() };
                let (run, verdicts, diags) = self.session.validate(&names)?;
                for d in &run.load_diags {
                    self.diag(d, None);
                }
                for (file, d) in &run.checks {
                    self.diag(d, in_file(file).as_deref());
                }
                for d in &run.session_checks {
                    self.diag(d, None);
                }
                if run.eng.is_none() {
                    self.out("(not evaluated)");
                    return Ok(());
                }
                for d in &diags {
                    self.diag(d, None);
                }
                if verdicts.is_empty() {
                    self.out("(no roots)");
                }
                for (name, errors, warnings) in &verdicts {
                    let n = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
                    if *errors == 0 && *warnings == 0 {
                        self.out(&format!("{name}: ok"));
                    } else {
                        let parts: Vec<String> = [if *errors > 0 { n(*errors, "error") } else { String::new() }, if *warnings > 0 { n(*warnings, "warning") } else { String::new() }].into_iter().filter(|s| !s.is_empty()).collect();
                        self.out(&format!("{name}: {}", parts.join(", ")));
                    }
                }
                Ok(())
            }
            ":fmt" => {
                no_args(&rest)?;
                let t = self.session.fmt()?;
                if t.is_empty() {
                    self.out("(empty session)");
                } else {
                    self.out(t.strip_suffix('\n').unwrap_or(&t));
                }
                Ok(())
            }
            ":type" => {
                if rest.is_empty() {
                    return Err(SessionError(":type expects an expression".into()));
                }
                let (ty, maybe_absent, diags) = self.session.type_of(&rest)?;
                for d in &diags {
                    self.diag(d, None);
                }
                self.out(&format!("{ty}{}", if maybe_absent { "  (maybe absent)" } else { "" }));
                Ok(())
            }
            ":doc" => {
                if rest.is_empty() {
                    return Err(SessionError(":doc expects a name".into()));
                }
                for l in self.session.doc_of(&rest)? {
                    self.out(&l);
                }
                Ok(())
            }
            ":path" => {
                if rest.is_empty() {
                    return Err(SessionError(":path expects an expression".into()));
                }
                let p = self.session.path_of(&rest)?;
                self.out(&p);
                Ok(())
            }
            ":trace" => {
                if rest.is_empty() || rest.contains(char::is_whitespace) {
                    return Err(SessionError(":trace expects a path".into()));
                }
                for l in self.session.trace(&rest)? {
                    self.out(&l);
                }
                Ok(())
            }
            ":complete" => {
                let names = command_names();
                let cs = self.session.complete(&rest, &names);
                if cs.is_empty() {
                    self.out("(no completions)");
                }
                for c in cs {
                    self.out(&c);
                }
                Ok(())
            }
            ":undo" | ":redo" => {
                let n = if rest.is_empty() { Some(1) } else { js_parse_int(&rest) };
                let Some(n) = n.filter(|n| *n >= 1) else { return Err(SessionError(format!("{cmd} expects a count"))) };
                let k = if cmd == ":undo" { self.session.undo(n as usize) } else { self.session.redo(n as usize) };
                if k == 0 {
                    self.out(if cmd == ":undo" { "nothing to undo" } else { "nothing to redo" });
                }
                Ok(())
            }
            ":history" => {
                if !rest.is_empty() {
                    let text = format!("{}\n", self.session.script_lines().join("\n"));
                    return std::fs::write(&rest, text).map_err(|_| SessionError(format!("cannot write {rest}")));
                }
                for l in self.session.history_lines() {
                    self.out(&l);
                }
                Ok(())
            }
            ":time" => {
                no_args(&rest)?;
                match self.session.last_timing.get() {
                    None => self.out("nothing evaluated yet"),
                    Some(t) => self.out(&format!("total {:.1} ms (load {:.1} ms, check {:.1} ms, bind {:.1} ms, evaluate {:.1} ms)", t.total, t.load, t.check, t.bind, t.evaluate)),
                }
                Ok(())
            }
            ":set" => {
                match rest.as_str() {
                    "pretty" => self.compact = false,
                    "compact" => self.compact = true,
                    _ => return Err(SessionError(":set expects pretty or compact".into())),
                }
                Ok(())
            }
            ":help" => {
                let want = if rest.is_empty() { None } else { Some(if rest.starts_with(':') { rest.clone() } else { format!(":{rest}") }) };
                let rows: Vec<&(&str, &str, &str)> = COMMANDS.iter().filter(|c| want.as_ref().map(|w| c.0.split(' ').next() == Some(w.as_str())).unwrap_or(true)).collect();
                if rows.is_empty() {
                    return Err(SessionError(format!("unknown command {rest}")));
                }
                let mut cat = "";
                for (form, what, c) in rows {
                    if want.is_none() && *c != cat {
                        cat = c;
                        self.out(&format!("{cat}:"));
                    }
                    self.out(&format!("  {:<24} {what}", form));
                }
                Ok(())
            }
            ":quit" => {
                no_args(&rest)?;
                self.quit_requested = true;
                Ok(())
            }
            _ => Err(SessionError(format!("unknown command {cmd}"))),
        }
    }
}

/// JavaScript's parseInt(text, 10): leading digits, or NaN (None)
fn js_parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let n: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
    if n.is_empty() {
        return None;
    }
    let v: i64 = n.parse().ok()?;
    Some(if neg { -v } else { v })
}

// ---------------- the command ----------------
pub fn run_repl(args: Vec<String>) -> i32 {
    let mut entry: Option<String> = None;
    let mut script: Option<String> = None;
    let mut compact = false;
    let mut inputs: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--script" {
            i += 1;
            script = args.get(i).cloned();
        } else if a == "--input" {
            i += 1;
            if let Some(s) = args.get(i) {
                inputs.push(s.clone());
            }
        } else if a == "--compact" {
            compact = true;
        } else if a.starts_with("--") {
            eprintln!("unknown option {a}");
            return 2;
        } else if entry.is_none() {
            entry = Some(a.clone());
        } else {
            eprintln!("decl repl takes one entry file");
            return 2;
        }
        i += 1;
    }
    if script.is_none() && entry.is_none() && !inputs.is_empty() {
        eprintln!("--input needs an entry file");
        return 2;
    }
    for spec in &inputs {
        if !spec.contains('=') {
            eprintln!("--input expects name=doc.json, got {spec}");
            return 2;
        }
    }

    let out: Box<dyn Fn(&str)> = Box::new(|l: &str| {
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        let _ = writeln!(h, "{l}");
    });
    let mut repl = Repl::new(out, entry.as_deref());
    repl.compact = compact;
    for spec in &inputs {
        repl.input(&format!(":bind {spec}"));
    }

    if let Some(script) = script {
        let text = if script == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map(|_| s).ok()
        } else {
            std::fs::read_to_string(&script).ok()
        };
        let Some(text) = text else {
            eprintln!("cannot read {script}");
            return 2;
        };
        let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
        for l in text.split('\n') {
            let prompt = if repl.pending() { ". " } else { "> " };
            println!("{prompt}{l}");
            repl.line(l);
            if repl.quit_requested {
                break;
            }
        }
        if repl.pending() {
            repl.line("");
        }
        return if repl.errors > 0 { 1 } else { 0 };
    }

    // interactive: a plain line loop over standard input, with the prompts
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("{}", if repl.pending() { ". " } else { "> " });
        let _ = std::io::stdout().flush();
        let Some(Ok(l)) = lines.next() else { break };
        repl.line(&l);
        if repl.quit_requested {
            break;
        }
    }
    let _ = Mode::Full;
    0
}
