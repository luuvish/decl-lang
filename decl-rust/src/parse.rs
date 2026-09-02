//! CST -> AST lowering over the compiled tree-sitter grammar (parse.ts).
use crate::ast::*;
use crate::semantics::Value;
use num_bigint::BigInt;
use num_traits::Num;
use std::rc::Rc;
use tree_sitter::{Language, Node, Parser};
use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_decl() -> *const ();
}
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_decl) };

pub struct ParseResult {
    pub decls: Vec<Decl>,
    pub errors: Vec<(usize, usize)>,
}

struct Lower<'a> {
    src: &'a [u8],
}

pub fn parse_source(src: &str) -> ParseResult {
    let mut parser = Parser::new();
    let lang: Language = LANGUAGE.into();
    parser.set_language(&lang).expect("grammar");
    let tree = parser.parse(src, None).expect("parse");
    let root = tree.root_node();
    let mut errors = Vec::new();
    collect_errors(root, &mut errors);
    let lw = Lower { src: src.as_bytes() };
    let mut decls = Vec::new();
    let mut cur = root.walk();
    for c in root.named_children(&mut cur) {
        if c.kind() == "ERROR" {
            continue;
        }
        match lw.decl(c) {
            Ok(Some(mut d)) => {
                let exported = c.prev_sibling().map(|p| lw.text(p) == "export").unwrap_or(false)
                    || matches!(d.body, DeclBody::ReExport { .. });
                d.exported = exported;
                decls.push(d);
            }
            Ok(None) => {}
            Err(_) => {
                if errors.is_empty() {
                    errors.push((c.start_position().row, c.start_position().column));
                }
            }
        }
    }
    ParseResult { decls, errors }
}

fn collect_errors(n: Node, out: &mut Vec<(usize, usize)>) {
    if n.kind() == "ERROR" || n.is_missing() {
        out.push((n.start_position().row, n.start_position().column));
    }
    if n.has_error() {
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            collect_errors(c, out);
        }
    }
}

type LR<T> = Result<T, String>;

impl<'a> Lower<'a> {
    fn text(&self, n: Node) -> String {
        n.utf8_text(self.src).unwrap_or("").to_string()
    }
    fn field<'b>(&self, n: Node<'b>, name: &str) -> Option<Node<'b>> {
        n.child_by_field_name(name)
    }
    fn req<'b>(&self, n: Node<'b>, name: &str) -> LR<Node<'b>> {
        n.child_by_field_name(name).ok_or_else(|| format!("missing field {name}"))
    }
    fn named<'b>(&self, n: Node<'b>) -> Vec<Node<'b>> {
        let mut cur = n.walk();
        n.named_children(&mut cur).collect()
    }
    fn all<'b>(&self, n: Node<'b>) -> Vec<Node<'b>> {
        let mut cur = n.walk();
        n.children(&mut cur).collect()
    }
    fn kids<'b>(&self, n: Node<'b>, kind: &str) -> Vec<Node<'b>> {
        self.named(n).into_iter().filter(|c| c.kind() == kind).collect()
    }
    fn kid<'b>(&self, n: Node<'b>, kind: &str) -> Option<Node<'b>> {
        self.named(n).into_iter().find(|c| c.kind() == kind)
    }
    fn json_string(&self, n: Node) -> LR<String> {
        json_unquote(&self.text(n).replace('\n', "\\n"))
    }

    // ---------------- declarations ----------------
    fn decl(&self, n: Node) -> LR<Option<Decl>> {
        let body = match n.kind() {
            "type_declaration" => {
                let params = match self.kid(n, "type_parameters") {
                    Some(ps) => self
                        .kids(ps, "type_parameter")
                        .into_iter()
                        .map(|p| {
                            let nc = self.named(p);
                            Ok(Param { name: self.text(nc[0]), ty: if nc.len() > 1 { Some(self.ty(nc[1])?) } else { None } })
                        })
                        .collect::<LR<Vec<_>>>()?,
                    None => vec![],
                };
                DeclBody::Type { name: self.text(self.req(n, "name")?), params, ty: self.ty(self.req(n, "type")?)?, tail: self.maybe_tail(n)? }
            }
            "const_declaration" => DeclBody::Const {
                name: self.text(self.req(n, "name")?),
                ty: match self.field(n, "type") { Some(t) => Some(self.ty(t)?), None => None },
                expr: self.expr(self.req(n, "value")?)?,
            },
            "func_declaration" => DeclBody::Func {
                name: self.text(self.req(n, "name")?),
                params: self.params(n)?,
                ret: match self.field(n, "return_type") { Some(t) => Some(self.ty(t)?), None => None },
                body: self.expr(self.req(n, "body")?)?,
            },
            "output_declaration" => DeclBody::Output {
                name: self.text(self.req(n, "name")?),
                ty: self.ty(self.req(n, "type")?)?,
                expr: self.expr(self.req(n, "value")?)?,
            },
            "input_declaration" => DeclBody::Input {
                name: self.text(self.req(n, "name")?),
                ty: self.ty(self.req(n, "type")?)?,
                fallback: match self.field(n, "fallback") { Some(f) => Some(self.expr(f)?), None => None },
            },
            "diagnostic_declaration" => DeclBody::Diagnostic {
                name: self.text(self.req(n, "name")?),
                params: self.params(n)?,
                severity: self.text(self.kid(n, "severity").ok_or("severity")?),
                template: self.template_parts(self.kid(n, "template_string").ok_or("template")?)?,
            },
            "dimension_declaration" => DeclBody::Dimension {
                name: self.text(self.req(n, "name")?),
                terms: self.kid(n, "dimension_expression").map(|e| self.dim_expr(e)),
            },
            "unit_declaration" => match self.field(n, "dimension") {
                Some(d) => DeclBody::Unit { name: self.text(self.req(n, "name")?), dim: Some(self.text(d)), factor: None, base: None },
                None => DeclBody::Unit {
                    name: self.text(self.req(n, "name")?),
                    dim: None,
                    factor: Some(self.expr(self.req(n, "factor")?)?),
                    base: Some(self.text(self.req(n, "base")?)),
                },
            },
            "import_declaration" => {
                let from = self.json_string(self.kid(n, "string").ok_or("from")?)?;
                match self.kid(n, "named_imports") {
                    Some(ni) => DeclBody::Import { from, names: Some(self.import_items(ni)), ns: None },
                    None => DeclBody::Import { from, names: None, ns: Some(self.text(self.kid(n, "identifier").ok_or("ns")?)) },
                }
            }
            "re_export_declaration" => DeclBody::ReExport {
                from: self.json_string(self.kid(n, "string").ok_or("from")?)?,
                names: self.import_items(n),
            },
            _ => return Ok(None),
        };
        Ok(Some(Decl { body, exported: false }))
    }
    fn params(&self, n: Node) -> LR<Vec<Param>> {
        self.kids(n, "parameter")
            .into_iter()
            .map(|p| {
                let nc = self.named(p);
                Ok(Param { name: self.text(nc[0]), ty: Some(self.ty(nc[1])?) })
            })
            .collect()
    }
    fn import_items(&self, n: Node) -> Vec<ImportItem> {
        self.kids(n, "import_item")
            .into_iter()
            .map(|it| {
                let ids = self.named(it);
                ImportItem { name: self.text(ids[0]), alias: ids.get(1).map(|a| self.text(*a)) }
            })
            .collect()
    }
    fn maybe_tail(&self, n: Node) -> LR<Option<Tail>> {
        match self.kid(n, "else_clause") {
            Some(t) => Ok(Some(self.tail(t)?)),
            None => Ok(None),
        }
    }
    fn tail(&self, n: Node) -> LR<Tail> {
        if let Some(sev) = self.kid(n, "severity") {
            return Ok(Tail::Inline { severity: self.text(sev), template: self.template_parts(self.kid(n, "template_string").ok_or("tmpl")?)? });
        }
        let name = self.text(self.kid(n, "qualified_name").ok_or("name")?);
        let args = self.named(n).into_iter().filter(|c| c.kind() != "qualified_name").map(|c| self.expr(c)).collect::<LR<Vec<_>>>()?;
        Ok(Tail::Ref { name, args })
    }
    fn template_parts(&self, n: Node) -> LR<Vec<TPart>> {
        let mut parts = Vec::new();
        for c in self.named(n) {
            match c.kind() {
                "template_chars" => parts.push(TPart::Text(self.text(c))),
                "template_escape" => {
                    let t = self.text(c);
                    let s = match t.as_str() { "\\n" => "\n", "\\t" => "\t", "\\r" => "\r", other => &other[1..] };
                    parts.push(TPart::Text(s.to_string()));
                }
                "interpolation" => parts.push(TPart::Expr(self.expr(self.named(c)[0])?)),
                _ => {}
            }
        }
        Ok(parts)
    }

    // ---------------- types ----------------
    fn ty(&self, n: Node) -> LR<TypeAst> {
        Ok(match n.kind() {
            "union_type" => TypeAst::Union(self.named(n).into_iter().map(|c| self.ty(c)).collect::<LR<_>>()?),
            "intersection_type" => TypeAst::Isect(self.named(n).into_iter().map(|c| self.ty(c)).collect::<LR<_>>()?),
            "nullable_type" => TypeAst::Union(vec![self.ty(self.named(n)[0])?, TypeAst::Prim("null".into())]),
            "array_type" => {
                let elem = Box::new(self.ty(self.named(n)[0])?);
                let range = self.kid(n, "array_size_range").or_else(|| self.field(n, "size").filter(|s| s.kind() == "range_expression"));
                if let Some(r) = range {
                    let ends: Vec<Value> = self.named(r).into_iter().map(|c| self.const_num(c)).collect::<LR<_>>()?;
                    let excl = self.all(r).iter().any(|c| !c.is_named() && self.text(*c) == "..<");
                    let lo = num_or_name(&ends[0]);
                    let hi = num_or_name(&ends[1]);
                    return Ok(match hi {
                        Value::Int(h) => TypeAst::Array { elem, lo: Some(lo), hi: Some(Value::Int(if excl { h - 1 } else { h })), excl: false },
                        other => TypeAst::Array { elem, lo: Some(lo), hi: Some(other), excl },
                    });
                }
                if let Some(size) = self.field(n, "size") {
                    let v = num_or_name(&self.const_num(size)?);
                    return Ok(TypeAst::Array { elem, lo: Some(v.clone()), hi: Some(v), excl: false });
                }
                TypeAst::Array { elem, lo: None, hi: None, excl: false }
            }
            "range_type" => {
                let nc = self.named(n);
                TypeAst::Range { lo: self.const_num(nc[0])?, hi: self.const_num(nc[1])?, excl: self.text(n).contains("..<") }
            }
            "number_literal" => TypeAst::Lit(self.const_num(n)?),
            "string" => TypeAst::Lit(Value::Str(self.json_string(n)?)),
            "pattern" => {
                let t = self.text(n);
                TypeAst::Pattern(t[1..t.len() - 1].to_string())
            }
            "paren_type" => self.ty(self.named(n)[0])?,
            "record_type" => {
                let mut open = false;
                let mut members = Vec::new();
                for c in self.named(n) {
                    if c.kind() == "open_marker" {
                        open = true;
                        continue;
                    }
                    if let Some(m) = self.member(c)? {
                        members.push(m);
                    }
                }
                TypeAst::Record { members, open }
            }
            "map_type" => TypeAst::Map { key: Box::new(self.ty(self.req(n, "key")?)?), val: Box::new(self.ty(self.req(n, "value")?)?) },
            "function_type" => {
                let mut cs: Vec<TypeAst> = self.named(n).into_iter().map(|c| self.ty(c)).collect::<LR<_>>()?;
                let ret = cs.pop().ok_or("func type")?;
                TypeAst::Func { params: cs, ret: Box::new(ret) }
            }
            "named_type" => {
                let name = self.text(self.kid(n, "qualified_name").ok_or("name")?);
                let args = match self.kid(n, "type_arguments") {
                    Some(a) => self.named(a).into_iter().map(|c| self.ty(c)).collect::<LR<_>>()?,
                    None => vec![],
                };
                let preds = match self.field(n, "predicates") {
                    Some(p) => Some(self.named(p).into_iter().map(|c| self.expr(c)).collect::<LR<_>>()?),
                    None => None,
                };
                let ext = match self.field(n, "extension") { Some(e) => Some(Box::new(self.ty(e)?)), None => None };
                if ["int", "uint", "float", "bool", "string"].contains(&name.as_str()) && args.is_empty() && preds.is_none() && ext.is_none() {
                    return Ok(TypeAst::Prim(name));
                }
                TypeAst::Named { name, args, preds, ext }
            }
            _ => match self.text(n).as_str() {
                "true" => TypeAst::Lit(Value::Bool(true)),
                "false" => TypeAst::Lit(Value::Bool(false)),
                "null" => TypeAst::Prim("null".into()),
                other => return Err(format!("lower_type: unhandled {} '{}'", n.kind(), other)),
            },
        })
    }
    fn dim_expr(&self, n: Node) -> Vec<(String, i32)> {
        let mut out = Vec::new();
        let mut sign = 1;
        for c in self.all(n) {
            if !c.is_named() {
                match self.text(c).as_str() { "/" => sign = -1, "*" => sign = 1, _ => {} }
                continue;
            }
            if c.kind() == "dimension_term" {
                let nc = self.named(c);
                let ident = nc.iter().find(|x| x.kind() == "identifier").unwrap();
                let num = nc.iter().find(|x| x.kind() == "int");
                let mut exp: i32 = num.map(|x| self.text(*x).parse().unwrap_or(1)).unwrap_or(1);
                if self.all(c).iter().any(|x| !x.is_named() && self.text(*x) == "-") {
                    exp = -exp;
                }
                out.push((self.text(*ident), exp * sign));
                sign = 1;
            }
        }
        out
    }
    fn const_num(&self, n: Node) -> LR<Value> {
        match n.kind() {
            "number_literal" => {
                let neg = self.text(n).trim_start().starts_with('-');
                let v = self.const_num(self.named(n)[0])?;
                Ok(if neg { neg_value(v) } else { v })
            }
            "int" => Ok(Value::Int(parse_int(&self.text(n))?)),
            "float" => Ok(Value::Float(self.text(n).replace('_', "").parse::<f64>().map_err(|e| e.to_string())?)),
            "qualified_name" | "identifier" => Ok(Value::Str(self.text(n))),
            k => Err(format!("const_num: {k}")),
        }
    }

    // ---------------- members ----------------
    fn member(&self, n: Node) -> LR<Option<MemberAst>> {
        Ok(Some(match n.kind() {
            "value_member" => {
                let name_n = self.req(n, "name")?;
                MemberAst::Value {
                    name: if name_n.kind() == "string" { self.json_string(name_n)? } else { self.text(name_n) },
                    opt: self.field(n, "optional").is_some(),
                    ty: self.ty(self.req(n, "type")?)?,
                    dflt: match self.field(n, "default") { Some(d) => Some(self.expr(d)?), None => None },
                }
            }
            "derived_member" => {
                let name_n = self.req(n, "name")?;
                MemberAst::Derived {
                    name: if name_n.kind() == "string" { self.json_string(name_n)? } else { self.text(name_n) },
                    ty: match self.field(n, "type") { Some(t) => Some(self.ty(t)?), None => None },
                    expr: self.expr(self.req(n, "value")?)?,
                }
            }
            "context_declaration" => MemberAst::Context { variable: self.text(self.req(n, "variable")?), ty: self.ty(self.req(n, "type")?)? },
            "assert_member" => MemberAst::Assert { name: self.text(self.req(n, "name")?), cond: self.expr(self.req(n, "condition")?)?, tail: self.maybe_tail(n)? },
            "when_member" => {
                let mut body = Vec::new();
                for c in self.named(n).into_iter().skip(1) {
                    if let Some(m) = self.member(c)? {
                        body.push(m);
                    }
                }
                MemberAst::When { cond: self.expr(self.req(n, "condition")?)?, body }
            }
            _ => return Ok(None),
        }))
    }

    // ---------------- expressions ----------------
    fn expr(&self, n: Node) -> LR<Rc<Expr>> {
        Ok(Rc::new(self.expr_inner(n)?))
    }
    fn expr_inner(&self, n: Node) -> LR<Expr> {
        const BIN: [&str; 13] = [
            "pipe_expression", "nullish_expression", "binary_expression_or", "binary_expression_and",
            "bit_or_expression", "bit_xor_expression", "bit_and_expression", "equality_expression",
            "relational_expression", "range_expression", "shift_expression", "additive_expression",
            "multiplicative_expression",
        ];
        Ok(match n.kind() {
            "int" => Expr::Lit(Value::Int(parse_int(&self.text(n))?)),
            "float" => Expr::Lit(Value::Float(self.text(n).replace('_', "").parse::<f64>().map_err(|e| e.to_string())?)),
            "unit_literal" => {
                let t = self.text(n);
                let re = regex::Regex::new(r"^([0-9._]+(?:[eE][+-]?[0-9]+)?)([A-Za-z][A-Za-z0-9]*)$").unwrap();
                let caps = re.captures(&t).ok_or("unit literal")?;
                Expr::UnitLit { num: caps[1].replace('_', "").parse::<f64>().map_err(|e| e.to_string())?, unit: caps[2].to_string() }
            }
            "string" => Expr::Lit(Value::Str(self.json_string(n)?)),
            "template_string" => Expr::Template(self.template_parts(n)?),
            "identifier" => Expr::Name(self.text(n)),
            "context_variable" => Expr::Ctx(self.text(n)),
            "referrers_expression" => Expr::Referrers { ty: self.text(self.req(n, "type")?), member: self.json_string(self.req(n, "member")?)? },
            "paren_expression" => Expr::Paren(self.expr(self.named(n)[0])?),
            "unary_expression" => Expr::Un { op: self.text(self.all(n)[0]), x: self.expr(self.named(n)[0])? },
            "if_expression" => Expr::If { c: self.expr(self.req(n, "condition")?)?, t: self.expr(self.req(n, "then")?)?, f: self.expr(self.req(n, "else")?)? },
            "lambda" => Expr::Lambda {
                params: self.kids(n, "lambda_parameter").into_iter().map(|p| self.text(self.named(p)[0])).collect(),
                body: self.expr(self.req(n, "body")?)?,
            },
            "with_expression" => {
                let nc = self.named(n);
                Expr::With { base: self.expr(nc[0])?, patch: self.expr(nc[1])? }
            }
            "member_access" | "safe_access" => {
                let nc = self.named(n);
                Expr::Member {
                    x: self.expr(nc[0])?,
                    name: if nc[1].kind() == "string" { self.json_string(nc[1])? } else { self.text(nc[1]) },
                    safe: n.kind() == "safe_access",
                }
            }
            "index_access" => {
                let nc = self.named(n);
                Expr::Index { x: self.expr(nc[0])?, i: self.expr(nc[1])? }
            }
            "call" => {
                let cs: Vec<Node> = self.all(n).into_iter().filter(|c| c.is_named() || ["true", "false", "null"].contains(&self.text(*c).as_str())).collect();
                Expr::Call { fun: self.expr(cs[0])?, args: cs[1..].iter().map(|c| self.expr(*c)).collect::<LR<_>>()? }
            }
            "object" => {
                if let Some(comp) = self.kid(n, "map_comprehension") {
                    return self.expr_inner(comp);
                }
                let mut entries = Vec::new();
                for en in self.kids(n, "object_entry") {
                    match self.field(en, "key") {
                        Some(k) => entries.push((if k.kind() == "string" { self.json_string(k)? } else { self.text(k) }, self.expr(self.req(en, "value")?)?)),
                        None => entries.push(("...".to_string(), self.expr(self.named(en)[0])?)),
                    }
                }
                Expr::Obj(entries)
            }
            "map_comprehension" => Expr::MapComp {
                key: self.expr(self.req(n, "key")?)?,
                val: self.expr(self.req(n, "value")?)?,
                clauses: self.kids(n, "for_clause").into_iter().map(|c| self.for_clause(c)).collect::<LR<_>>()?,
            },
            "array" => {
                if let Some(comp) = self.kid(n, "array_comprehension") {
                    return self.expr_inner(comp);
                }
                let mut items = Vec::new();
                for en in self.kids(n, "array_entry") {
                    let spread = self.text(en).starts_with("...");
                    let inner = self.named(en).into_iter().next().or_else(|| self.all(en).into_iter().find(|c| ["true", "false", "null"].contains(&self.text(*c).as_str()))).ok_or("entry")?;
                    items.push((spread, self.expr(inner)?));
                }
                Expr::Arr(items)
            }
            "array_comprehension" => Expr::Comp {
                head: self.expr(self.req(n, "head")?)?,
                clauses: self.kids(n, "for_clause").into_iter().map(|c| self.for_clause(c)).collect::<LR<_>>()?,
            },
            "matches_expression" => {
                let nc = self.named(n);
                Expr::Bin { op: "matches".into(), l: self.expr(nc[0])?, r: self.expr(nc[1])? }
            }
            "pattern" => {
                let t = self.text(n);
                Expr::Pattern(t[1..t.len() - 1].to_string())
            }
            "match_expression" => {
                let mut arms = Vec::new();
                for a in self.kids(n, "match_arm") {
                    let body = self.req(a, "body")?;
                    let others: Vec<Node> = self.named(a).into_iter().filter(|c| c.id() != body.id()).collect();
                    arms.push(MatchArm { v: self.text(others[0]), ty: if others.len() > 1 { Some(self.ty(others[1])?) } else { None }, body: self.expr(body)? });
                }
                Expr::Match { subject: self.expr(self.req(n, "subject")?)?, arms }
            }
            k if BIN.contains(&k) => {
                let nc = self.named(n);
                let op = self.all(n).into_iter().filter(|c| !c.is_named()).map(|c| self.text(c)).find(|t| !t.trim().is_empty()).ok_or("op")?;
                Expr::Bin { op, l: self.expr(nc[0])?, r: self.expr(nc[1])? }
            }
            _ => match self.text(n).as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                other => return Err(format!("lower_expr: unhandled {} '{}'", n.kind(), other)),
            },
        })
    }
    fn for_clause(&self, n: Node) -> LR<ForClause> {
        let mut cur = n.walk();
        let filters = n.children_by_field_name("filter", &mut cur).map(|c| self.expr(c)).collect::<LR<Vec<_>>>()?;
        Ok(ForClause { v: self.text(self.req(n, "variable")?), iter: self.expr(self.req(n, "iterable")?)?, filters })
    }
}

fn num_or_name(v: &Value) -> Value {
    match v {
        Value::Float(f) => Value::Int(BigInt::from(*f as i64)),
        other => other.clone(),
    }
}
fn neg_value(v: Value) -> Value {
    match v {
        Value::Int(i) => Value::Int(-i),
        Value::Float(f) => Value::Float(-f),
        other => other,
    }
}
pub fn parse_int(text: &str) -> LR<BigInt> {
    let t = text.replace('_', "");
    let (radix, digits) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, h.to_string())
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (8, o.to_string())
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, b.to_string())
    } else {
        (10, t.clone())
    };
    BigInt::from_str_radix(&digits, radix).map_err(|e| e.to_string())
}

/// JSON string literal -> its value (the lexer guarantees the form)
pub fn json_unquote(s: &str) -> LR<String> {
    let inner = &s[1..s.len() - 1];
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let cp = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    Ok(out)
}
