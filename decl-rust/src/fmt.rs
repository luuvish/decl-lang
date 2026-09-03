//! Canonical formatter — a port of the reference implementation's fmt.ts
//! (ROADMAP Phase 4; §2.1/D1): LF, 4-space indentation, no tabs,
//! normalized intra-line spacing. The original line structure is
//! preserved — §2.9 makes newlines separators, so where a construct
//! breaks lines is the author's statement — and the formatter re-derives
//! indentation and token spacing deterministically, which makes it
//! idempotent by construction.
use crate::parse::LANGUAGE;
use tree_sitter::{Language, Node, Parser};

struct Leaf {
    text: String,
    kind: String,
    parent: String,
    row: usize,
    end_row: usize,
    col: usize,
}

// atoms: leaves kept verbatim, including their internal whitespace
const ATOMS: [&str; 7] = ["string", "template_string", "pattern", "unit_literal", "doc_comment", "line_comment", "block_comment"];
const BIN_OPS: [&str; 24] = ["=", "==", "!=", "<=", ">=", "+", "*", "/", "%", "&&", "||", "??", "|>", "=>", "<<", ">>", "in", "matches", "with", "then", "else", "for", "if", "as"];
const BIN_OPS_EXTRA: [&str; 1] = ["from"];
const CONT_STARTERS: [&str; 22] = ["else", "=", "for", "if", "&&", "||", "|>", "??", ".", "?.", "+", "-", "*", "/", "==", "!=", "<=", ">=", "<", ">", "=>", "then"];
// a line whose last token leaves an expression open (`=`, `=>`, a binary
// operator, `then`/`else`) makes the next line a continuation too
const CONT_ENDERS: [&str; 29] = [
    "=", "=>", "&&", "||", "|>", "??", "+", "-", "*", "/", "%", "==", "!=", "<=", ">=", "<", ">", "&", "|", "^", "<<", ">>", "..", "..<",
    "then", "else", "in", "with", "matches",
];
const KEYWORDS: [&str; 27] = ["type", "const", "func", "output", "input", "export", "import", "diagnostic", "dimension", "unit", "assert", "when", "if", "then", "else", "match", "for", "in", "with", "as", "from", "true", "false", "null", "error", "warn", "info"];
const KEYWORDS_EXTRA: [&str; 1] = ["matches"];

/// a JavaScript string's length: UTF-16 code units
pub fn u16len(s: &str) -> usize {
    s.encode_utf16().count()
}
fn is_atom(kind: &str) -> bool {
    ATOMS.contains(&kind)
}
// name-like: `[A-Za-z_$][A-Za-z0-9_]*\$?` — a hidden member's name `x$` is name-like too (D34)
fn keywordy(t: &str) -> bool {
    let mut cs = t.chars();
    let Some(c0) = cs.next() else { return false };
    if !(c0.is_ascii_alphabetic() || c0 == '_' || c0 == '$') {
        return false;
    }
    let rest: Vec<char> = cs.collect();
    let body: &[char] = match rest.split_last() {
        Some(('$', b)) => b,
        _ => &rest[..],
    };
    body.iter().all(|c| c.is_ascii_alphanumeric() || *c == '_')
}
fn is_keyword(t: &str) -> bool {
    KEYWORDS.contains(&t) || KEYWORDS_EXTRA.contains(&t)
}
fn is_bin_op(t: &str) -> bool {
    BIN_OPS.contains(&t) || BIN_OPS_EXTRA.contains(&t)
}

fn collect(n: Node, src: &str, lines: &[&str], out: &mut Vec<Leaf>) {
    if is_atom(n.kind()) || n.child_count() == 0 {
        let text = n.utf8_text(src.as_bytes()).unwrap_or("");
        if text.is_empty() {
            return; // zero-width externals (NEWLINE)
        }
        let row = n.start_position().row;
        let col = lines.get(row).map(|l| u16len(l.get(..n.start_position().column).unwrap_or(""))).unwrap_or(0);
        out.push(Leaf {
            text: text.to_string(),
            kind: n.kind().to_string(),
            parent: n.parent().map(|p| p.kind().to_string()).unwrap_or_default(),
            row,
            end_row: n.end_position().row,
            col,
        });
        return;
    }
    let mut cur = n.walk();
    for c in n.children(&mut cur) {
        collect(c, src, lines, out);
    }
}

fn is_type_angle(l: &Leaf) -> bool {
    (l.text == "<" || l.text == ">") && (l.parent == "type_arguments" || l.parent == "type_parameters")
}

/// spacing decision: does a space go between a and b on one line?
fn spaced(a: &Leaf, b: &Leaf, prev: Option<&Leaf>) -> bool {
    let (at, bt) = (a.text.as_str(), b.text.as_str());
    // comments keep at least one space before them (handled by caller)
    if b.kind.ends_with("comment") {
        return true;
    }
    if is_type_angle(a) && at == "<" {
        return false;
    }
    if is_type_angle(b) {
        return false; // Vec<...>, no space before either angle
    }
    if at == "(" || at == "[" {
        return false;
    }
    if bt == ")" || bt == "]" || bt == "," || bt == ":" {
        return false;
    }
    if bt == "?" || at == "?" {
        return false; // int?, name?:
    }
    if at == "." || bt == "." || at == "?." || bt == "?." {
        return false;
    }
    if bt == ";" {
        return false;
    }
    if at == ".." || at == "..<" || bt == ".." || bt == "..<" {
        return false;
    }
    if bt == "(" {
        // call/parameter parens attach to a name or closing bracket; grouping parens do not
        return !(keywordy(at) && !is_keyword(at)) && at != ")" && at != "]" && !is_type_angle(a);
    }
    if bt == "[" {
        // index/size brackets attach (also after a record type or literal: `{...}[]`); array literals stand off
        return !(keywordy(at) || at == ")" || at == "]" || at == "}" || is_type_angle(a));
    }
    if at == "{" || bt == "}" {
        return true; // { a: 1 }
    }
    if bt == "{" || at == "}" {
        return true;
    }
    if at == "!" || at == "~" {
        return false; // unary
    }
    if at == "-" || at == "+" {
        // unary sign: previous token is an operator, opener, or keyword
        let unary = match prev {
            None => true,
            Some(p) => {
                let pt = p.text.as_str();
                is_bin_op(pt) || ["(", "[", "{", ",", ":", "<", "..", "..<", "-", "+", "!", "~"].contains(&pt) || (keywordy(pt) && is_keyword(pt))
            }
        };
        if unary {
            return false;
        }
    }
    true
}

pub fn format(src: &str) -> Result<String, String> {
    let mut parser = Parser::new();
    let lang: Language = LANGUAGE.into();
    parser.set_language(&lang).map_err(|e| e.to_string())?;
    let tree = parser.parse(src, None).ok_or("parse failed")?;
    if tree.root_node().has_error() {
        return Err("cannot format: file has parse errors".into());
    }
    let src_lines: Vec<&str> = src.split('\n').collect();
    let mut leaves: Vec<Leaf> = vec![];
    collect(tree.root_node(), src, &src_lines, &mut leaves);

    // group leaves by their original starting row
    let mut lines: Vec<Vec<Leaf>> = vec![];
    for l in leaves {
        match lines.last_mut() {
            Some(bucket) if bucket[0].row == l.row => bucket.push(l),
            _ => lines.push(vec![l]),
        }
    }

    let mut out: Vec<String> = vec![];
    let mut depth: usize = 0;
    let mut last_row_end: i64 = -1; // last original row consumed (multiline atoms span rows)
    let mut last_code: Option<&Leaf> = None; // the previous line's last non-comment token
    for line in &lines {
        let first = &line[0];
        if (first.row as i64) <= last_row_end {
            continue; // inside a multiline atom
        }
        // one blank line max between constructs
        if !out.is_empty() && (first.row as i64) > last_row_end + 1 {
            out.push(String::new());
        }
        // indentation: bracket depth, closers on the line start dedent first
        let closers = line.iter().take_while(|l| l.text == ")" || l.text == "]" || l.text == "}").count();
        let mut indent = depth.saturating_sub(closers);
        // a line starting with a continuation token, or following a line that
        // left an expression open, hangs one level deeper
        // (`ref<...>` closes a type, it opens nothing)
        let after_open = last_code.map(|l| !is_atom(&l.kind) && CONT_ENDERS.contains(&l.text.as_str()) && !is_type_angle(l)).unwrap_or(false);
        if closers == 0 && (CONT_STARTERS.contains(&first.text.as_str()) || after_open) {
            indent = depth + 1;
        }
        let mut text = "    ".repeat(indent);
        let mut prev: Option<&Leaf> = None;
        let mut prev2: Option<&Leaf> = None;
        for l in line {
            if let Some(p) = prev {
                if l.kind.ends_with("comment") {
                    // inline comment: keep the author's alignment (min one space)
                    let gap = (l.col as i64) - ((p.col + u16len(&p.text)) as i64);
                    text.push_str(&" ".repeat(gap.max(1) as usize));
                } else if spaced(p, l, prev2) {
                    text.push(' ');
                }
            }
            text.push_str(&l.text);
            if !is_atom(&l.kind) {
                for ch in l.text.chars() {
                    match ch {
                        '{' | '[' | '(' => depth += 1,
                        '}' | ']' | ')' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            prev2 = prev;
            prev = Some(l);
            if !l.kind.ends_with("comment") {
                last_code = Some(l);
            }
            last_row_end = last_row_end.max(l.end_row as i64);
        }
        out.push(text.trim_end_matches([' ', '\t']).to_string());
    }
    Ok(out.join("\n") + "\n")
}
