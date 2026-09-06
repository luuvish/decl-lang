//! YAML for documents (docs/tooling/05_render.md §2, §4): a reader of
//! YAML 1.2 with the core schema into the JSON data model — the values
//! `read_json` produces, so that a document written in YAML is
//! indistinguishable from the same document written in JSON from the
//! reader on — and a writer of the block-style form that every YAML 1.2
//! reader (and no YAML 1.1 reader) reads back as the canonical JSON
//! document. Beside them, the JSON layouts of §4.1. The reader accepts
//! exactly what the document says and refuses the rest with the reason
//! and the line, so that the three implementations refuse the same texts
//! with the same words. A port of the reference's yaml.ts.
use crate::engine::fmt_f;
use crate::semantics::{json_str, Value};
use num_bigint::BigInt;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

/// a document the reader refuses: `<reason> at line L`
#[derive(Debug, Clone, PartialEq)]
pub struct YamlError {
    /// what was refused
    pub reason: String,
    /// the line it was found on (from 1)
    pub line: usize,
}
impl YamlError {
    /// `<reason> at line L`
    pub fn message(&self) -> String {
        format!("{} at line {}", self.reason, self.line)
    }
}
impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}
type YR<T> = Result<T, YamlError>;

/// a document path names YAML by its extension (§2); anything else is JSON
pub fn is_yaml_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    lower.ends_with(".yaml") || lower.ends_with(".yml")
}

// ---------------- the core schema (§2): what a plain scalar means ----------------
// null, bool, int (decimal, octal, hexadecimal), float — everything else
// is a string. YAML 1.1's spellings (yes/no/on/off, sexagesimals,
// timestamps, `1_000`) are strings.
static RE_INT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?[0-9]+$").unwrap());
static RE_OCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0o[0-7]+$").unwrap());
static RE_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0x[0-9a-fA-F]+$").unwrap());
static RE_FLOAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$").unwrap());
static RE_NONFINITE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([-+]?\.(inf|Inf|INF)|\.(nan|NaN|NAN))$").unwrap());

/// what a plain scalar resolves to under the core schema
#[derive(Debug, Clone, PartialEq)]
pub enum Plain {
    /// `null`, `~`, empty
    Null,
    /// a bool
    Bool(bool),
    /// an integer
    Int(BigInt),
    /// a float
    Float(f64),
    /// `.inf` / `.nan`: refused
    NonFinite,
    /// everything else
    Str(String),
}
/// the core schema's reading of a plain scalar
pub fn resolve_plain(s: &str) -> Plain {
    if s.is_empty() || s == "~" || s == "null" || s == "Null" || s == "NULL" {
        return Plain::Null;
    }
    if s == "true" || s == "True" || s == "TRUE" {
        return Plain::Bool(true);
    }
    if s == "false" || s == "False" || s == "FALSE" {
        return Plain::Bool(false);
    }
    if RE_INT.is_match(s) {
        let digits = s.strip_prefix('+').unwrap_or(s);
        return Plain::Int(digits.parse::<BigInt>().expect("an integer"));
    }
    if RE_OCT.is_match(s) {
        return Plain::Int(BigInt::parse_bytes(&s.as_bytes()[2..], 8).expect("octal"));
    }
    if RE_HEX.is_match(s) {
        return Plain::Int(BigInt::parse_bytes(&s.as_bytes()[2..], 16).expect("hex"));
    }
    if RE_FLOAT.is_match(s) {
        return Plain::Float(parse_float(s));
    }
    if RE_NONFINITE.is_match(s) {
        return Plain::NonFinite;
    }
    Plain::Str(s.to_string())
}
// the core schema's float forms (`1.`, `.5`, `+1e3`) are all Rust's
fn parse_float(s: &str) -> f64 {
    s.parse::<f64>().expect("a float")
}
fn plain_value(p: Plain) -> Value {
    match p {
        Plain::Null => Value::Null,
        Plain::Bool(b) => Value::Bool(b),
        Plain::Int(i) => Value::Int(i),
        Plain::Float(f) => Value::Float(f),
        Plain::Str(s) => Value::Str(s),
        Plain::NonFinite => Value::Null,
    }
}

// ---------------- the reader ----------------
const fn is_space(c: Option<char>) -> bool {
    matches!(c, Some(' ') | Some('\t'))
}
const fn is_break_or_end(c: Option<char>) -> bool {
    matches!(c, None | Some('\n'))
}
fn is_flow_end(c: Option<char>) -> bool {
    matches!(c, Some(',') | Some('[') | Some(']') | Some('{') | Some('}'))
}
#[derive(Clone, Copy, PartialEq)]
enum Where {
    Seq,
    Map,
    None,
}

struct Reader {
    s: Vec<char>,
    i: usize,
    anchors: HashMap<String, Value>,
}

impl Reader {
    fn new(src: &str) -> Reader {
        let text = src.strip_prefix('\u{feff}').unwrap_or(src);
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        Reader {
            s: text.chars().collect(),
            i: 0,
            anchors: HashMap::new(),
        }
    }
    // ---- positions ----
    fn line(&self, at: usize) -> usize {
        1 + self.s[..at.min(self.s.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count()
    }
    fn err(&self, reason: &str, at: usize) -> YamlError {
        YamlError {
            reason: reason.to_string(),
            line: self.line(at),
        }
    }
    fn fail<T>(&self, reason: &str) -> YR<T> {
        Err(self.err(reason, self.i))
    }
    fn fail_at<T>(&self, reason: &str, at: usize) -> YR<T> {
        Err(self.err(reason, at))
    }
    fn at(&self, k: usize) -> Option<char> {
        self.s.get(k).copied()
    }
    fn peek(&self) -> Option<char> {
        self.at(self.i)
    }
    fn peek1(&self) -> Option<char> {
        self.at(self.i + 1)
    }
    fn at_end_of(&self, k: usize) -> bool {
        is_break_or_end(self.at(k))
    }
    fn at_end(&self) -> bool {
        self.at_end_of(self.i)
    }
    fn line_start(&self, mut k: usize) -> usize {
        while k > 0 && self.s[k - 1] != '\n' {
            k -= 1;
        }
        k
    }
    fn col(&self) -> usize {
        self.i - self.line_start(self.i)
    }
    /// the indentation of the line holding k: its leading spaces (a tab there is refused)
    fn indent_of(&self, k: usize) -> YR<usize> {
        let mut j = self.line_start(k);
        let mut n = 0;
        while self.at(j) == Some(' ') {
            j += 1;
            n += 1;
        }
        if self.at(j) == Some('\t') {
            return self.fail_at("tab in indentation", j);
        }
        Ok(n)
    }
    /// skip spaces and a comment on the current line; stays before its break
    fn skip_inline(&mut self) {
        while is_space(self.peek()) {
            self.i += 1;
        }
        if self.peek() == Some('#') {
            while !self.at_end() {
                self.i += 1;
            }
        }
    }
    /// the rest of the current line must be empty (a comment allowed)
    fn end_line(&mut self, what: &str) -> YR<()> {
        self.skip_inline();
        if !self.at_end() {
            return self.fail(&format!("unexpected content after {what}"));
        }
        Ok(())
    }
    /// advance over line breaks, blank lines, and comment lines to the next
    /// content character, and return its column; None at the end. Idempotent:
    /// at a content character with only indentation before it, stays.
    fn next_content(&mut self) -> YR<Option<usize>> {
        loop {
            match self.peek() {
                None => return Ok(None),
                Some('\n') => {
                    self.i += 1;
                    continue;
                }
                _ => {}
            }
            let start = self.line_start(self.i);
            let mut j = start;
            while self.at(j) == Some(' ') {
                j += 1;
            }
            if j < self.i {
                // mid-line: the caller left content behind — it belongs to no node
                return self.fail("unexpected content");
            }
            if self.at(j) == Some('\t') {
                return self.fail_at("tab in indentation", j);
            }
            self.i = j;
            match self.peek() {
                None => return Ok(None),
                Some('\n') => continue,
                Some('#') => {
                    while !self.at_end() {
                        self.i += 1;
                    }
                    continue;
                }
                _ => return Ok(Some(j - start)),
            }
        }
    }
    /// is a `-`, `?`, or `:` at k an indicator (followed by a space or the line's end)?
    fn indicator_at(&self, k: usize) -> bool {
        is_space(self.at(k + 1)) || is_break_or_end(self.at(k + 1))
    }
    /// a document marker (`---` or `...`) at column 0 ends every node
    fn at_marker(&self) -> bool {
        self.line_is("---") || self.line_is("...")
    }
    fn line_is(&self, text: &str) -> bool {
        let n = text.chars().count();
        self.col() == 0
            && self.i + n <= self.s.len()
            && self.s[self.i..self.i + n].iter().collect::<String>() == text
            && self.indicator_at(self.i + n - 1)
    }
    fn dash_here(&self) -> bool {
        self.peek() == Some('-') && self.indicator_at(self.i)
    }

    // ---- the stream ----
    fn document(&mut self) -> YR<Value> {
        // directives and the start marker
        loop {
            if self.peek() == Some('%') && self.col() == 0 {
                let mut end = self.i;
                while !self.at_end_of(end) {
                    end += 1;
                }
                let text: String = self.s[self.i..end].iter().collect();
                static RE_YAML12: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"^%YAML[ \t]+1\.2[ \t]*(#.*)?$").unwrap());
                if RE_YAML12.is_match(&text) {
                    self.i = end;
                    continue;
                }
                if text.starts_with("%TAG") {
                    return self.fail("uses a tag");
                }
                if text.starts_with("%YAML") {
                    return self.fail("unsupported YAML version");
                }
                let word = text.split([' ', '\t']).next().unwrap_or("");
                return self.fail(&format!("unsupported directive {word}"));
            }
            let Some(n) = self.next_content()? else {
                return Ok(Value::Null);
            };
            if self.peek() == Some('%') && n == 0 {
                continue;
            }
            break;
        }
        let mut value = None;
        if self.line_is("---") {
            self.i += 3;
            self.skip_inline();
            if !self.at_end() {
                value = Some(self.node(-1, Where::Seq, -1)?);
            }
        } else if self.line_is("...") {
            return self.fail("unexpected end marker");
        }
        let value = match value {
            Some(v) => v,
            None => self.node(-1, Where::None, -1)?,
        };
        // the tail: blank lines and comments, an end marker, nothing else
        let mut ended = false;
        loop {
            if self.next_content()?.is_none() {
                return Ok(value);
            }
            if self.line_is("...") {
                self.i += 3;
                self.end_line("the end marker")?;
                ended = true;
                continue;
            }
            if self.line_is("---") {
                return self.fail("stream holds more than one document");
            }
            return self.fail(if ended {
                "unexpected content after the end marker"
            } else {
                "unexpected content"
            });
        }
    }

    // ---- block nodes ----
    /// the node whose text starts at the cursor (inline: on the line of the
    /// `- ` or `key: ` it follows) or on the following lines (indented more
    /// than `parent`; a sequence may sit at `seq_at` = the parent mapping's
    /// own indentation). `where_` says what the inline position follows.
    /// Null for an empty node, leaving the cursor where it was.
    fn node(&mut self, parent: i64, where_: Where, seq_at: i64) -> YR<Value> {
        self.skip_inline();
        if !self.at_end() {
            let c = self.col() as i64;
            return self.node_at(c, where_, parent);
        }
        let save = self.i;
        let Some(n) = self.next_content()? else {
            return Ok(Value::Null);
        };
        let n = n as i64;
        let dash = self.dash_here();
        if (n <= parent && !(dash && seq_at == n)) || self.at_marker() {
            self.i = save;
            return Ok(Value::Null);
        }
        self.node_at(n, Where::None, parent)
    }
    fn node_at(&mut self, ind: i64, where_: Where, parent: i64) -> YR<Value> {
        let c = self.peek();
        match c {
            Some('&') => {
                let name = self.anchor_name()?;
                self.skip_inline();
                let v = if self.at_end() {
                    self.node(parent, where_, -1)?
                } else {
                    let col = self.col() as i64;
                    self.node_at(col, where_, parent)?
                };
                self.anchors.insert(name, v.clone());
                Ok(v)
            }
            Some('*') => {
                let at = self.i;
                let name = self.anchor_name()?;
                let Some(v) = self.anchors.get(&name).cloned() else {
                    return self.fail_at(&format!("unknown alias *{name}"), at);
                };
                self.end_line("an alias")?;
                Ok(copy_of(&v))
            }
            Some('!') => self.fail("uses a tag"),
            Some('@') | Some('`') => self.fail(&format!("reserved indicator {}", c.unwrap())),
            Some('%') => self.fail("unexpected directive"),
            Some('[') | Some('{') => {
                let v = self.flow_node()?;
                self.end_line("a flow collection")?;
                Ok(v)
            }
            Some('|') | Some('>') => self.block_scalar(parent),
            Some('"') | Some('\'') => {
                let start = self.i;
                let text = self.quoted()?;
                while is_space(self.peek()) {
                    self.i += 1;
                }
                if self.peek() == Some(':') && self.indicator_at(self.i) {
                    if where_ == Where::Map {
                        return self.fail("unexpected mapping value");
                    }
                    self.i = start;
                    return self.mapping(ind);
                }
                self.end_line("a scalar")?;
                Ok(Value::Str(text))
            }
            Some('-') if self.indicator_at(self.i) => {
                if where_ == Where::Map {
                    return self.fail("unexpected sequence");
                }
                self.sequence(ind)
            }
            Some('?') if self.indicator_at(self.i) => self.fail("mapping key is not a string"),
            Some(':') if self.indicator_at(self.i) => self.fail("unexpected ':'"),
            _ => {
                // a plain scalar — a mapping when `: ` follows it on the line
                if self.plain_is_key() {
                    if where_ == Where::Map {
                        return self.fail("unexpected mapping value");
                    }
                    return self.mapping(ind);
                }
                self.plain_scalar(parent)
            }
        }
    }
    fn anchor_name(&mut self) -> YR<String> {
        self.i += 1; // & or *
        let start = self.i;
        while !self.at_end() && !is_space(self.peek()) && !is_flow_end(self.peek()) {
            self.i += 1;
        }
        if self.i == start {
            return self.fail_at("missing anchor name", start - 1);
        }
        Ok(self.s[start..self.i].iter().collect())
    }
    /// does the plain text at the cursor end in a `: ` on this line (before any comment)?
    fn plain_is_key(&self) -> bool {
        let mut k = self.i;
        loop {
            let c = self.at(k);
            if is_break_or_end(c) {
                return false;
            }
            if c == Some('#') && k > self.i && is_space(self.at(k - 1)) {
                return false;
            }
            if c == Some(':') && self.indicator_at(k) {
                return true;
            }
            k += 1;
        }
    }
    /// the plain text on the current line up to a comment or the line's end (not a key)
    fn plain_line(&mut self) -> YR<String> {
        let start = self.i;
        let mut end = self.i;
        loop {
            let c = self.at(end);
            if is_break_or_end(c) {
                break;
            }
            if c == Some('#') && end > start && is_space(self.at(end - 1)) {
                break;
            }
            if c == Some(':') && self.indicator_at(end) {
                return self.fail_at("unexpected ':'", end);
            }
            end += 1;
        }
        self.i = end;
        let text: String = self.s[start..end].iter().collect();
        let text = text.trim_end_matches([' ', '\t']).to_string();
        self.skip_inline();
        Ok(text)
    }
    fn plain_scalar(&mut self, parent: i64) -> YR<Value> {
        let at = self.i;
        let mut text = self.plain_line()?;
        // continuation lines: indented more than the parent, folded with a
        // space; blank lines between fold to line breaks
        loop {
            let save = self.i;
            let mut blanks = 0;
            let mut k = self.i;
            if self.at(k) != Some('\n') {
                break;
            }
            let mut found: Option<usize> = None;
            while self.at(k) == Some('\n') {
                k += 1;
                let mut j = k;
                while matches!(self.at(j), Some(' ') | Some('\t')) {
                    j += 1;
                }
                if self.at(j) == Some('\n') {
                    blanks += 1;
                    k = j;
                    continue;
                }
                if self.at(j).is_none() {
                    break;
                }
                found = Some(j);
                break;
            }
            let Some(found) = found else { break };
            let ind = self.indent_of(found)? as i64;
            let c = self.at(found);
            if ind <= parent || c == Some('#') {
                break;
            }
            if matches!(c, Some('-') | Some('?') | Some(':')) && self.indicator_at(found) {
                break;
            }
            self.i = found;
            if self.at_marker() || self.plain_is_key() {
                self.i = save;
                break;
            }
            let more = self.plain_line()?;
            if blanks > 0 {
                text.push_str(&"\n".repeat(blanks));
            } else {
                text.push(' ');
            }
            text.push_str(&more);
            if self.i == save {
                break;
            }
        }
        let r = resolve_plain(&text);
        if r == Plain::NonFinite {
            return self.fail_at("non-finite float", at);
        }
        Ok(plain_value(r))
    }
    fn mapping(&mut self, ind: i64) -> YR<Value> {
        let mut entries: Vec<(String, Value)> = vec![];
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            let at = self.i;
            let key = self.key()?;
            if seen.contains(&key) {
                return self.fail_at(&format!("mapping repeats the key {}", json_str(&key)), at);
            }
            seen.insert(key.clone());
            let value = self.node(ind, Where::Map, ind)?;
            entries.push((key, value));
            let Some(n) = self.next_content()? else { break };
            let n = n as i64;
            if n < ind || self.at_marker() {
                break;
            }
            if n > ind {
                return self.fail("bad indentation");
            }
            if self.dash_here() {
                return self.fail("unexpected sequence");
            }
        }
        Ok(Value::JObj(Rc::new(entries)))
    }
    /// a mapping key at the cursor, and the `:` after it
    fn key(&mut self) -> YR<String> {
        let c = self.peek();
        let key: String = match c {
            Some('"') | Some('\'') => self.quoted()?,
            Some('?') if self.indicator_at(self.i) => {
                return self.fail("mapping key is not a string")
            }
            Some('&') | Some('*') => return self.fail("mapping key is not a string"),
            Some('!') => return self.fail("uses a tag"),
            Some('[') | Some('{') => return self.fail("mapping key is not a string"),
            _ => {
                let start = self.i;
                let mut end = self.i;
                loop {
                    let ch = self.at(end);
                    if is_break_or_end(ch) {
                        return self.fail_at("missing ':' after a mapping key", start);
                    }
                    if ch == Some('#') && end > start && is_space(self.at(end - 1)) {
                        return self.fail_at("missing ':' after a mapping key", start);
                    }
                    if ch == Some(':') && self.indicator_at(end) {
                        break;
                    }
                    end += 1;
                }
                let text: String = self.s[start..end].iter().collect();
                let text = text.trim_end_matches([' ', '\t']).to_string();
                if !matches!(resolve_plain(&text), Plain::Str(_)) {
                    return self.fail_at("mapping key is not a string", start);
                }
                self.i = end;
                text
            }
        };
        while is_space(self.peek()) {
            self.i += 1;
        }
        if !(self.peek() == Some(':') && self.indicator_at(self.i)) {
            return self.fail("missing ':' after a mapping key");
        }
        self.i += 1;
        Ok(key)
    }
    fn sequence(&mut self, ind: i64) -> YR<Value> {
        let mut items = vec![];
        loop {
            self.i += 1; // the dash
            items.push(self.node(ind, Where::Seq, -1)?);
            let Some(n) = self.next_content()? else { break };
            let n = n as i64;
            if n < ind || self.at_marker() {
                break;
            }
            if n > ind {
                return self.fail("bad indentation");
            }
            if !self.dash_here() {
                break;
            }
        }
        Ok(Value::JArr(Rc::new(items)))
    }

    // ---- scalars ----
    /// a single- or double-quoted scalar at the cursor, folded over lines
    fn quoted(&mut self) -> YR<String> {
        let q = self.peek().unwrap();
        let at = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.fail_at("unterminated quoted scalar", at);
            };
            if c == q {
                if q == '\'' && self.peek1() == Some('\'') {
                    out.push('\'');
                    self.i += 2;
                    continue;
                }
                self.i += 1;
                return Ok(out);
            }
            if c == '\n' {
                // folding: one break is a space, further breaks are kept
                let mut breaks = 0;
                while self.peek() == Some('\n') || is_space(self.peek()) {
                    if self.peek() == Some('\n') {
                        breaks += 1;
                    }
                    self.i += 1;
                }
                let trimmed = out.trim_end_matches([' ', '\t']).len();
                out.truncate(trimmed);
                if breaks > 1 {
                    out.push_str(&"\n".repeat(breaks - 1));
                } else {
                    out.push(' ');
                }
                continue;
            }
            if q == '"' && c == '\\' {
                self.i += 1;
                let e = self.escape()?;
                out.push_str(&e);
                continue;
            }
            out.push(c);
            self.i += 1;
        }
    }
    fn escape(&mut self) -> YR<String> {
        let c = self.peek();
        let at = self.i - 1;
        self.i += 1;
        Ok(match c {
            Some('0') => "\0".into(),
            Some('a') => "\x07".into(),
            Some('b') => "\x08".into(),
            Some('t') | Some('\t') => "\t".into(),
            Some('n') => "\n".into(),
            Some('v') => "\x0b".into(),
            Some('f') => "\x0c".into(),
            Some('r') => "\r".into(),
            Some('e') => "\x1b".into(),
            Some(' ') => " ".into(),
            Some('"') => "\"".into(),
            Some('/') => "/".into(),
            Some('\\') => "\\".into(),
            Some('N') => "\u{85}".into(),
            Some('_') => "\u{a0}".into(),
            Some('L') => "\u{2028}".into(),
            Some('P') => "\u{2029}".into(),
            Some('x') | Some('u') | Some('U') => {
                let len = match c {
                    Some('x') => 2,
                    Some('u') => 4,
                    _ => 8,
                };
                let hex: String = self.s[self.i..(self.i + len).min(self.s.len())]
                    .iter()
                    .collect();
                if hex.chars().count() != len || !hex.chars().all(|h| h.is_ascii_hexdigit()) {
                    return self.fail_at("bad escape", at);
                }
                self.i += len;
                let cp = u32::from_str_radix(&hex, 16).unwrap();
                match char::from_u32(cp) {
                    Some(ch) => ch.to_string(),
                    // a lone surrogate: the reference keeps it as a code unit; no
                    // document of the corpus carries one
                    None => return self.fail_at("bad escape", at),
                }
            }
            Some('\n') => {
                // an escaped line break joins the lines; leading white space is dropped
                while is_space(self.peek()) {
                    self.i += 1;
                }
                String::new()
            }
            _ => return self.fail_at("bad escape", at),
        })
    }
    /// a block scalar (`|` or `>`) with its indicators; `parent` is the enclosing indentation
    fn block_scalar(&mut self, parent: i64) -> YR<Value> {
        let at = self.i;
        let folded = self.peek() == Some('>');
        self.i += 1;
        let mut chomp = 0i8; // 0 clip, -1 strip, 1 keep
        let mut chomp_set = false;
        let mut explicit = 0usize;
        for _ in 0..2 {
            match self.peek() {
                Some('-') | Some('+') => {
                    if chomp_set {
                        return self.fail_at("bad block scalar header", at);
                    }
                    chomp = if self.peek() == Some('-') { -1 } else { 1 };
                    chomp_set = true;
                    self.i += 1;
                }
                Some(d) if ('1'..='9').contains(&d) => {
                    if explicit != 0 {
                        return self.fail_at("bad block scalar header", at);
                    }
                    explicit = d as usize - '0' as usize;
                    self.i += 1;
                }
                _ => {}
            }
        }
        self.end_line("a block scalar header")?;
        // the content lines: those indented at least the content indentation
        // (explicit, or the first non-blank line's), until a lesser one
        let mut lines: Vec<(String, bool)> = vec![];
        let mut indent: i64 = if explicit > 0 {
            parent.max(0) + explicit as i64
        } else {
            -1
        };
        let mut k = self.i;
        let mut end_at = self.i;
        while self.at(k) == Some('\n') {
            let start = k + 1;
            let mut j = start;
            while self.at(j) == Some(' ') {
                j += 1;
            }
            let blank = matches!(self.at(j), Some('\n') | None);
            let line_indent = (j - start) as i64;
            if blank {
                let mut e = j;
                while !matches!(self.at(e), Some('\n') | None) {
                    e += 1;
                }
                let text = if indent >= 0 && line_indent > indent {
                    " ".repeat((line_indent - indent) as usize)
                } else {
                    String::new()
                };
                lines.push((text, true));
                k = e;
                end_at = e;
                if self.at(e).is_none() {
                    break;
                }
                continue;
            }
            if indent < 0 {
                if line_indent <= parent {
                    break;
                }
                indent = line_indent;
                // blank lines before the first content line carry no spaces
                for l in lines.iter_mut() {
                    l.0.clear();
                }
            }
            if line_indent < indent {
                break;
            }
            if self.at(j) == Some('\t') && line_indent == indent {
                return self.fail_at("tab in indentation", j);
            }
            let mut e = j;
            while !matches!(self.at(e), Some('\n') | None) {
                e += 1;
            }
            let text: String = self.s[start + indent as usize..e].iter().collect();
            lines.push((text, false));
            k = e;
            end_at = e;
            if self.at(e).is_none() {
                break;
            }
        }
        self.i = end_at;
        // trailing blank lines are the chomping's business
        let mut last = lines.len();
        while last > 0 && lines[last - 1].1 {
            last -= 1;
        }
        let body = &lines[..last];
        let trailing = lines.len() - last;
        let mut text = String::new();
        if !folded {
            text = body
                .iter()
                .map(|l| l.0.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        } else {
            // folding: a break between two normal lines is a space, blank lines
            // are kept as breaks, more-indented lines are kept as written
            let more_indented =
                |x: &(String, bool)| !x.1 && (x.0.starts_with(' ') || x.0.starts_with('\t'));
            for (n, l) in body.iter().enumerate() {
                if n == 0 {
                    text = l.0.clone();
                    continue;
                }
                let prev = &body[n - 1];
                if l.1 || prev.1 || more_indented(prev) || more_indented(l) {
                    text.push('\n');
                } else {
                    text.push(' ');
                }
                text.push_str(&l.0);
            }
        }
        if body.is_empty() {
            return Ok(Value::Str(if chomp > 0 {
                "\n".repeat(trailing)
            } else {
                String::new()
            }));
        }
        Ok(Value::Str(match chomp {
            -1 => text,
            0 => text + "\n",
            _ => text + &"\n".repeat(trailing + 1),
        }))
    }

    // ---- flow nodes ----
    fn flow_ws(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') | Some('\n') => self.i += 1,
                Some('#')
                    if self.i == 0
                        || is_space(self.at(self.i - 1))
                        || self.at(self.i - 1) == Some('\n') =>
                {
                    while !self.at_end() {
                        self.i += 1;
                    }
                }
                _ => return,
            }
        }
    }
    fn flow_node(&mut self) -> YR<Value> {
        self.flow_ws();
        let Some(c) = self.peek() else {
            return self.fail("unterminated flow collection");
        };
        match c {
            '&' => {
                let name = self.anchor_name()?;
                let v = self.flow_node()?;
                self.anchors.insert(name, v.clone());
                Ok(v)
            }
            '*' => {
                let at = self.i;
                let name = self.anchor_name()?;
                match self.anchors.get(&name).cloned() {
                    Some(v) => Ok(copy_of(&v)),
                    None => self.fail_at(&format!("unknown alias *{name}"), at),
                }
            }
            '!' => self.fail("uses a tag"),
            '[' => {
                let at = self.i;
                self.i += 1;
                let mut items = vec![];
                loop {
                    self.flow_ws();
                    match self.peek() {
                        Some(']') => {
                            self.i += 1;
                            return Ok(Value::JArr(Rc::new(items)));
                        }
                        None => return self.fail_at("unterminated flow collection", at),
                        Some(',') => return self.fail("unexpected ','"),
                        _ => {}
                    }
                    items.push(self.flow_node()?);
                    self.flow_ws();
                    match self.peek() {
                        Some(':') => return self.fail("unexpected ':'"),
                        Some(',') => {
                            self.i += 1;
                            continue;
                        }
                        Some(']') => continue,
                        _ => return self.fail("expected ',' or ']'"),
                    }
                }
            }
            '{' => {
                let at = self.i;
                self.i += 1;
                let mut entries: Vec<(String, Value)> = vec![];
                let mut seen: HashSet<String> = HashSet::new();
                loop {
                    self.flow_ws();
                    match self.peek() {
                        Some('}') => {
                            self.i += 1;
                            return Ok(Value::JObj(Rc::new(entries)));
                        }
                        None => return self.fail_at("unterminated flow collection", at),
                        Some(',') => return self.fail("unexpected ','"),
                        _ => {}
                    }
                    let key_at = self.i;
                    let Value::Str(key) = self.flow_node()? else {
                        return self.fail_at("mapping key is not a string", key_at);
                    };
                    if seen.contains(&key) {
                        return self.fail_at(
                            &format!("mapping repeats the key {}", json_str(&key)),
                            key_at,
                        );
                    }
                    seen.insert(key.clone());
                    self.flow_ws();
                    let mut value = Value::Null;
                    if self.peek() == Some(':') {
                        self.i += 1;
                        self.flow_ws();
                        if !matches!(self.peek(), Some(',') | Some('}')) {
                            value = self.flow_node()?;
                        }
                        self.flow_ws();
                    }
                    entries.push((key, value));
                    match self.peek() {
                        Some(',') => {
                            self.i += 1;
                            continue;
                        }
                        Some('}') => continue,
                        _ => return self.fail("expected ',' or '}'"),
                    }
                }
            }
            '"' | '\'' => Ok(Value::Str(self.quoted()?)),
            ']' | '}' => self.fail(&format!("unexpected '{c}'")),
            _ => {
                // a plain scalar in flow context: ends at an indicator, folded over lines
                let at = self.i;
                let mut text = String::new();
                loop {
                    let start = self.i;
                    let mut end = self.i;
                    loop {
                        let ch = self.at(end);
                        if is_break_or_end(ch) || is_flow_end(ch) {
                            break;
                        }
                        if ch == Some('#') && end > start && is_space(self.at(end - 1)) {
                            break;
                        }
                        if ch == Some(':')
                            && (is_space(self.at(end + 1))
                                || is_break_or_end(self.at(end + 1))
                                || is_flow_end(self.at(end + 1)))
                        {
                            break;
                        }
                        end += 1;
                    }
                    let piece: String = self.s[start..end].iter().collect();
                    text.push_str(piece.trim_end_matches([' ', '\t']));
                    self.i = end;
                    // a line break inside the scalar folds to a space
                    let mut k = end;
                    while is_space(self.at(k)) {
                        k += 1;
                    }
                    if self.at(k) == Some('#') {
                        break;
                    }
                    if self.at(k) != Some('\n') {
                        break;
                    }
                    let mut blanks = 0;
                    while self.at(k) == Some('\n') || is_space(self.at(k)) {
                        if self.at(k) == Some('\n') {
                            blanks += 1;
                        }
                        k += 1;
                    }
                    let ch = self.at(k);
                    if ch.is_none()
                        || is_flow_end(ch)
                        || ch == Some('#')
                        || (ch == Some(':') && self.indicator_at(k))
                    {
                        self.i = k;
                        break;
                    }
                    if blanks > 1 {
                        text.push_str(&"\n".repeat(blanks - 1));
                    } else {
                        text.push(' ');
                    }
                    self.i = k;
                }
                if text.is_empty() {
                    return self.fail_at("unexpected content", at);
                }
                let r = resolve_plain(&text);
                if r == Plain::NonFinite {
                    return self.fail_at("non-finite float", at);
                }
                Ok(plain_value(r))
            }
        }
    }
}

// an alias is a copy of the anchored value
fn copy_of(v: &Value) -> Value {
    match v {
        Value::JArr(items) => Value::JArr(Rc::new(items.iter().map(copy_of).collect())),
        Value::JObj(entries) => Value::JObj(Rc::new(
            entries
                .iter()
                .map(|(k, x)| (k.clone(), copy_of(x)))
                .collect(),
        )),
        other => other.clone(),
    }
}

/// read one YAML document (§2) into the JSON data model
pub fn read_yaml(src: &str) -> Result<Value, YamlError> {
    Reader::new(src).document()
}

// ---------------- the writer (§4.2) ----------------
// the YAML 1.1 spellings a 1.1 reader would take for a bool or a null
const YAML11_WORDS: [&str; 16] = [
    "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "on", "On", "ON", "off", "Off",
    "OFF",
];
/// plain only when a YAML 1.2 reader reads it back as exactly this string
/// and a YAML 1.1 reader has nothing to reinterpret: it starts with a
/// letter or `_`, holds no indicator that could open a collection, an
/// anchor, a tag, or a comment, no `: `, no `#`, no break, tab, or
/// unprintable character, does not end in `:` or a space, and is not a
/// word either schema reads as a bool or a null
pub fn plain_safe(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if YAML11_WORDS.contains(&s) || !matches!(resolve_plain(s), Plain::Str(_)) {
        return false;
    }
    if s.ends_with(':') || s.ends_with(' ') {
        return false;
    }
    if s.contains(": ") || s.contains(" #") {
        return false;
    }
    for ch in s.chars() {
        let cp = ch as u32;
        if "[]{},&*!|>'\"%@`#".contains(ch) {
            return false;
        }
        if cp < 0x20 || cp == 0x7f || (0x80..=0x9f).contains(&cp) {
            return false;
        }
        if cp == 0xfeff || cp == 0xfffe || cp == 0xffff {
            return false;
        }
    }
    true
}
fn yaml_str(s: &str) -> String {
    if plain_safe(s) {
        s.to_string()
    } else {
        json_str(s)
    }
}
fn is_empty_coll(v: &Value) -> bool {
    match v {
        Value::JArr(items) => items.is_empty(),
        Value::JObj(entries) => entries.is_empty(),
        _ => false,
    }
}
fn is_block(v: &Value) -> bool {
    matches!(v, Value::JArr(_) | Value::JObj(_)) && !is_empty_coll(v)
}
fn scalar_text(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => fmt_f(*f),
        Value::Str(s) => yaml_str(s),
        Value::JArr(_) => "[]".into(),
        Value::JObj(_) => "{}".into(),
        _ => panic!("to_yaml: unexpected value"),
    }
}
// the lines of a block node: the first without its indentation (the
// caller places it after `- ` or on a line of its own), the rest with
// `ind` in front of them
fn block_lines(v: &Value, ind: &str, step: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    match v {
        Value::JArr(items) => {
            for item in items.iter() {
                let sub = if is_block(item) {
                    block_lines(item, &format!("{ind}  "), step)
                } else {
                    vec![scalar_text(item)]
                };
                let lead = if out.is_empty() { "" } else { ind };
                out.push(format!("{lead}- {}", sub[0]));
                out.extend(sub[1..].iter().cloned());
            }
        }
        Value::JObj(entries) => {
            for (k, x) in entries.iter() {
                let key = yaml_str(k);
                let lead = if out.is_empty() { "" } else { ind };
                if !is_block(x) {
                    out.push(format!("{lead}{key}: {}", scalar_text(x)));
                    continue;
                }
                out.push(format!("{lead}{key}:"));
                let sub = block_lines(x, &format!("{ind}{step}"), step);
                out.push(format!("{ind}{step}{}", sub[0]));
                out.extend(sub[1..].iter().cloned());
            }
        }
        _ => unreachable!(),
    }
    out
}
/// the YAML text of a JSON value (read_json's shape), block style, no trailing newline
pub fn to_yaml(v: &Value, indent: usize) -> String {
    let step = " ".repeat(indent);
    if is_block(v) {
        block_lines(v, "", &step).join("\n")
    } else {
        scalar_text(v)
    }
}

// ---------------- the JSON layouts (§4.1) ----------------
/// the JSON text of a value (read_json's shape): canonical for indent 0, laid out with `indent` spaces per level otherwise
pub fn to_json(v: &Value, indent: usize) -> String {
    fn go(x: &Value, ind: &str, indent: usize) -> String {
        match x {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_f(*f),
            Value::Str(s) => json_str(s),
            Value::JArr(items) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let inner = format!("{ind}{}", " ".repeat(indent));
                if indent == 0 {
                    return format!(
                        "[{}]",
                        items
                            .iter()
                            .map(|e| go(e, ind, indent))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                format!(
                    "[\n{}\n{ind}]",
                    items
                        .iter()
                        .map(|e| format!("{inner}{}", go(e, &inner, indent)))
                        .collect::<Vec<_>>()
                        .join(",\n")
                )
            }
            Value::JObj(entries) => {
                if entries.is_empty() {
                    return "{}".into();
                }
                let inner = format!("{ind}{}", " ".repeat(indent));
                if indent == 0 {
                    return format!(
                        "{{{}}}",
                        entries
                            .iter()
                            .map(|(k, e)| format!("{}:{}", json_str(k), go(e, ind, indent)))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                format!(
                    "{{\n{}\n{ind}}}",
                    entries
                        .iter()
                        .map(|(k, e)| format!("{inner}{}: {}", json_str(k), go(e, &inner, indent)))
                        .collect::<Vec<_>>()
                        .join(",\n")
                )
            }
            _ => panic!("to_json: unexpected value"),
        }
    }
    go(v, "", indent)
}
