//! The renderer (docs/tooling/05_render.md): the form a module declares
//! for an output with `@render` — a format and a layout, a template, a
//! destination, a fan-out — read from the annotation (§3), the structured
//! text of a document in that form (§4), and the templates (§5) and the
//! fan-out (§6) that turn one evaluated root into text or files. The
//! command line, the REPL, the library, and the editor preview all emit
//! through here, so that the three implementations print the same bytes.
//! A port of the reference's render.ts.
use crate::ast::{Decl, Expr};
use crate::semantics::{NatFn, Value, R};
use crate::yaml::{to_json, to_yaml};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use regex::Regex;
use std::sync::LazyLock;

/// a template's delimiters (§5.2): each an opener and a closer
#[derive(Debug, Clone, PartialEq)]
pub struct Delimiters {
    /// `{= =}`
    pub value: (String, String),
    /// `{% %}`
    pub statement: (String, String),
    /// `{# #}`
    pub comment: (String, String),
}
impl Default for Delimiters {
    fn default() -> Self {
        Delimiters {
            value: ("{=".into(), "=}".into()),
            statement: ("{%".into(), "%}".into()),
            comment: ("{#".into(), "#}".into()),
        }
    }
}

/// the declared form of a root (§3): what `@render` says, every key optional
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Form {
    /// `"yaml"`, else JSON
    pub yaml: bool,
    /// the layout (§4)
    pub indent: Option<usize>,
    /// a template file (§5), relative to the module's directory
    pub template: Option<String>,
    /// the default destination (§3.2)
    pub file: Option<String>,
    /// fan-out (§6)
    pub each: Option<String>,
    /// the template's delimiters
    pub delimiters: Option<Delimiters>,
}

const FORM_KEYS: [&str; 6] = ["format", "indent", "template", "file", "each", "delimiters"];

// a literal value in an annotation argument: a string, an integer, a
// bool, or null (a negative integer is a unary minus over a literal)
fn literal(e: &Expr) -> Option<Value> {
    match e {
        Expr::Lit(v) => Some(v.clone()),
        Expr::Un { op, x } if op == "-" => match &**x {
            Expr::Lit(Value::Int(i)) => Some(Value::Int(-i.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// the form `@render` declares on a declaration (§3), or the E7004 message
/// naming what is wrong with it; a declaration without one is canonical JSON
pub fn declared_form(decl: &Decl) -> Result<Form, String> {
    let anns: Vec<_> = decl
        .annotations
        .iter()
        .filter(|a| a.name == "render")
        .collect();
    if anns.is_empty() {
        return Ok(Form::default());
    }
    if anns.len() > 1 {
        return Err("more than one @render".into());
    }
    let a = anns[0];
    let entries = match a.args.as_slice() {
        [arg] => match &**arg {
            Expr::Obj(entries) => entries,
            _ => return Err("@render takes one object literal".into()),
        },
        _ => return Err("@render takes one object literal".into()),
    };
    let mut form = Form::default();
    let mut seen: Vec<&str> = vec![];
    for (key, val) in entries.iter() {
        if !FORM_KEYS.contains(&key.as_str()) {
            return Err(format!("@render: unknown key {key}"));
        }
        if seen.contains(&key.as_str()) {
            return Err(format!("@render: key {key} repeats"));
        }
        seen.push(key);
        let lit = literal(val);
        match key.as_str() {
            "format" => match lit {
                Some(Value::Str(s)) if s == "json" => form.yaml = false,
                Some(Value::Str(s)) if s == "yaml" => form.yaml = true,
                _ => return Err("@render: format must be \"json\" or \"yaml\"".into()),
            },
            "indent" => match lit {
                Some(Value::Int(i)) if i >= BigInt::from(0) && i <= BigInt::from(16) => {
                    form.indent = Some(i.to_usize().unwrap())
                }
                _ => return Err("@render: indent must be an integer in 0..16".into()),
            },
            "template" | "file" | "each" => match lit {
                Some(Value::Str(s)) if !s.is_empty() => match key.as_str() {
                    "template" => form.template = Some(s),
                    "file" => form.file = Some(s),
                    _ => form.each = Some(s),
                },
                _ => return Err(format!("@render: {key} must be a non-empty string")),
            },
            _ => form.delimiters = Some(delimiters(val).map_err(|m| format!("@render: {m}"))?),
        }
    }
    Ok(form)
}

fn delimiters(e: &Expr) -> Result<Delimiters, String> {
    let Expr::Obj(entries) = e else {
        return Err("delimiters must be an object of three pairs".into());
    };
    let mut out = Delimiters::default();
    let mut seen: Vec<&str> = vec![];
    for (key, val) in entries.iter() {
        if !["value", "statement", "comment"].contains(&key.as_str()) {
            return Err(format!("delimiters: unknown key {key}"));
        }
        if seen.contains(&key.as_str()) {
            return Err(format!("delimiters: key {key} repeats"));
        }
        seen.push(key);
        let Expr::Arr(items) = &**val else {
            return Err(format!("delimiters: {key} must be a pair of strings"));
        };
        if items.len() != 2 || items.iter().any(|(spread, _)| *spread) {
            return Err(format!("delimiters: {key} must be a pair of strings"));
        }
        let mut pair: Vec<String> = vec![];
        for (_, it) in items.iter() {
            match literal(it) {
                Some(Value::Str(s)) if !s.is_empty() => pair.push(s),
                _ => {
                    return Err(format!(
                        "delimiters: {key} must be a pair of non-empty strings"
                    ))
                }
            }
        }
        let p = (pair[0].clone(), pair[1].clone());
        match key.as_str() {
            "value" => out.value = p,
            "statement" => out.statement = p,
            _ => out.comment = p,
        }
    }
    let openers = [&out.value.0, &out.statement.0, &out.comment.0];
    if openers[0] == openers[1] || openers[0] == openers[2] || openers[1] == openers[2] {
        return Err("delimiters: the three openers must differ".into());
    }
    Ok(out)
}

/// the structured text of a document (read_json's shape) in a format and layout (§4), one trailing newline
pub fn layout(raw: &Value, yaml: bool, indent: Option<usize>) -> String {
    if yaml {
        to_yaml(raw, indent.unwrap_or(2)) + "\n"
    } else {
        to_json(raw, indent.unwrap_or(0)) + "\n"
    }
}

// ---------------- templates (§5) ----------------
// A template is text with tags in it: `{= expr =}` places the text form of
// a Decl expression, `{% stmt %}` is a statement, `{# … #}` a comment,
// `{% raw %}…{% endraw %}` verbatim text. The dialect is fixed here and
// implemented three times; expressions are the language's, evaluated by
// its engine over the root's document (§5.4).

use crate::engine::{fmt_f, Engine, Inst};
use crate::parse::{json_unquote, parse_expr_text};
use crate::semantics::{
    path_str, read_json, rec_members, Diag, Env, EvalErr, Fail, MKind, Scope, Seg, SlotState,
};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// a rendering diagnostic: the code, the message, and where — `L:C` of the tag, or a document path
#[derive(Debug, Clone)]
pub struct RenderError {
    /// the code (§5.8)
    pub code: String,
    /// the message
    pub message: String,
    /// `L:C` of the tag, or a document path
    pub at: String,
    /// the file the diagnostic is reported against: the template's path as given, or None for the module's
    pub file: Option<String>,
}
impl RenderError {
    fn new(
        code: &str,
        message: impl Into<String>,
        at: impl Into<String>,
        file: Option<&str>,
    ) -> Self {
        RenderError {
            code: code.to_string(),
            message: message.into(),
            at: at.into(),
            file: file.map(str::to_string),
        }
    }
    /// the diagnostic (§12.2)
    pub fn diag(&self) -> Diag {
        Diag::error(self.message.clone(), self.at.clone(), Some(&self.code))
    }
}
type RR<T> = Result<T, RenderError>;

#[derive(Debug, Clone, Copy)]
struct Pos {
    line: usize,
    col: usize,
}
impl Pos {
    fn at(&self) -> String {
        format!("{}:{}", self.line, self.col)
    }
}
#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Value {
        expr: Rc<Expr>,
        at: Pos,
    },
    If {
        arms: Vec<(Option<Rc<Expr>>, Vec<Node>, Pos)>,
    },
    For {
        vars: Vec<String>,
        iter: Rc<Expr>,
        filter: Option<Rc<Expr>>,
        body: Vec<Node>,
        empty: Option<Vec<Node>>,
        at: Pos,
    },
    Set {
        name: String,
        expr: Rc<Expr>,
        at: Pos,
    },
    Include {
        path: String,
        at: Pos,
    },
}

/// a parsed template: its path as given (diagnostics), its directory (includes), its nodes
#[derive(Debug, Clone)]
pub struct Template {
    /// the path as given
    pub path: String,
    /// the absolute directory its includes resolve from
    pub dir: String,
    nodes: Vec<Node>,
}

enum Tok {
    Text(String),
    Tag { stmt: bool, text: String, at: Pos },
}

fn pos_of(src: &[char], k: usize) -> Pos {
    let mut line = 1;
    let mut last: isize = -1;
    for (j, c) in src.iter().enumerate().take(k) {
        if *c == '\n' {
            line += 1;
            last = j as isize;
        }
    }
    Pos {
        line,
        col: (k as isize - last) as usize,
    }
}
fn starts_with(src: &[char], i: usize, pat: &[char]) -> bool {
    i + pat.len() <= src.len() && src[i..i + pat.len()] == *pat
}
fn escape_re(s: &str) -> String {
    regex::escape(s)
}

// the lexer: text and tags, with the whitespace rules of §5.2 applied —
// trim_blocks and lstrip_blocks on for statements, `-` and `+` overriding
fn lex(src: &[char], path: &str, d: &Delimiters) -> RR<Vec<Tok>> {
    let chars = |s: &str| s.chars().collect::<Vec<char>>();
    let mut openers: Vec<(Vec<char>, usize)> = vec![
        (chars(&d.value.0), 0),
        (chars(&d.statement.0), 1),
        (chars(&d.comment.0), 2),
    ];
    openers.sort_by_key(|o| std::cmp::Reverse(o.0.len())); // longest first at every position
    let closers = [
        chars(&d.value.1),
        chars(&d.statement.1),
        chars(&d.comment.1),
    ];
    let mut out: Vec<Tok> = vec![];
    let mut i = 0;
    let mut text = String::new(); // the text since the last tag
    let mut after: u8; // what the last tag asks of the text after it: 0 none, 1 trim, 2 strip
    let mut first = true; // no tag yet: the text starts the template
    let fail = |code: &str, message: String, k: usize| -> RenderError {
        RenderError::new(code, message, pos_of(src, k).at(), Some(path))
    };
    while i < src.len() {
        let Some((opener, kind)) = openers.iter().find(|(o, _)| starts_with(src, i, o)) else {
            text.push(src[i]);
            i += 1;
            continue;
        };
        let kind = *kind;
        let start = i;
        let mut j = i + opener.len();
        // the modifier after the opener
        let mut left: Option<char> = None;
        if kind != 2 && matches!(src.get(j), Some('-') | Some('+')) {
            left = Some(src[j]);
            j += 1;
        }
        let closer = &closers[kind];
        // the tag's end: the closer, possibly preceded by a modifier
        let mut end: Option<usize> = None;
        let mut right: Option<char> = None;
        let mut k = j;
        while k + closer.len() <= src.len() {
            if starts_with(src, k, closer) {
                if kind != 2 && k > j && matches!(src[k - 1], '-' | '+') {
                    right = Some(src[k - 1]);
                    end = Some(k - 1);
                } else {
                    end = Some(k);
                }
                break;
            }
            k += 1;
        }
        let opener_s: String = opener.iter().collect();
        let Some(end) = end else {
            return Err(fail("E7001", format!("unclosed {opener_s} tag"), start));
        };
        let body: String = src[j..end].iter().collect();
        let mut next = end + usize::from(right.is_some()) + closer.len();
        // the text before the tag: `-` strips all white space, a statement's
        // default strips the indentation of its line (lstrip_blocks), `+` keeps
        let mut before = text.clone();
        if left == Some('-') {
            before = before.trim_end().to_string();
        } else if kind == 1 && left != Some('+') {
            let trimmed = before.trim_end_matches([' ', '\t']);
            if trimmed.ends_with('\n') || (first && trimmed.is_empty()) {
                before = trimmed.to_string();
            }
        }
        if !before.is_empty() {
            out.push(Tok::Text(before));
        }
        text.clear();
        first = false;
        if kind == 2 {
            after = 0;
        } else if kind == 1 && body.trim() == "raw" {
            // verbatim text to the matching endraw, which may carry modifiers
            let re = Regex::new(&format!(
                "{}[-+]?\\s*endraw\\s*[-+]?{}",
                escape_re(&d.statement.0),
                escape_re(&d.statement.1)
            ))
            .unwrap();
            let rest: String = src[next..].iter().collect();
            let Some(m) = re.find(&rest) else {
                return Err(fail("E7001", "unclosed {% raw %}".into(), start));
            };
            let mut raw = rest[..m.start()].to_string();
            if right == Some('-') {
                raw = raw.trim_start().to_string();
            } else if right != Some('+') {
                raw = raw
                    .strip_prefix("\r\n")
                    .or_else(|| raw.strip_prefix('\n'))
                    .unwrap_or(&raw)
                    .to_string();
            }
            let end_tag: Vec<char> = m.as_str().chars().collect();
            let end_left = end_tag[d.statement.0.chars().count()];
            let end_right = end_tag[end_tag.len() - d.statement.1.chars().count() - 1];
            if end_left == '-' {
                raw = raw.trim_end().to_string();
            } else if end_left != '+' {
                let t = raw.trim_end_matches([' ', '\t']);
                if t.ends_with('\n') {
                    raw = t.to_string();
                }
            }
            out.push(Tok::Text(raw));
            next += rest[..m.end()].chars().count();
            after = match end_right {
                '-' => 2,
                '+' => 0,
                _ => 1,
            };
        } else {
            out.push(Tok::Tag {
                stmt: kind == 1,
                text: body,
                at: pos_of(src, start),
            });
            after = match right {
                Some('-') => 2,
                Some('+') => 0,
                _ => {
                    if kind == 1 {
                        1
                    } else {
                        0
                    }
                }
            };
        }
        i = next;
        // the text after the tag: `-` strips all white space, a statement's
        // default drops the line break that follows it (trim_blocks)
        if after == 2 {
            while i < src.len() && src[i].is_whitespace() {
                i += 1;
            }
        } else if after == 1 {
            if src.get(i) == Some(&'\n') {
                i += 1;
            } else if src.get(i) == Some(&'\r') && src.get(i + 1) == Some(&'\n') {
                i += 2;
            }
        }
    }
    if !text.is_empty() {
        out.push(Tok::Text(text));
    }
    Ok(out)
}

// the parser: statements nest, every `if` and `for` closes
struct Parser<'a> {
    toks: Vec<Tok>,
    k: usize,
    path: &'a str,
}
impl Parser<'_> {
    fn fail(&self, message: impl Into<String>, at: Pos) -> RenderError {
        RenderError::new("E7001", message, at.at(), Some(self.path))
    }
    fn expr(&self, text: &str, at: Pos) -> RR<Rc<Expr>> {
        parse_expr_text(text)
            .ok_or_else(|| self.fail(format!("expression does not parse: {}", text.trim()), at))
    }
    // `for x in e if c`: the filter is the last top-level `if` whose two
    // sides both parse; an `if` inside brackets or a string is the expression's
    fn iter_and_filter(&self, text: &str, at: Pos) -> RR<(Rc<Expr>, Option<Rc<Expr>>)> {
        let cs: Vec<char> = text.chars().collect();
        let mut cands: Vec<usize> = vec![];
        let mut depth = 0i32;
        let mut quote: Option<char> = None;
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            if let Some(q) = quote {
                if c == '\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            if c == '"' || c == '\'' || c == '`' {
                quote = Some(c);
            } else if "([{".contains(c) {
                depth += 1;
            } else if ")]}".contains(c) {
                depth -= 1;
            } else if depth == 0
                && (i == 0 || cs[i - 1].is_whitespace())
                && c == 'i'
                && cs.get(i + 1) == Some(&'f')
                && cs.get(i + 2).is_some_and(|x| x.is_whitespace())
            {
                cands.push(i);
            }
            i += 1;
        }
        for i in cands.into_iter().rev() {
            let a: String = cs[..i].iter().collect();
            let b: String = cs[i + 2..].iter().collect();
            if let (Some(x), Some(y)) = (parse_expr_text(&a), parse_expr_text(&b)) {
                return Ok((x, Some(y)));
            }
        }
        Ok((self.expr(text, at)?, None))
    }
    // a body up to one of the closers: (nodes, the closer, its position, its rest)
    fn body(
        &mut self,
        closers: &[&str],
        opened: &str,
        at: Pos,
    ) -> RR<(Vec<Node>, String, Pos, String)> {
        let mut nodes: Vec<Node> = vec![];
        while self.k < self.toks.len() {
            let tok = &self.toks[self.k];
            self.k += 1;
            let (stmt, text, tat) = match tok {
                Tok::Text(s) => {
                    nodes.push(Node::Text(s.clone()));
                    continue;
                }
                Tok::Tag { stmt, text, at } => (*stmt, text.clone(), *at),
            };
            if !stmt {
                nodes.push(Node::Value {
                    expr: self.expr(&text, tat)?,
                    at: tat,
                });
                continue;
            }
            let t = text.trim();
            let word: String = t
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if word.is_empty() || !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                return Err(self.fail("empty statement", tat));
            }
            let rest = t[word.len()..].trim().to_string();
            if closers.contains(&word.as_str()) {
                return Ok((nodes, word, tat, rest));
            }
            match word.as_str() {
                "if" => {
                    let mut arms: Vec<(Option<Rc<Expr>>, Vec<Node>, Pos)> = vec![];
                    let mut cond: Option<Rc<Expr>> = Some(self.expr(&rest, tat)?);
                    let mut arm_at = tat;
                    loop {
                        let (b, closer, c_at, c_rest) =
                            self.body(&["elif", "else", "endif"], "if", arm_at)?;
                        arms.push((cond.clone(), b, arm_at));
                        if closer == "endif" {
                            if !c_rest.is_empty() {
                                return Err(self.fail("{% endif %} takes nothing", c_at));
                            }
                            break;
                        }
                        if closer == "else" {
                            if !c_rest.is_empty() {
                                return Err(self.fail("{% else %} takes nothing", c_at));
                            }
                            if cond.is_none() {
                                return Err(self.fail("{% else %} after {% else %}", c_at));
                            }
                            cond = None;
                        } else {
                            if cond.is_none() {
                                return Err(self.fail("{% elif %} after {% else %}", c_at));
                            }
                            cond = Some(self.expr(&c_rest, c_at)?);
                        }
                        arm_at = c_at;
                    }
                    nodes.push(Node::If { arms });
                }
                "for" => {
                    static RE_FOR: LazyLock<Regex> = LazyLock::new(|| {
                        Regex::new(r"(?s)^([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*([A-Za-z_][A-Za-z0-9_]*))?\s+in\s+(.+)$").unwrap()
                    });
                    let Some(m) = RE_FOR.captures(&rest) else {
                        return Err(self.fail("{% for %} expects `x in e` or `k, v in e`", tat));
                    };
                    let mut vars = vec![m[1].to_string()];
                    if let Some(v) = m.get(2) {
                        vars.push(v.as_str().to_string());
                    }
                    let (iter, filter) = self.iter_and_filter(&m[3], tat)?;
                    let (b, closer, c_at, c_rest) = self.body(&["else", "endfor"], "for", tat)?;
                    if !c_rest.is_empty() {
                        return Err(self.fail(format!("{{% {closer} %}} takes nothing"), c_at));
                    }
                    let mut empty = None;
                    if closer == "else" {
                        let (e, c2, c2_at, c2_rest) = self.body(&["endfor"], "for", c_at)?;
                        if !c2_rest.is_empty() {
                            return Err(self.fail(format!("{{% {c2} %}} takes nothing"), c2_at));
                        }
                        empty = Some(e);
                    }
                    nodes.push(Node::For {
                        vars,
                        iter,
                        filter,
                        body: b,
                        empty,
                        at: tat,
                    });
                }
                "set" => {
                    static RE_SET: LazyLock<Regex> = LazyLock::new(|| {
                        Regex::new(r"(?s)^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$").unwrap()
                    });
                    let Some(m) = RE_SET.captures(&rest) else {
                        return Err(self.fail("{% set %} expects `x = e`", tat));
                    };
                    nodes.push(Node::Set {
                        name: m[1].to_string(),
                        expr: self.expr(&m[2], tat)?,
                        at: tat,
                    });
                }
                "include" => {
                    static RE_INC: LazyLock<Regex> =
                        LazyLock::new(|| Regex::new(r#"^"((?:[^"\\]|\\.)*)"$"#).unwrap());
                    let Some(m) = RE_INC.captures(&rest) else {
                        return Err(self.fail("{% include %} expects a quoted path", tat));
                    };
                    let path = json_unquote(&format!("\"{}\"", &m[1]))
                        .map_err(|_| self.fail("{% include %} expects a quoted path", tat))?;
                    nodes.push(Node::Include { path, at: tat });
                }
                "elif" | "else" | "endif" | "endfor" | "endraw" => {
                    let opener = match word.as_str() {
                        "endfor" => "{% for %}",
                        "endraw" => "{% raw %}",
                        _ => "{% if %}",
                    };
                    return Err(self.fail(format!("{{% {word} %}} without {opener}"), tat));
                }
                _ => return Err(self.fail(format!("unknown tag {{% {word} %}}"), tat)),
            }
        }
        if !opened.is_empty() {
            return Err(self.fail(format!("unclosed {{% {opened} %}}"), at));
        }
        Ok((nodes, String::new(), at, String::new()))
    }
}

/// parse a template's text (§5.2–5.3); E7001 for what does not parse. `path`
/// is the template's path as given (its diagnostics name it); `dir` is where
/// its includes resolve, the absolute directory it was read from
pub fn parse_template(
    text: &str,
    path: &str,
    delimiters: &Delimiters,
    dir: Option<&str>,
) -> RR<Template> {
    let src: Vec<char> = text.chars().collect();
    let toks = lex(&src, path, delimiters)?;
    let mut p = Parser { toks, k: 0, path };
    let (nodes, _, _, _) = p.body(&[], "", Pos { line: 1, col: 1 })?;
    let dir = match dir {
        Some(d) => d.to_string(),
        None => parent_of(&absolute(path)),
    };
    Ok(Template {
        path: path.to_string(),
        dir,
        nodes,
    })
}

/// an absolute, normalized path (`.` and `..` resolved), as the reference's host resolves one
pub fn absolute(p: &str) -> String {
    let joined = std::path::absolute(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
    normalize_path(&joined.to_string_lossy())
}
/// `dir/rel` absolute and normalized (an absolute `rel` wins)
pub fn resolve_in(dir: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        normalize_path(rel)
    } else {
        absolute(&format!("{dir}/{rel}"))
    }
}
fn normalize_path(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if !out.is_empty() && *out.last().unwrap() != ".." {
                out.pop();
            } else if !abs {
                out.push("..");
            }
        } else {
            out.push(seg);
        }
    }
    let body = out.join("/");
    if abs {
        format!("/{body}")
    } else if body.is_empty() {
        ".".into()
    } else {
        body
    }
}
fn parent_of(p: &str) -> String {
    match p.trim_end_matches('/').rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}
fn base_of(p: &str) -> String {
    let n = p.trim_end_matches('/');
    n.rsplit('/').next().unwrap_or(n).to_string()
}

/// what a template renders over (§5.4)
pub struct Context<'a> {
    /// the engine of the run
    pub eng: Rc<Engine>,
    /// the entry module's environment: its consts and funcs are in scope
    pub menv: Rc<Env>,
    /// the root's name
    pub root_name: String,
    /// the root's document, bound to the root's name
    pub root: Value,
    /// a fan-out element and its key (§6), when rendering one element
    pub item: Option<(Value, Value)>,
    /// the text of another template file by absolute path, or None when it cannot be read
    pub read_template: &'a dyn Fn(&str) -> Option<String>,
    /// the template's delimiters
    pub delimiters: Delimiters,
}

/// the text form of a value (§5.5); Err((code, message)) when it has none
pub fn text_form(eng: &Engine, v: &Value, root_name: &str) -> Result<String, (String, String)> {
    let no = |what: &str| Err(("E7002".to_string(), format!("value has no text form{what}")));
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(fmt_f(*f)),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok("null".into()),
        Value::Absent | Value::Undef => no(": absent"),
        Value::Clo(_) | Value::Nat(_) | Value::Std(_) | Value::NsRef(_) => no(": a function"),
        Value::Q { dim, value } => {
            let unit = eng
                .env
                .base_unit_of
                .borrow()
                .get(dim)
                .cloned()
                .unwrap_or(dim.clone());
            Ok(format!("{} {unit}", fmt_f(*value)))
        }
        Value::Ref(segs) => Ok(path_str(segs, Some(root_name))),
        Value::Arr(_) | Value::Map(_) | Value::Rec(_) => Ok(eng.serialize(v, root_name, false)),
        _ => no(""),
    }
}

fn eval_err(msg: impl Into<String>) -> Fail {
    Fail::Eval(EvalErr {
        msg: msg.into(),
        code: None,
    })
}
// the `render` namespace (§5.6): json, yaml, indent
fn render_namespace(eng: Rc<Engine>, root_name: &str) -> Value {
    let raw = {
        let eng = eng.clone();
        let root = root_name.to_string();
        move |v: &Value| -> R<Value> {
            read_json(&eng.serialize(v, &root, false)).map_err(|_| eval_err("render: not data"))
        }
    };
    let raw = Rc::new(raw);
    let indent_arg = |a: &[Value], i: usize| -> R<i64> {
        match a.get(i) {
            None => Ok(-1),
            Some(Value::Int(n)) if *n >= BigInt::from(0) && *n <= BigInt::from(16) => {
                Ok(n.to_i64().unwrap())
            }
            _ => Err(eval_err("render: indent must be an integer in 0..16")),
        }
    };
    let json: NatFn = {
        let raw = raw.clone();
        Rc::new(move |a: &[Value]| -> R<Value> {
            if a.is_empty() {
                return Err(eval_err("render.json expects a value"));
            }
            let n = indent_arg(a, 1)?.max(0) as usize;
            Ok(Value::Str(to_json(&raw(&a[0])?, n)))
        })
    };
    let yaml: NatFn = {
        let raw = raw.clone();
        Rc::new(move |a: &[Value]| -> R<Value> {
            if a.is_empty() {
                return Err(eval_err("render.yaml expects a value"));
            }
            let n = indent_arg(a, 1)?;
            let n = if n < 0 { 2 } else { n as usize };
            Ok(Value::Str(to_yaml(&raw(&a[0])?, n)))
        })
    };
    let indent: NatFn = Rc::new(|a: &[Value]| -> R<Value> {
        match (a.first(), a.get(1)) {
            (Some(Value::Str(s)), Some(Value::Int(n))) if *n >= BigInt::from(0) => Ok(Value::Str(
                s.replace('\n', &format!("\n{}", " ".repeat(n.to_usize().unwrap()))),
            )),
            _ => Err(eval_err("render.indent expects a string and a count")),
        }
    });
    Value::PreObj(Rc::new(vec![
        ("json".into(), Value::Nat(json)),
        ("yaml".into(), Value::Nat(yaml)),
        ("indent".into(), Value::Nat(indent)),
    ]))
}

// the members of a record in canonical order (§7.2), as the serializer walks them
fn record_entries(eng: &Engine, inst: &Inst) -> R<Vec<(String, Value)>> {
    let names: Vec<String> = {
        let b = inst.borrow();
        let mut out: Vec<String> = vec![];
        let mut done: HashSet<String> = HashSet::new();
        for n in &b.entry_order {
            done.insert(n.clone());
            if b.extra(n).is_some() {
                continue;
            }
            let Some(s) = b.slot(n) else { continue };
            if s.hidden
                || matches!(s.state, SlotState::Invalid | SlotState::Absent)
                || s.kind == MKind::Der
            {
                continue;
            }
            out.push(n.clone());
        }
        for m in rec_members(&b.rt) {
            if done.contains(&m.name) && m.kind != MKind::Der {
                continue;
            }
            let Some(s) = b.slot(&m.name) else { continue };
            if s.hidden
                || matches!(
                    s.state,
                    SlotState::Invalid | SlotState::Absent | SlotState::Unforced
                )
            {
                continue;
            }
            out.push(m.name.clone());
        }
        out
    };
    let rec = Value::Rec(inst.clone());
    let mut out = vec![];
    for n in names {
        out.push((n.clone(), eng.access(&rec, &n)?));
    }
    Ok(out)
}
// the absent members of a record: bound to Absent so that `??` can supply a text
fn absent_members(inst: &Inst) -> Vec<String> {
    inst.borrow()
        .slots
        .iter()
        .filter(|(_, s)| s.state == SlotState::Absent)
        .map(|(n, _)| n.clone())
        .collect()
}

// the language's code for an evaluation failure inside a template
fn code_of(e: &EvalErr) -> String {
    e.code.clone().unwrap_or_else(|| {
        if e.msg.starts_with("unknown name") {
            "E3003".into()
        } else {
            "E4001".into()
        }
    })
}

/// render a parsed template over a context (§5); a RenderError carries the diagnostic
pub fn render_template(tpl: &Template, cx: &Context) -> RR<String> {
    let mut locals: HashMap<String, Value> = HashMap::new();
    locals.insert(cx.root_name.clone(), cx.root.clone());
    let members = |v: &Value, locals: &mut HashMap<String, Value>| -> RR<()> {
        if let Value::Rec(inst) = v {
            for (n, x) in record_entries(&cx.eng, inst).map_err(|f| fail_of(f, "", None))? {
                locals.insert(n, x);
            }
            for n in absent_members(inst) {
                locals.entry(n).or_insert(Value::Absent);
            }
        }
        Ok(())
    };
    if let Some((item, key)) = &cx.item {
        locals.insert("item".into(), item.clone());
        locals.insert("key".into(), key.clone());
        members(item, &mut locals)?;
    } else {
        members(&cx.root, &mut locals)?;
    }
    locals.insert(
        "render".into(),
        render_namespace(cx.eng.clone(), &cx.root_name),
    );
    let mut parsed: HashMap<String, Rc<Template>> = HashMap::new();
    let mut stack: Vec<String> = vec![resolve_in(&tpl.dir, &base_of(&tpl.path))];
    render_nodes(tpl, &tpl.nodes, locals, cx, &mut parsed, &mut stack)
}

fn fail_of(f: Fail, at: &str, file: Option<&str>) -> RenderError {
    match f {
        Fail::Eval(e) => RenderError::new(&code_of(&e), e.msg.clone(), at, file),
        _ => RenderError::new("E4001", "expression cannot be evaluated", at, file),
    }
}

fn render_nodes(
    tpl: &Template,
    nodes: &[Node],
    mut locals: HashMap<String, Value>,
    cx: &Context,
    parsed: &mut HashMap<String, Rc<Template>>,
    stack: &mut Vec<String>,
) -> RR<String> {
    let eng = &cx.eng;
    let scope = |locals: &HashMap<String, Value>| Scope {
        inst: None,
        locals: Rc::new(locals.clone()),
        root_name: cx.root_name.clone(),
        menv: Some(cx.menv.clone()),
    };
    let eval_at = |e: &Rc<Expr>, locals: &HashMap<String, Value>, p: Pos| -> RR<Value> {
        let sc = scope(locals);
        let r = (|| -> R<Value> {
            let v = eng.ev(e, &sc)?;
            let v = eng.materialize(v, &[Seg::Name("_".into())])?;
            eng.force_all(&v);
            Ok(v)
        })();
        r.map_err(|f| fail_of(f, &p.at(), Some(&tpl.path)))
    };
    let declare = |name: &str, locals: &HashMap<String, Value>, p: Pos| -> RR<()> {
        if locals.contains_key(name)
            || cx.menv.consts.borrow().contains_key(name)
            || cx.menv.funcs.borrow().contains_key(name)
        {
            return Err(RenderError::new(
                "E3019",
                format!("{name} shadows a name in scope"),
                p.at(),
                Some(&tpl.path),
            ));
        }
        Ok(())
    };
    let mut out = String::new();
    for n in nodes {
        match n {
            Node::Text(s) => out.push_str(s),
            Node::Value { expr, at } => {
                let v = eval_at(expr, &locals, *at)?;
                match text_form(eng, &v, &cx.root_name) {
                    Ok(t) => out.push_str(&t),
                    Err((code, message)) => {
                        return Err(RenderError::new(&code, message, at.at(), Some(&tpl.path)))
                    }
                }
            }
            Node::If { arms } => {
                for (cond, body, at) in arms {
                    let taken = match cond {
                        None => true,
                        Some(c) => match eval_at(c, &locals, *at)? {
                            Value::Bool(b) => b,
                            _ => {
                                return Err(RenderError::new(
                                    "E4001",
                                    "condition is not a bool",
                                    at.at(),
                                    Some(&tpl.path),
                                ))
                            }
                        },
                    };
                    if taken {
                        out.push_str(&render_nodes(tpl, body, locals.clone(), cx, parsed, stack)?);
                        break;
                    }
                }
            }
            Node::For {
                vars,
                iter,
                filter,
                body,
                empty,
                at,
            } => {
                for v in vars {
                    declare(v, &locals, *at)?;
                }
                if vars.len() == 2 && vars[0] == vars[1] {
                    return Err(RenderError::new(
                        "E3019",
                        format!("{} shadows a name in scope", vars[0]),
                        at.at(),
                        Some(&tpl.path),
                    ));
                }
                let coll = eval_at(iter, &locals, *at)?;
                let mut pairs: Vec<(Value, Value)> = if vars.len() == 1 {
                    match eng.iterate(&coll) {
                        Ok(items) => items.into_iter().map(|x| (x, Value::Undef)).collect(),
                        Err(_) => {
                            return Err(RenderError::new(
                                "E4001",
                                "for over a value that is not an array",
                                at.at(),
                                Some(&tpl.path),
                            ))
                        }
                    }
                } else {
                    match &coll {
                        Value::Rec(inst) => record_entries(eng, inst)
                            .map_err(|f| fail_of(f, &at.at(), Some(&tpl.path)))?
                            .into_iter()
                            .map(|(k, v)| (Value::Str(k), v))
                            .collect(),
                        Value::Map(m) => m
                            .borrow()
                            .entries
                            .iter()
                            .map(|(k, v)| (Value::Str(k.clone()), v.clone()))
                            .collect(),
                        _ => {
                            return Err(RenderError::new(
                                "E4001",
                                "for k, v over a value that is not an object or a map",
                                at.at(),
                                Some(&tpl.path),
                            ))
                        }
                    }
                };
                if let Some(f) = filter {
                    let mut kept = vec![];
                    for (a, b) in pairs {
                        let mut l2 = locals.clone();
                        l2.insert(vars[0].clone(), a.clone());
                        if vars.len() == 2 {
                            l2.insert(vars[1].clone(), b.clone());
                        }
                        let c = eng
                            .ev(f, &scope(&l2))
                            .map_err(|fl| fail_of(fl, &at.at(), Some(&tpl.path)))?;
                        match c {
                            Value::Bool(true) => kept.push((a, b)),
                            Value::Bool(false) => {}
                            _ => {
                                return Err(RenderError::new(
                                    "E4001",
                                    "filter is not a bool",
                                    at.at(),
                                    Some(&tpl.path),
                                ))
                            }
                        }
                    }
                    pairs = kept;
                }
                if pairs.is_empty() {
                    if let Some(e) = empty {
                        out.push_str(&render_nodes(tpl, e, locals.clone(), cx, parsed, stack)?);
                    }
                    continue;
                }
                let len = pairs.len();
                for (i, (a, b)) in pairs.into_iter().enumerate() {
                    let mut l2 = locals.clone();
                    l2.insert(vars[0].clone(), a);
                    if vars.len() == 2 {
                        l2.insert(vars[1].clone(), b);
                    }
                    l2.insert(
                        "loop".into(),
                        Value::PreObj(Rc::new(vec![
                            ("index".into(), Value::Int(BigInt::from(i + 1))),
                            ("index0".into(), Value::Int(BigInt::from(i))),
                            ("first".into(), Value::Bool(i == 0)),
                            ("last".into(), Value::Bool(i == len - 1)),
                            ("length".into(), Value::Int(BigInt::from(len))),
                        ])),
                    );
                    out.push_str(&render_nodes(tpl, body, l2, cx, parsed, stack)?);
                }
            }
            Node::Set { name, expr, at } => {
                declare(name, &locals, *at)?;
                if name == "loop" {
                    return Err(RenderError::new(
                        "E3019",
                        "loop cannot be assigned",
                        at.at(),
                        Some(&tpl.path),
                    ));
                }
                let v = eval_at(expr, &locals, *at)?;
                locals.insert(name.clone(), v);
            }
            Node::Include { path, at } => {
                let abs = resolve_in(&tpl.dir, path);
                if stack.contains(&abs) {
                    let chain: Vec<String> =
                        stack.iter().chain([&abs]).map(|p| base_of(p)).collect();
                    return Err(RenderError::new(
                        "E7001",
                        format!("include cycle: {}", chain.join(" -> ")),
                        at.at(),
                        Some(&tpl.path),
                    ));
                }
                let sub = match parsed.get(&abs) {
                    Some(t) => t.clone(),
                    None => {
                        let Some(text) = (cx.read_template)(&abs) else {
                            return Err(RenderError::new(
                                "E7003",
                                format!("template cannot be read: {path}"),
                                at.at(),
                                Some(&tpl.path),
                            ));
                        };
                        let t = Rc::new(parse_template(
                            &text,
                            path,
                            &cx.delimiters,
                            Some(&parent_of(&abs)),
                        )?);
                        parsed.insert(abs.clone(), t.clone());
                        t
                    }
                };
                stack.push(abs);
                let r = render_nodes(&sub, &sub.nodes, locals.clone(), cx, parsed, stack);
                stack.pop();
                out.push_str(&r?);
            }
        }
    }
    Ok(out)
}

// ---------------- emission: one root in its form (§3, §6) ----------------

/// the text a root is emitted as: one text, or the files of a fan-out (§6), path by path
#[derive(Debug, Clone)]
pub enum Emitted {
    /// one text
    One(String),
    /// one file per element, in element order
    Many(Vec<(String, String)>),
}

/// what emits one root: its value, its form with the invocation's overrides, and the template's text when there is one
pub struct Emission<'a> {
    /// the engine of the run
    pub eng: Rc<Engine>,
    /// the entry module's environment
    pub menv: Rc<Env>,
    /// the root's name
    pub root_name: String,
    /// the root's value
    pub value: Value,
    /// the declared form
    pub form: Form,
    /// `--format yaml` / `--format json`
    pub yaml: Option<bool>,
    /// `--indent n`
    pub indent: Option<usize>,
    /// the template's path as given, its text, and the absolute directory its includes resolve from
    pub template: Option<(String, String, String)>,
    /// the text of a template file by absolute path
    pub read_template: &'a dyn Fn(&str) -> Option<String>,
}

// a fan-out element's file path (§6): a string, relative, `/`-separated,
// not leaving the directory, distinct — else E7005 at the element's path
fn fan_out_path(
    each: &str,
    elem: &Value,
    key: &Value,
    at: &str,
    seen: &mut HashSet<String>,
) -> RR<String> {
    let e7005 = |m: String| RenderError::new("E7005", m, at, None);
    let p: Value = if each == "$key" {
        match key {
            Value::Str(_) => key.clone(),
            _ => {
                return Err(e7005(
                    "fan-out path: $key names no key (the root is an array)".into(),
                ))
            }
        }
    } else {
        let Value::Rec(inst) = elem else {
            return Err(e7005(format!(
                "fan-out path: the element has no member {each}"
            )));
        };
        let b = inst.borrow();
        let Some(s) = b.slot(each) else {
            return Err(e7005(format!(
                "fan-out path: the element has no member {each}"
            )));
        };
        if s.state == SlotState::Absent {
            Value::Absent
        } else {
            s.value.clone()
        }
    };
    let Value::Str(p) = p else {
        return Err(e7005("fan-out path is not a string".into()));
    };
    if p.is_empty() {
        return Err(e7005("fan-out path is empty".into()));
    }
    if p.starts_with('/') {
        return Err(e7005(format!("fan-out path is absolute: {p}")));
    }
    if p.contains('\\') {
        return Err(e7005(format!("fan-out path uses \\: {p}")));
    }
    if p.split('/').any(|s| s == ".." || s == "." || s.is_empty()) {
        return Err(e7005(format!(
            "fan-out path leaves the destination directory: {p}"
        )));
    }
    if seen.contains(&p) {
        return Err(e7005(format!("fan-out path repeats: {p}")));
    }
    seen.insert(p.clone());
    Ok(p)
}

/// emit one root (§3.1): its structured text or its template's text, as one text or one file per element
pub fn emit_root(e: &Emission) -> RR<Emitted> {
    let yaml = e.yaml.unwrap_or(e.form.yaml);
    let indent = e.indent.or(e.form.indent);
    let raw = |v: &Value| -> Value {
        read_json(&e.eng.serialize(v, &e.root_name, false))
            .ok()
            .expect("canonical JSON")
    };
    let delimiters = e.form.delimiters.clone().unwrap_or_default();
    let tpl = match &e.template {
        Some((path, text, dir)) => Some(parse_template(text, path, &delimiters, Some(dir))?),
        None => None,
    };
    let cx = |item: Option<(Value, Value)>| Context {
        eng: e.eng.clone(),
        menv: e.menv.clone(),
        root_name: e.root_name.clone(),
        root: e.value.clone(),
        item,
        read_template: e.read_template,
        delimiters: delimiters.clone(),
    };
    let Some(each) = &e.form.each else {
        return Ok(Emitted::One(match &tpl {
            Some(t) => render_template(t, &cx(None))?,
            None => layout(&raw(&e.value), yaml, indent),
        }));
    };
    // fan-out: every element of the array or map to its own file
    let elems: Vec<(Value, Value, Seg)> = match &e.value {
        Value::Arr(a) => a
            .borrow()
            .items
            .iter()
            .enumerate()
            .map(|(i, v)| (v.clone(), Value::Int(BigInt::from(i)), Seg::Idx(i)))
            .collect(),
        Value::Map(m) => m
            .borrow()
            .entries
            .iter()
            .map(|(k, v)| (v.clone(), Value::Str(k.clone()), Seg::Key(k.clone())))
            .collect(),
        _ => {
            return Err(RenderError::new(
                "E7004",
                "@render: each on a root that is neither an array nor a map",
                e.root_name.clone(),
                None,
            ))
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut paths = vec![];
    for (v, k, seg) in &elems {
        let at = path_str(&[Seg::Name(e.root_name.clone()), seg.clone()], None);
        paths.push(fan_out_path(each, v, k, &at, &mut seen)?);
    }
    let mut files = vec![];
    for (i, (v, k, _)) in elems.into_iter().enumerate() {
        let text = match &tpl {
            Some(t) => render_template(t, &cx(Some((v, k))))?,
            None => layout(&raw(&v), yaml, indent),
        };
        files.push((paths[i].clone(), text));
    }
    Ok(Emitted::Many(files))
}
