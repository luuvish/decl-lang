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
    /// the start line
    pub sl: usize,
    /// the start column
    pub sc: usize,
    /// the end line
    pub el: usize,
    /// the end column, exclusive
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
/// Record an expression's source range.
pub fn set_expr_loc(e: &Rc<Expr>, loc: Loc) {
    EXPR_LOCS.with(|t| t.borrow_mut().insert(expr_key(e), loc));
}
/// An expression's source range, when recorded.
pub fn expr_loc(e: &Rc<Expr>) -> Option<Loc> {
    EXPR_LOCS.with(|t| t.borrow().get(&expr_key(e)).copied())
}

#[derive(Debug, Clone)]
/// a type expression (§3; chapter 11)
pub enum TypeAst {
    /// a primitive type by name
    Prim {
        /// the name
        name: String,
        /// the source range
        loc: Option<Loc>,
    },
    /// a literal type
    Lit {
        /// the literal
        v: Value,
        /// the source range
        loc: Option<Loc>,
    },
    /// a numeric range
    Range {
        /// the lower bound
        lo: Value,
        /// the upper bound
        hi: Value,
        /// whether the upper bound is excluded
        excl: bool,
        /// the source range
        loc: Option<Loc>,
    },
    /// a string pattern
    Pattern {
        /// the pattern's text
        re: String,
        /// the source range
        loc: Option<Loc>,
    },
    /// a record type
    Record {
        /// its members
        members: Vec<MemberAst>,
        /// whether it is open (`...`)
        open: bool,
        /// the source range
        loc: Option<Loc>,
    },
    /// a map type
    Map {
        /// the key type
        key: Box<TypeAst>,
        /// the value type
        val: Box<TypeAst>,
        /// the source range
        loc: Option<Loc>,
    },
    /// an array type
    Array {
        /// the element type
        elem: Box<TypeAst>,
        /// the lower size bound
        lo: Option<Value>,
        /// the upper size bound
        hi: Option<Value>,
        /// whether the upper bound is excluded
        excl: bool,
        /// the source range
        loc: Option<Loc>,
    },
    /// a union
    Union {
        /// its arms
        arms: Vec<TypeAst>,
        /// the source range
        loc: Option<Loc>,
    },
    /// an intersection
    Isect {
        /// its arms
        arms: Vec<TypeAst>,
        /// the source range
        loc: Option<Loc>,
    },
    /// a function type
    Func {
        /// the parameter types
        params: Vec<TypeAst>,
        /// the return type
        ret: Box<TypeAst>,
        /// the source range
        loc: Option<Loc>,
    },
    /// a named type, with its arguments, predicates, and extension
    Named {
        /// the name
        name: String,
        /// its type arguments
        args: Vec<TypeAst>,
        /// its predicates, when refined
        preds: Option<Vec<Rc<Expr>>>,
        /// its extension (`{ … }`)
        ext: Option<Box<TypeAst>>,
        /// the source range
        loc: Option<Loc>,
    },
}
impl TypeAst {
    /// The source range.
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
    /// Record the source range.
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
/// a member of a record type (§5)
pub enum MemberAst {
    /// a value member: required, optional, or defaulted
    Value {
        /// the name
        name: String,
        /// optional (`?`)
        opt: bool,
        /// the type
        ty: TypeAst,
        /// the default, for a defaulted member
        dflt: Option<Rc<Expr>>,
        /// the annotations (§5.10)
        annotations: Vec<Annotation>,
        /// the source range
        loc: Option<Loc>,
    },
    /// `hidden`: `x$ = e` — computed for the schema's own use, never part of the value (D34)
    Derived {
        /// the name
        name: String,
        /// the annotation, when given
        ty: Option<TypeAst>,
        /// the expression
        expr: Rc<Expr>,
        /// hidden (`x$ = e`)
        hidden: bool,
        /// the annotations (§5.10)
        annotations: Vec<Annotation>,
        /// the source range
        loc: Option<Loc>,
    },
    /// a context declaration (`$parent: ref<P>`, §7.3)
    Context {
        /// the variable
        variable: String,
        /// the declared type
        ty: TypeAst,
        /// the annotations (§5.10)
        annotations: Vec<Annotation>,
        /// the source range
        loc: Option<Loc>,
    },
    /// an assertion
    Assert {
        /// the name
        name: String,
        /// the condition
        cond: Rc<Expr>,
        /// the `else` tail
        tail: Option<Tail>,
        /// the annotations (§5.10)
        annotations: Vec<Annotation>,
        /// the source range
        loc: Option<Loc>,
    },
    /// a guarded group of members (`when`)
    When {
        /// the condition
        cond: Rc<Expr>,
        /// the members
        body: Vec<MemberAst>,
        /// the annotations (§5.10)
        annotations: Vec<Annotation>,
        /// the source range
        loc: Option<Loc>,
    },
}
impl MemberAst {
    /// The source range.
    pub fn loc(&self) -> Option<Loc> {
        match self {
            MemberAst::Value { loc, .. }
            | MemberAst::Derived { loc, .. }
            | MemberAst::Context { loc, .. }
            | MemberAst::Assert { loc, .. }
            | MemberAst::When { loc, .. } => *loc,
        }
    }
    /// Record the source range.
    pub fn set_loc(&mut self, l: Loc) {
        match self {
            MemberAst::Value { loc, .. }
            | MemberAst::Derived { loc, .. }
            | MemberAst::Context { loc, .. }
            | MemberAst::Assert { loc, .. }
            | MemberAst::When { loc, .. } => *loc = Some(l),
        }
    }
    /// Attach the annotations (§5.10).
    pub fn set_annotations(&mut self, a: Vec<Annotation>) {
        match self {
            MemberAst::Value { annotations, .. }
            | MemberAst::Derived { annotations, .. }
            | MemberAst::Context { annotations, .. }
            | MemberAst::Assert { annotations, .. }
            | MemberAst::When { annotations, .. } => *annotations = a,
        }
    }
    /// the annotations (§5.10)
    pub fn annotations(&self) -> &[Annotation] {
        match self {
            MemberAst::Value { annotations, .. }
            | MemberAst::Derived { annotations, .. }
            | MemberAst::Context { annotations, .. }
            | MemberAst::Assert { annotations, .. }
            | MemberAst::When { annotations, .. } => annotations,
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
/// a part of a template: text, or an interpolated expression
pub enum TPart {
    /// text
    Text(String),
    /// an interpolated expression
    Expr(Rc<Expr>),
}

#[derive(Debug, Clone)]
/// an `else` tail: an inline message with a severity, or a diagnostic reference
pub enum Tail {
    /// an inline message
    Inline {
        /// the severity
        severity: String,
        /// the message template
        template: Vec<TPart>,
    },
    /// a diagnostic reference
    Ref {
        /// the diagnostic's name
        name: String,
        /// its arguments
        args: Vec<Rc<Expr>>,
    },
}

#[derive(Debug, Clone)]
/// a comprehension clause
pub struct ForClause {
    /// the variable
    pub v: String,
    /// what it ranges over
    pub iter: Rc<Expr>,
    /// the `if` filters
    pub filters: Vec<Rc<Expr>>,
}

#[derive(Debug, Clone)]
/// an arm of a `match`
pub struct MatchArm {
    /// the variable
    pub v: String,
    /// the type it matches; none for the catch-all
    pub ty: Option<TypeAst>,
    /// the body
    pub body: Rc<Expr>,
}

#[derive(Debug, Clone)]
/// an expression (§4)
pub enum Expr {
    /// a literal
    Lit(Value),
    /// a unit literal
    UnitLit {
        /// the number
        num: f64,
        /// the unit
        unit: String,
    },
    /// a template string
    Template(Vec<TPart>),
    /// a name
    Name(String),
    /// a context variable (`$this`, `$parent`, `$root`, `$key`)
    Ctx(String),
    /// `$referrers(T, "m")`
    Referrers {
        /// the referring type
        ty: String,
        /// its member
        member: String,
    },
    /// a record literal
    Obj(Vec<(String, Rc<Expr>)>),
    /// an array literal: (spread, item)
    Arr(Vec<(bool, Rc<Expr>)>),
    /// an array comprehension
    Comp {
        /// the head
        head: Rc<Expr>,
        /// the clauses
        clauses: Vec<ForClause>,
    },
    /// a map comprehension
    MapComp {
        /// the key
        key: Rc<Expr>,
        /// the value
        val: Rc<Expr>,
        /// the clauses
        clauses: Vec<ForClause>,
    },
    /// a binary operation
    Bin {
        /// the operator
        op: String,
        /// the left operand
        l: Rc<Expr>,
        /// the right operand
        r: Rc<Expr>,
    },
    /// a unary operation
    Un {
        /// the operator
        op: String,
        /// the operand
        x: Rc<Expr>,
    },
    /// a parenthesized expression
    Paren(Rc<Expr>),
    /// `if … then … else`
    If {
        /// the condition
        c: Rc<Expr>,
        /// the `then` branch
        t: Rc<Expr>,
        /// the `else` branch
        f: Rc<Expr>,
    },
    /// a function literal
    Lambda {
        /// the parameters
        params: Vec<String>,
        /// the body
        body: Rc<Expr>,
    },
    /// a call
    Call {
        /// the function
        fun: Rc<Expr>,
        /// the arguments
        args: Vec<Rc<Expr>>,
    },
    /// a member access
    Member {
        /// the record
        x: Rc<Expr>,
        /// the member
        name: String,
        /// `?.`
        safe: bool,
    },
    /// an index or key access
    Index {
        /// the array or map
        x: Rc<Expr>,
        /// the index or key
        i: Rc<Expr>,
    },
    /// a record update (`with`)
    With {
        /// the base
        base: Rc<Expr>,
        /// the patch
        patch: Rc<Expr>,
    },
    /// a pattern literal
    Pattern(String),
    /// a `match`
    Match {
        /// the subject
        subject: Rc<Expr>,
        /// the arms
        arms: Vec<MatchArm>,
    },
}

#[derive(Debug, Clone)]
/// an annotation (§5.10): `@name` or `@name(args)` — metadata only (D4)
pub struct Annotation {
    /// the name
    pub name: String,
    /// the arguments
    pub args: Vec<Rc<Expr>>,
    /// the source range
    pub loc: Option<Loc>,
}

#[derive(Debug, Clone)]
/// a parameter of a function, a type, or a diagnostic
pub struct Param {
    /// the name
    pub name: String,
    /// the type, when annotated
    pub ty: Option<TypeAst>,
}

#[derive(Debug, Clone)]
/// an imported name, possibly renamed
pub struct ImportItem {
    /// the name
    pub name: String,
    /// the alias
    pub alias: Option<String>,
}

// `ret` is lowered for fidelity with the reference AST; the runtime does
// not check return annotations (that is the static checker's job)
#[allow(dead_code)]
#[derive(Debug, Clone)]
/// a declaration (§5, §8)
pub enum DeclBody {
    /// a type
    Type {
        /// the name
        name: String,
        /// the type parameters
        params: Vec<Param>,
        /// the type
        ty: TypeAst,
        /// its `else` tail
        tail: Option<Tail>,
    },
    /// a constant
    Const {
        /// the name
        name: String,
        /// the annotation, when given
        ty: Option<TypeAst>,
        /// the expression
        expr: Rc<Expr>,
    },
    /// a function
    Func {
        /// the name
        name: String,
        /// the parameters
        params: Vec<Param>,
        /// the return type, when annotated
        ret: Option<TypeAst>,
        /// the body
        body: Rc<Expr>,
    },
    /// an output root
    Output {
        /// the name
        name: String,
        /// the type
        ty: TypeAst,
        /// the expression
        expr: Rc<Expr>,
    },
    /// an input root
    Input {
        /// the name
        name: String,
        /// the type
        ty: TypeAst,
        /// the fallback, when given
        fallback: Option<Rc<Expr>>,
    },
    /// a diagnostic template
    Diagnostic {
        /// the name
        name: String,
        /// the parameters
        params: Vec<Param>,
        /// the severity
        severity: String,
        /// the message template
        template: Vec<TPart>,
    },
    /// a dimension
    Dimension {
        /// the name
        name: String,
        /// its definition as (dimension, exponent) terms; none for a base dimension
        terms: Option<Vec<(String, i32)>>,
    },
    /// a unit
    Unit {
        /// the name
        name: String,
        /// its dimension
        dim: Option<String>,
        /// its factor against the base unit
        factor: Option<Rc<Expr>>,
        /// the base unit it is defined against
        base: Option<String>,
    },
    /// an import
    Import {
        /// the module
        from: String,
        /// the names imported; none for a namespace import
        names: Option<Vec<ImportItem>>,
        /// the namespace, for `import * as ns`
        ns: Option<String>,
    },
    /// a re-export
    ReExport {
        /// the module
        from: String,
        /// the names
        names: Vec<ImportItem>,
    },
}

#[derive(Debug, Clone)]
/// a declaration, with whether it is exported and its source range
pub struct Decl {
    /// the declaration
    pub body: DeclBody,
    /// `export`
    pub exported: bool,
    /// the annotations (§5.10)
    pub annotations: Vec<Annotation>,
    /// the declaration's source range (Phase 6 foundations); `export` included
    pub loc: Option<Loc>,
}

impl Decl {
    /// The declared name, when the declaration has one.
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

/// A short name for an expression, for messages.
pub fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Name(n) => n.clone(),
        Expr::Call { fun, .. } => expr_name(fun),
        _ => "<predicate>".into(),
    }
}
