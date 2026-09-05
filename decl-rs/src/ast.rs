//! AST of the runtime — the shape the tree-sitter CST lowers into
//! (parse.rs), mirroring the reference implementation's ast.ts.
use crate::semantics::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// a source range: zero-based rows and columns (UTF-16 units, as the
/// reference reads them from web-tree-sitter), end exclusive
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loc {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
}

// Every expression node carries its source range in a side table keyed
// by the node's address (expressions are shared through `Rc`, so the
// address is the node's identity, as object identity is in the
// reference); types, members, and declarations carry `loc` inline.
thread_local! {
    static EXPR_LOCS: RefCell<HashMap<usize, Loc>> = RefCell::new(HashMap::new());
}
fn expr_key(e: &Rc<Expr>) -> usize {
    Rc::as_ptr(e) as *const u8 as usize
}
pub fn set_expr_loc(e: &Rc<Expr>, loc: Loc) {
    EXPR_LOCS.with(|t| t.borrow_mut().insert(expr_key(e), loc));
}
pub fn expr_loc(e: &Rc<Expr>) -> Option<Loc> {
    EXPR_LOCS.with(|t| t.borrow().get(&expr_key(e)).copied())
}

#[derive(Debug, Clone)]
pub enum TypeAst {
    Prim {
        name: String,
        loc: Option<Loc>,
    },
    Lit {
        v: Value,
        loc: Option<Loc>,
    },
    Range {
        lo: Value,
        hi: Value,
        excl: bool,
        loc: Option<Loc>,
    },
    Pattern {
        re: String,
        loc: Option<Loc>,
    },
    Record {
        members: Vec<MemberAst>,
        open: bool,
        loc: Option<Loc>,
    },
    Map {
        key: Box<TypeAst>,
        val: Box<TypeAst>,
        loc: Option<Loc>,
    },
    Array {
        elem: Box<TypeAst>,
        lo: Option<Value>,
        hi: Option<Value>,
        excl: bool,
        loc: Option<Loc>,
    },
    Union {
        arms: Vec<TypeAst>,
        loc: Option<Loc>,
    },
    Isect {
        arms: Vec<TypeAst>,
        loc: Option<Loc>,
    },
    Func {
        params: Vec<TypeAst>,
        ret: Box<TypeAst>,
        loc: Option<Loc>,
    },
    Named {
        name: String,
        args: Vec<TypeAst>,
        preds: Option<Vec<Rc<Expr>>>,
        ext: Option<Box<TypeAst>>,
        loc: Option<Loc>,
    },
}
impl TypeAst {
    pub fn loc(&self) -> Option<Loc> {
        match self {
            TypeAst::Prim { loc, .. }
            | TypeAst::Lit { loc, .. }
            | TypeAst::Range { loc, .. }
            | TypeAst::Pattern { loc, .. }
            | TypeAst::Record { loc, .. }
            | TypeAst::Map { loc, .. }
            | TypeAst::Array { loc, .. }
            | TypeAst::Union { loc, .. }
            | TypeAst::Isect { loc, .. }
            | TypeAst::Func { loc, .. }
            | TypeAst::Named { loc, .. } => *loc,
        }
    }
    pub fn set_loc(&mut self, l: Loc) {
        match self {
            TypeAst::Prim { loc, .. }
            | TypeAst::Lit { loc, .. }
            | TypeAst::Range { loc, .. }
            | TypeAst::Pattern { loc, .. }
            | TypeAst::Record { loc, .. }
            | TypeAst::Map { loc, .. }
            | TypeAst::Array { loc, .. }
            | TypeAst::Union { loc, .. }
            | TypeAst::Isect { loc, .. }
            | TypeAst::Func { loc, .. }
            | TypeAst::Named { loc, .. } => *loc = Some(l),
        }
    }
}

#[derive(Debug, Clone)]
pub enum MemberAst {
    Value {
        name: String,
        opt: bool,
        ty: TypeAst,
        dflt: Option<Rc<Expr>>,
        loc: Option<Loc>,
    },
    /// `hidden`: `x$ = e` — computed for the schema's own use, never part of the value (D34)
    Derived {
        name: String,
        ty: Option<TypeAst>,
        expr: Rc<Expr>,
        hidden: bool,
        loc: Option<Loc>,
    },
    Context {
        variable: String,
        ty: TypeAst,
        loc: Option<Loc>,
    },
    Assert {
        name: String,
        cond: Rc<Expr>,
        tail: Option<Tail>,
        loc: Option<Loc>,
    },
    When {
        cond: Rc<Expr>,
        body: Vec<MemberAst>,
        loc: Option<Loc>,
    },
}
impl MemberAst {
    pub fn loc(&self) -> Option<Loc> {
        match self {
            MemberAst::Value { loc, .. }
            | MemberAst::Derived { loc, .. }
            | MemberAst::Context { loc, .. }
            | MemberAst::Assert { loc, .. }
            | MemberAst::When { loc, .. } => *loc,
        }
    }
    pub fn set_loc(&mut self, l: Loc) {
        match self {
            MemberAst::Value { loc, .. }
            | MemberAst::Derived { loc, .. }
            | MemberAst::Context { loc, .. }
            | MemberAst::Assert { loc, .. }
            | MemberAst::When { loc, .. } => *loc = Some(l),
        }
    }
    /// the member's name (`name` in the reference: value, derived, and assert members)
    pub fn name(&self) -> Option<&str> {
        match self {
            MemberAst::Value { name, .. }
            | MemberAst::Derived { name, .. }
            | MemberAst::Assert { name, .. } => Some(name),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TPart {
    Text(String),
    Expr(Rc<Expr>),
}

#[derive(Debug, Clone)]
pub enum Tail {
    Inline {
        severity: String,
        template: Vec<TPart>,
    },
    Ref {
        name: String,
        args: Vec<Rc<Expr>>,
    },
}

#[derive(Debug, Clone)]
pub struct ForClause {
    pub v: String,
    pub iter: Rc<Expr>,
    pub filters: Vec<Rc<Expr>>,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub v: String,
    pub ty: Option<TypeAst>,
    pub body: Rc<Expr>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    UnitLit {
        num: f64,
        unit: String,
    },
    Template(Vec<TPart>),
    Name(String),
    Ctx(String),
    Referrers {
        ty: String,
        member: String,
    },
    Obj(Vec<(String, Rc<Expr>)>),
    Arr(Vec<(bool, Rc<Expr>)>),
    Comp {
        head: Rc<Expr>,
        clauses: Vec<ForClause>,
    },
    MapComp {
        key: Rc<Expr>,
        val: Rc<Expr>,
        clauses: Vec<ForClause>,
    },
    Bin {
        op: String,
        l: Rc<Expr>,
        r: Rc<Expr>,
    },
    Un {
        op: String,
        x: Rc<Expr>,
    },
    Paren(Rc<Expr>),
    If {
        c: Rc<Expr>,
        t: Rc<Expr>,
        f: Rc<Expr>,
    },
    Lambda {
        params: Vec<String>,
        body: Rc<Expr>,
    },
    Call {
        fun: Rc<Expr>,
        args: Vec<Rc<Expr>>,
    },
    Member {
        x: Rc<Expr>,
        name: String,
        safe: bool,
    },
    Index {
        x: Rc<Expr>,
        i: Rc<Expr>,
    },
    With {
        base: Rc<Expr>,
        patch: Rc<Expr>,
    },
    Pattern(String),
    Match {
        subject: Rc<Expr>,
        arms: Vec<MatchArm>,
    },
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeAst>,
}

#[derive(Debug, Clone)]
pub struct ImportItem {
    pub name: String,
    pub alias: Option<String>,
}

// `ret` is lowered for fidelity with the reference AST; the runtime does
// not check return annotations (that is the static checker's job)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DeclBody {
    Type {
        name: String,
        params: Vec<Param>,
        ty: TypeAst,
        tail: Option<Tail>,
    },
    Const {
        name: String,
        ty: Option<TypeAst>,
        expr: Rc<Expr>,
    },
    Func {
        name: String,
        params: Vec<Param>,
        ret: Option<TypeAst>,
        body: Rc<Expr>,
    },
    Output {
        name: String,
        ty: TypeAst,
        expr: Rc<Expr>,
    },
    Input {
        name: String,
        ty: TypeAst,
        fallback: Option<Rc<Expr>>,
    },
    Diagnostic {
        name: String,
        params: Vec<Param>,
        severity: String,
        template: Vec<TPart>,
    },
    Dimension {
        name: String,
        terms: Option<Vec<(String, i32)>>,
    },
    Unit {
        name: String,
        dim: Option<String>,
        factor: Option<Rc<Expr>>,
        base: Option<String>,
    },
    Import {
        from: String,
        names: Option<Vec<ImportItem>>,
        ns: Option<String>,
    },
    ReExport {
        from: String,
        names: Vec<ImportItem>,
    },
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub body: DeclBody,
    pub exported: bool,
    /// the declaration's source range (Phase 6 foundations); `export` included
    pub loc: Option<Loc>,
}

impl Decl {
    pub fn name(&self) -> Option<&str> {
        match &self.body {
            DeclBody::Type { name, .. }
            | DeclBody::Const { name, .. }
            | DeclBody::Func { name, .. }
            | DeclBody::Output { name, .. }
            | DeclBody::Input { name, .. }
            | DeclBody::Diagnostic { name, .. }
            | DeclBody::Dimension { name, .. }
            | DeclBody::Unit { name, .. } => Some(name),
            _ => None,
        }
    }
}

/// does an expression mention `$referrers` anywhere? (deferred slots)
pub fn mentions_referrers(e: &Expr) -> bool {
    match e {
        Expr::Referrers { .. } => true,
        Expr::Lit(_) | Expr::UnitLit { .. } | Expr::Name(_) | Expr::Ctx(_) | Expr::Pattern(_) => {
            false
        }
        Expr::Template(parts) => parts
            .iter()
            .any(|p| matches!(p, TPart::Expr(x) if mentions_referrers(x))),
        Expr::Obj(es) => es.iter().any(|(_, v)| mentions_referrers(v)),
        Expr::Arr(items) => items.iter().any(|(_, v)| mentions_referrers(v)),
        Expr::Comp { head, clauses } => {
            mentions_referrers(head) || clauses.iter().any(clause_mentions)
        }
        Expr::MapComp { key, val, clauses } => {
            mentions_referrers(key)
                || mentions_referrers(val)
                || clauses.iter().any(clause_mentions)
        }
        Expr::Bin { l, r, .. } => mentions_referrers(l) || mentions_referrers(r),
        Expr::Un { x, .. } | Expr::Paren(x) => mentions_referrers(x),
        Expr::If { c, t, f } => {
            mentions_referrers(c) || mentions_referrers(t) || mentions_referrers(f)
        }
        Expr::Lambda { body, .. } => mentions_referrers(body),
        Expr::Call { fun, args } => {
            mentions_referrers(fun) || args.iter().any(|a| mentions_referrers(a))
        }
        Expr::Member { x, .. } => mentions_referrers(x),
        Expr::Index { x, i } => mentions_referrers(x) || mentions_referrers(i),
        Expr::With { base, patch } => mentions_referrers(base) || mentions_referrers(patch),
        Expr::Match { subject, arms } => {
            mentions_referrers(subject) || arms.iter().any(|a| mentions_referrers(&a.body))
        }
    }
}

fn clause_mentions(c: &ForClause) -> bool {
    mentions_referrers(&c.iter) || c.filters.iter().any(|f| mentions_referrers(f))
}

pub fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Name(n) => n.clone(),
        Expr::Call { fun, .. } => expr_name(fun),
        _ => "<predicate>".into(),
    }
}
