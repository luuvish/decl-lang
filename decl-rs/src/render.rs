//! The renderer (docs/tooling/05_render.md): the form a module declares
//! for an output with `@render` — a format and a layout, a template, a
//! destination, a fan-out — read from the annotation (§3), the structured
//! text of a document in that form (§4), and the templates (§5) and the
//! fan-out (§6) that turn one evaluated root into text or files. The
//! command line, the REPL, the library, and the editor preview all emit
//! through here, so that the three implementations print the same bytes.
//! A port of the reference's render.ts.
use crate::ast::{Decl, Expr};
use crate::semantics::Value;
use crate::yaml::{to_json, to_yaml};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

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
