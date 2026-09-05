//! Value model, environment, and type resolution — a port of the
//! reference implementation's semantics.ts.
use crate::ast::*;
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::LazyLock;

// the pattern-interpolation grammar (§3.6): compiled once
static PATTERN_HOLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{([^}]*)\}").unwrap());
static PATTERN_STR_LIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^"((?:[^"\\]|\\.)*)"$"#).unwrap());
static PATTERN_INT_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(-?[0-9]+)\.\.(<?)(-?[0-9]+)$").unwrap());
static PATTERN_INT_LIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?[0-9]+$").unwrap());
static PATTERN_IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*$").unwrap());

// ---------------- paths ----------------
#[derive(Clone, Debug, PartialEq)]
/// a segment of a canonical path (§7.2)
pub enum Seg {
    /// a record member by name: dotted when the dot can spell it (§7.2)
    Name(String),
    /// an array index
    Idx(usize),
    /// a map key: always bracketed (§7.2)
    Key(String),
}
/// a canonical path
pub type SegPath = Vec<Seg>;
/// A segment's text as the path spells it.
pub fn seg_text(s: &Seg) -> String {
    match s {
        Seg::Name(n) | Seg::Key(n) => n.clone(),
        Seg::Idx(i) => i.to_string(),
    }
}
/// dot-spellable (§3.11, §4.3): identifier-shaped and not a literal keyword
pub fn dot_spellable(name: &str) -> bool {
    let mut cs = name.chars();
    let head = matches!(cs.next(), Some(c) if c == '_' || c.is_ascii_alphabetic());
    head && cs.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && !matches!(name, "true" | "false" | "null")
}

// ---------------- values ----------------
#[derive(Clone)]
/// a runtime value (§9): the scalars, quantities, references, and instances,
/// and the engine's intermediate forms
pub enum Value {
    /// an integer
    Int(BigInt),
    /// a float
    Float(f64),
    /// a string
    Str(String),
    /// a boolean
    Bool(bool),
    /// null
    Null,
    /// an absent optional member (§4.6)
    Absent,
    /// not yet computed
    Undef,
    /// a quantity
    Q {
        /// its dimension key
        dim: String,
        /// its value in the base unit
        value: f64,
    },
    /// a reference, by canonical path
    Ref(Rc<SegPath>),
    /// a record instance
    Rec(Rc<RefCell<RecInst>>),
    /// an array
    Arr(Rc<RefCell<ArrV>>),
    /// a map
    Map(Rc<RefCell<MapV>>),
    /// a range value
    Range {
        /// the lower bound
        lo: Box<Value>,
        /// the upper bound
        hi: Box<Value>,
        /// whether the upper bound is excluded
        excl: bool,
    },
    /// a closure
    Clo(Rc<Closure>),
    /// a native function
    Nat(NatFn),
    /// a standard-library path, partially spelled
    Std(Rc<Vec<String>>),
    /// a namespace
    NsRef(Rc<NsRefV>),
    /// a pattern
    Pat(String),
    /// a record literal not yet bound
    PreObj(Rc<Vec<(String, Value)>>),
    /// an array literal not yet bound: (spread, item)
    PreArr(Rc<Vec<(bool, Value)>>),
    /// an expression not yet evaluated, with its scope
    PreVal(Rc<PreValV>),
    /// a JSON object, as read
    JObj(Rc<Vec<(String, Value)>>),
    /// a JSON array, as read
    JArr(Rc<Vec<Value>>),
    /// a path value
    Segs(Rc<SegPath>),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Null => write!(f, "null"),
            Value::Absent => write!(f, "ABSENT"),
            Value::Undef => write!(f, "UNDEF"),
            Value::Q { dim, value } => write!(f, "{value}<{dim}>"),
            other => write!(f, "<{}>", other.tag()),
        }
    }
}

impl Value {
    /// The value's kind, as a word.
    pub fn tag(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Absent => "absent",
            Value::Undef => "undef",
            Value::Q { .. } => "quantity",
            Value::Ref(_) => "ref",
            Value::Rec(_) => "record",
            Value::Arr(_) => "array",
            Value::Map(_) => "map",
            Value::Range { .. } => "range",
            Value::Clo(_) => "closure",
            Value::Nat(_) => "native",
            Value::Std(_) => "std",
            Value::NsRef(_) => "namespace",
            Value::Pat(_) => "pattern",
            Value::PreObj(_) => "pre-obj",
            Value::PreArr(_) => "pre-arr",
            Value::PreVal(_) => "pre-val",
            Value::JObj(_) => "json-obj",
            Value::JArr(_) => "json-arr",
            Value::Segs(_) => "segs",
        }
    }
    /// Whether the value is not yet computed.
    pub fn is_undef(&self) -> bool {
        matches!(self, Value::Undef)
    }
    /// Whether the value is absent.
    pub fn is_absent(&self) -> bool {
        matches!(self, Value::Absent)
    }
    /// The canonical path the value sits at or refers to, when it has one.
    pub fn place(&self) -> Option<SegPath> {
        match self {
            Value::Ref(p) => Some((**p).clone()),
            Value::Rec(r) => Some(r.borrow().path.clone()),
            Value::Arr(a) => Some(a.borrow().path.clone()),
            Value::Map(m) => Some(m.borrow().path.clone()),
            _ => None,
        }
    }
}

/// a native function
pub type NatFn = Rc<dyn Fn(&[Value]) -> R<Value>>;

/// a function value: its parameters, its body, the scope it closed over
pub struct Closure {
    /// the parameters
    pub params: Vec<String>,
    /// the body
    pub body: Rc<Expr>,
    /// the scope
    pub scope: Scope,
}
/// a namespace's exports
pub struct NsRefV {
    /// the exports, by name
    pub exports: Rc<RefCell<HashMap<String, Export>>>,
}
/// an expression waiting to be evaluated in its scope
pub struct PreValV {
    /// the expression
    pub expr: Rc<Expr>,
    /// the scope
    pub scope: Scope,
}
/// an array value
pub struct ArrV {
    /// the items
    pub items: Vec<Value>,
    /// its canonical path
    pub path: SegPath,
}
/// a map value
pub struct MapV {
    /// the entries, in order
    pub entries: Vec<(String, Value)>,
    /// its canonical path
    pub path: SegPath,
}
impl MapV {
    /// The value at a key.
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.entries.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    /// Whether the key is present.
    pub fn has(&self, k: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == k)
    }
    /// Set a key's value.
    pub fn set(&mut self, k: String, v: Value) {
        if let Some(e) = self.entries.iter_mut().find(|(n, _)| *n == k) {
            e.1 = v;
        } else {
            self.entries.push((k, v));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
/// a member's kind (§5)
pub enum MKind {
    /// required
    Req,
    /// optional
    Opt,
    /// defaulted
    Dflt,
    /// derived
    Der,
}
#[derive(Clone, Copy, PartialEq, Debug)]
/// the state of a slot
pub enum SlotState {
    /// not forced yet
    Unforced,
    /// being forced
    Forcing,
    /// forced, with a value
    Ok,
    /// an error invalidated it
    Invalid,
    /// absent
    Absent,
}

#[derive(Clone)]
/// how a slot computes its value
pub enum Compute {
    /// a supplied value checked against the types
    Check {
        /// the value supplied
        raw: Value,
        /// the types to check against
        types: Vec<RT>,
        /// the member
        name: String,
        /// the root
        root_name: String,
        /// the module environment
        menv: Option<Rc<Env>>,
    },
    /// a default expression
    Default {
        /// the expression
        expr: Rc<Expr>,
        /// the types to check against
        types: Vec<RT>,
        /// the member
        name: String,
        /// the root
        root_name: String,
        /// the module environment
        menv: Option<Rc<Env>>,
    },
    /// a derived expression
    Derived {
        /// the expression
        expr: Rc<Expr>,
        /// the declared type
        ty: Option<RT>,
        /// the value the document supplied, to compare (§10.5)
        supplied: Option<Value>,
        /// the member
        name: String,
        /// the root
        root_name: String,
        /// the module environment
        menv: Option<Rc<Env>>,
    },
}

/// a member's slot in an instance
pub struct Slot {
    /// the member's kind
    pub kind: MKind,
    /// `x$ = e`: computed, never part of the value (D34)
    pub hidden: bool,
    /// its state
    pub state: SlotState,
    /// its value, once forced
    pub value: Value,
    /// how it computes
    pub compute: Option<Compute>,
}

/// a record instance: its type, its path, its slots
pub struct RecInst {
    /// the type's name, when named
    pub type_name: Option<String>,
    /// the type
    pub rt: RT,
    /// its canonical path
    pub path: SegPath,
    /// the enclosing instance
    pub parent: Option<Rc<RefCell<RecInst>>>,
    // declaration order matters (forcing order drives diagnostic order)
    /// the slots, in declaration order
    pub slots: Vec<(String, Slot)>,
    /// the order the document supplied the members
    pub entry_order: Vec<String>,
    /// members of an open record beyond its type
    pub extras: Vec<(String, Value)>,
    /// the module environment
    pub menv: Option<Rc<Env>>,
}
impl RecInst {
    /// An extra member's value.
    pub fn extra(&self, n: &str) -> Option<&Value> {
        self.extras.iter().find(|(k, _)| k == n).map(|(_, v)| v)
    }
    /// Set an extra member.
    pub fn set_extra(&mut self, n: &str, v: Value) {
        if let Some(e) = self.extras.iter_mut().find(|(k, _)| k == n) {
            e.1 = v;
        } else {
            self.extras.push((n.to_string(), v));
        }
    }
    /// A slot by name.
    pub fn slot(&self, n: &str) -> Option<&Slot> {
        self.slots.iter().find(|(k, _)| k == n).map(|(_, s)| s)
    }
    /// A slot by name, mutably.
    pub fn slot_mut(&mut self, n: &str) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|(k, _)| k == n).map(|(_, s)| s)
    }
    /// Whether the slot exists.
    pub fn has_slot(&self, n: &str) -> bool {
        self.slots.iter().any(|(k, _)| k == n)
    }
}

#[derive(Clone)]
/// an evaluation scope: the enclosing instance, the local variables, the root, the module environment
pub struct Scope {
    /// the enclosing instance
    pub inst: Option<Rc<RefCell<RecInst>>>,
    /// the local variables
    pub locals: Rc<HashMap<String, Value>>,
    /// the root
    pub root_name: String,
    /// the module environment
    pub menv: Option<Rc<Env>>,
}
impl Scope {
    /// A scope at a root.
    pub fn new(root_name: &str, menv: Option<Rc<Env>>) -> Scope {
        Scope {
            inst: None,
            locals: Rc::new(HashMap::new()),
            root_name: root_name.to_string(),
            menv,
        }
    }
    /// The scope with local variables.
    pub fn with_locals(&self, locals: HashMap<String, Value>) -> Scope {
        Scope {
            inst: self.inst.clone(),
            locals: Rc::new(locals),
            root_name: self.root_name.clone(),
            menv: self.menv.clone(),
        }
    }
    /// The scope inside an instance.
    pub fn with_inst(&self, inst: Option<Rc<RefCell<RecInst>>>) -> Scope {
        Scope {
            inst,
            locals: self.locals.clone(),
            root_name: self.root_name.clone(),
            menv: self.menv.clone(),
        }
    }
    /// The scope in a module environment.
    pub fn with_menv(&self, menv: Option<Rc<Env>>) -> Scope {
        Scope {
            inst: self.inst.clone(),
            locals: self.locals.clone(),
            root_name: self.root_name.clone(),
            menv,
        }
    }
}

// ---------------- failures & diagnostics ----------------
/// an evaluation error, with its code when it has one
pub struct EvalErr {
    /// the message
    pub msg: String,
    /// the code
    pub code: Option<String>,
}
/// why an evaluation stopped
pub enum Fail {
    /// it read an invalid value
    Taint,
    /// it must wait for phase 2
    Defer,
    /// an error
    Eval(EvalErr),
}
/// an evaluation's outcome
pub type R<T> = Result<T, Fail>;
/// An evaluation error without a code.
pub fn err<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail::Eval(EvalErr {
        msg: msg.into(),
        code: None,
    }))
}
/// An evaluation error with a code.
pub fn err_code<T>(msg: impl Into<String>, code: &str) -> R<T> {
    Err(Fail::Eval(EvalErr {
        msg: msg.into(),
        code: Some(code.to_string()),
    }))
}

#[derive(Clone, Debug)]
/// a diagnostic (§6, §12)
pub struct Diag {
    /// `error`, `warn`, or `info`
    pub severity: String,
    /// the constraint's stable id, for an assertion
    pub id: Option<String>,
    /// the message, rendered
    pub message: String,
    /// the canonical path
    pub path: String,
    /// the code (§12)
    pub code: Option<String>,
    /// the source range the checker reported at (the declaration, or the expression under inference)
    pub loc: Option<Loc>,
    /// the evaluation step that produced it (a slot, a root, an assert): dependency tracking's tag
    pub by: Option<String>,
}
impl Diag {
    /// An error diagnostic at a path.
    pub fn error(message: impl Into<String>, path: String, code: Option<&str>) -> Diag {
        Diag {
            severity: "error".into(),
            id: None,
            message: message.into(),
            path,
            code: code.map(|c| c.to_string()),
            loc: None,
            by: None,
        }
    }
    /// The diagnostic as a JSON object in the report's field order (§12.2), with the file when given.
    pub fn to_json(&self, file: Option<&str>) -> String {
        let mut parts = Vec::new();
        // the report's field order (§12.2): file, code, id, severity,
        // message, path — absent fields omitted (byte-identical across
        // implementations)
        if let Some(f) = file {
            parts.push(format!("\"file\":{}", json_str(f)));
        }
        if let Some(c) = &self.code {
            parts.push(format!("\"code\":{}", json_str(c)));
        }
        if let Some(id) = &self.id {
            parts.push(format!("\"id\":{}", json_str(id)));
        }
        parts.push(format!("\"severity\":{}", json_str(&self.severity)));
        parts.push(format!("\"message\":{}", json_str(&self.message)));
        parts.push(format!("\"path\":{}", json_str(&self.path)));
        format!("{{{}}}", parts.join(","))
    }
}

// ---------------- resolved types ----------------
/// a resolved type, shared
pub type RT = Rc<Ty>;

/// a resolved type: its kind, its name when named, its `else` tail
pub struct Ty {
    /// the kind
    pub k: RTk,
    /// the name, for a named type
    pub name: RefCell<Option<String>>,
    /// the `else` tail
    pub tail: RefCell<Option<Tail>>,
}
/// A resolved type of a kind.
pub fn ty(k: RTk) -> RT {
    Rc::new(Ty {
        k,
        name: RefCell::new(None),
        tail: RefCell::new(None),
    })
}

/// the kinds of resolved types (§3)
pub enum RTk {
    /// a primitive
    Prim(String),
    /// a literal
    Lit(Value),
    /// a numeric range
    Range {
        /// the lower bound
        lo: Value,
        /// the upper bound
        hi: Value,
        /// whether the upper bound is excluded
        excl: bool,
        /// int or float
        base: String,
    },
    /// a string pattern
    Pattern {
        /// its text
        src: String,
        /// compiled
        re: Regex,
    },
    /// an array
    Arr {
        /// the element type
        elem: RT,
        /// the lower size bound
        lo: Option<i64>,
        /// the upper size bound
        hi: Option<i64>,
    },
    /// a map
    Map {
        /// the key type
        key: RT,
        /// the value type
        val: RT,
    },
    /// a union
    Union(Vec<RT>),
    /// an intersection not yet merged
    IsectN(Vec<RT>),
    /// a record
    Rec(RecType),
    /// a predicate refinement
    Pred {
        /// the base type
        base: RT,
        /// the predicates
        preds: Vec<Rc<Expr>>,
    },
    /// a reference type
    Ref(RT),
    /// a quantity of a dimension
    Quantity(String),
    /// a function type
    Func {
        /// the parameter types
        params: Vec<RT>,
        /// the return type
        ret: RT,
    },
    /// any value
    Any,
}

/// a record type's shape
pub struct RecType {
    /// whether it is open
    pub open: Cell<bool>,
    /// its members
    pub members: RefCell<Vec<Member>>,
    /// its assertions and guarded groups
    pub asserts: RefCell<Vec<AssertItem>>,
    /// `context $parent: ref<T>` declarations (D30), checked at embedding sites
    pub ctx_decls: RefCell<Vec<(String, RT)>>,
    /// still being filled; extensions of it wait in `pending` (§3.14)
    pub filling: Cell<bool>,
    /// extensions not yet merged (recursive types)
    pub pending: RefCell<Vec<(RT, RT)>>,
}
/// An empty record type.
pub fn rec_type(open: bool) -> RecType {
    RecType {
        open: Cell::new(open),
        members: RefCell::new(vec![]),
        asserts: RefCell::new(vec![]),
        ctx_decls: RefCell::new(vec![]),
        filling: Cell::new(false),
        pending: RefCell::new(vec![]),
    }
}

#[derive(Clone)]
/// a member of a resolved record type
pub struct Member {
    /// the kind
    pub kind: MKind,
    /// the name
    pub name: String,
    /// a hidden member (D34): computed, never part of the value
    pub hidden: bool,
    /// the type
    pub ty: Option<RT>,
    /// the types a conjunction contributed
    pub conj: Option<Vec<RT>>,
    /// the default
    pub dflt: Option<Rc<Expr>>,
    /// the derived expression
    pub expr: Option<Rc<Expr>>,
    /// the module environment
    pub menv: Option<Rc<Env>>,
}

#[derive(Clone)]
/// an assertion or a guarded group of a record type
pub struct AssertItem {
    /// whether it is a `when` group
    pub when: bool,
    /// the name
    pub name: String,
    /// the condition
    pub cond: Rc<Expr>,
    /// the `else` tail
    pub tail: Option<Tail>,
    /// the members, for a group
    pub body: Vec<MemberAst>,
    /// the type declaring it, for the id
    pub origin: Option<String>,
    /// the module environment
    pub menv: Option<Rc<Env>>,
}

/// The members of a record type; empty for any other type.
pub fn rec_members(t: &RT) -> Vec<Member> {
    match &t.k {
        RTk::Rec(r) => r.members.borrow().clone(),
        _ => vec![],
    }
}
/// Whether the type is a record.
pub fn is_rec(t: &RT) -> bool {
    matches!(t.k, RTk::Rec(_))
}

// ---------------- dimension vectors ----------------
/// a dimension as base dimensions with exponents
pub type DimVec = BTreeMap<String, i32>;
/// The dimension's key (`Length*Time^-1`).
pub fn key_of_vec(v: &DimVec) -> String {
    v.iter()
        .filter(|(_, e)| **e != 0)
        .map(|(n, e)| {
            if *e == 1 {
                n.clone()
            } else {
                format!("{n}^{e}")
            }
        })
        .collect::<Vec<_>>()
        .join("*")
}
/// A dimension from its key.
pub fn vec_of_key(key: &str) -> DimVec {
    let mut v = DimVec::new();
    if key.is_empty() {
        return v;
    }
    for p in key.split('*') {
        let (n, e) = match p.split_once('^') {
            Some((n, e)) => (n.to_string(), e.parse::<i32>().unwrap_or(1)),
            None => (p.to_string(), 1),
        };
        *v.entry(n).or_insert(0) += e;
    }
    v
}
/// Two dimensions multiplied (`sign` 1) or divided (`sign` -1).
pub fn vec_combine(a: &DimVec, b: &DimVec, sign: i32) -> DimVec {
    let mut out = a.clone();
    for (n, e) in b {
        *out.entry(n.clone()).or_insert(0) += sign * e;
    }
    out
}

// ---------------- environment ----------------
/// an exported name: the environment declaring it
pub struct Export {
    /// the environment
    pub env: Rc<Env>,
    /// the name there
    pub name: String,
}
impl Clone for Export {
    fn clone(&self) -> Self {
        Export {
            env: self.env.clone(),
            name: self.name.clone(),
        }
    }
}
/// a constant's declaration and memoized value
pub struct ConstEntry {
    /// the expression
    pub expr: Rc<Expr>,
    /// the annotation
    pub ty: Option<TypeAst>,
    /// whether the value is computed
    pub state: Cell<bool>,
    /// the value
    pub value: RefCell<Value>,
}
/// a function's declaration
pub struct FuncEntry {
    /// the parameters
    pub params: Vec<Param>,
    /// the return type
    pub ret: Option<TypeAst>,
    /// the body
    pub body: Rc<Expr>,
}
/// a type's declaration
pub struct TypeEntry {
    /// the type
    pub ast: TypeAst,
    /// its `else` tail
    pub tail: Option<Tail>,
    /// the type parameters
    pub params: Vec<Param>,
}
/// a diagnostic template's declaration
pub struct DiagDecl {
    /// the parameters
    pub params: Vec<Param>,
    /// the severity
    pub severity: String,
    /// the template
    pub template: Vec<TPart>,
}
/// a unit's declaration
pub struct UnitDecl {
    /// its dimension
    pub dim: Option<String>,
    /// its factor
    pub factor: Option<Rc<Expr>>,
    /// its base unit
    pub base: Option<String>,
}
/// evaluates a constant by name (the engine's)
pub type ConstEval = Rc<dyn Fn(&str) -> R<Value>>;
/// evaluates an expression (the engine's)
pub type ExprEval = Rc<dyn Fn(&Rc<Expr>) -> R<Value>>;

/// the environment of a module: its declarations, its imports, its roots and
/// diagnostics, the unit space
pub struct Env {
    /// the type declarations
    pub type_asts: RefCell<HashMap<String, Rc<TypeEntry>>>,
    /// resolved types, memoized
    pub type_memo: RefCell<HashMap<String, RT>>,
    // names being spliced into a pattern right now, across nested
    // resolutions — a mutually recursive pair is a cycle, not a stack overflow
    /// the named types being resolved (recursion)
    pub pattern_visiting: RefCell<Vec<String>>,
    /// the constants
    pub consts: RefCell<HashMap<String, Rc<ConstEntry>>>,
    /// the functions
    pub funcs: RefCell<HashMap<String, Rc<FuncEntry>>>,
    /// names declared twice
    pub duplicates: RefCell<Vec<String>>,
    /// the outputs: name, type, expression
    pub outputs: RefCell<Vec<(String, TypeAst, Rc<Expr>)>>,
    /// the inputs: type, fallback
    pub inputs: RefCell<HashMap<String, (TypeAst, Option<Rc<Expr>>)>>,
    /// the diagnostic templates
    pub diags: RefCell<HashMap<String, Rc<DiagDecl>>>,
    /// every instance bound
    pub registry: RefCell<Rc<RefCell<Vec<Rc<RefCell<RecInst>>>>>>,
    /// the roots' values, by name
    pub roots: RefCell<Rc<RefCell<Vec<(String, Value)>>>>,
    /// the diagnostics raised
    pub diagnostics: RefCell<Rc<RefCell<Vec<Diag>>>>,
    /// the constant evaluator, once an engine is wired in
    pub const_eval: RefCell<Option<ConstEval>>,
    /// the expression evaluator, once an engine is wired in
    pub expr_eval: RefCell<Option<ExprEval>>,
    /// the imported names
    pub imports: RefCell<HashMap<String, Export>>,
    /// the namespace imports
    pub namespaces: RefCell<HashMap<String, (Rc<Env>, Rc<RefCell<HashMap<String, Export>>>)>>,
    const_diag_seen: RefCell<HashSet<String>>,
    /// the dimension declarations
    pub dim_decls: RefCell<HashMap<String, Option<Vec<(String, i32)>>>>,
    /// dimensions resolved, memoized
    pub dim_memo: RefCell<HashMap<String, DimVec>>,
    /// the unit declarations
    pub unit_decls: RefCell<HashMap<String, UnitDecl>>,
    /// units resolved to (dimension, factor), memoized
    pub unit_memo: RefCell<HashMap<String, (String, f64)>>,
    /// the base unit of each dimension
    pub base_unit_of: RefCell<HashMap<String, String>>,
    /// the unit space's diagnostics (E4073 and friends)
    pub space_diags: RefCell<Vec<Diag>>,
    /// declaration order (HashMaps do not keep it; diagnostics follow it)
    pub type_order: RefCell<Vec<String>>,
    /// the units in declaration order
    pub unit_order: RefCell<Vec<String>>,
    /// installed by the checker: constant-evaluation errors go here instead of the report
    pub const_diag_sink: RefCell<Option<Rc<RefCell<Vec<Diag>>>>>,
    /// installed by the engine: the evaluation step a report is attributed to
    pub tagger: RefCell<Option<Rc<dyn Fn() -> Option<String>>>>,
}

/// §6.7: evaluation- and validation-time diagnostics sort by (path, id), path in canonical order; stable
pub fn sort_diags(diags: Vec<Diag>) -> Vec<Diag> {
    let segs_of = |p: &str| -> SegPath {
        if p.is_empty() {
            return vec![];
        }
        parse_path(p, "").unwrap_or_else(|_| vec![Seg::Name(p.to_string())])
    };
    let mut keyed: Vec<(usize, SegPath, Diag)> = diags
        .into_iter()
        .enumerate()
        .map(|(i, d)| (i, segs_of(&d.path), d))
        .collect();
    keyed.sort_by(|a, b| {
        cmp_path(&a.1, &b.1)
            .then_with(|| {
                a.2.id
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.2.id.as_deref().unwrap_or(""))
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    keyed.into_iter().map(|(_, _, d)| d).collect()
}

const SI_PREFIXES: [(&str, f64); 20] = [
    ("y", 1e-24),
    ("z", 1e-21),
    ("a", 1e-18),
    ("f", 1e-15),
    ("p", 1e-12),
    ("n", 1e-9),
    ("u", 1e-6),
    ("m", 1e-3),
    ("c", 1e-2),
    ("d", 1e-1),
    ("da", 1e1),
    ("h", 1e2),
    ("k", 1e3),
    ("M", 1e6),
    ("G", 1e9),
    ("T", 1e12),
    ("P", 1e15),
    ("E", 1e18),
    ("Z", 1e21),
    ("Y", 1e24),
];

impl Env {
    /// An empty environment.
    pub fn new() -> Rc<Env> {
        let env = Env {
            type_asts: RefCell::new(HashMap::new()),
            type_memo: RefCell::new(HashMap::new()),
            pattern_visiting: RefCell::new(vec![]),
            consts: RefCell::new(HashMap::new()),
            funcs: RefCell::new(HashMap::new()),
            duplicates: RefCell::new(vec![]),
            outputs: RefCell::new(vec![]),
            inputs: RefCell::new(HashMap::new()),
            diags: RefCell::new(HashMap::new()),
            registry: RefCell::new(Rc::new(RefCell::new(vec![]))),
            roots: RefCell::new(Rc::new(RefCell::new(vec![]))),
            diagnostics: RefCell::new(Rc::new(RefCell::new(vec![]))),
            const_eval: RefCell::new(None),
            expr_eval: RefCell::new(None),
            imports: RefCell::new(HashMap::new()),
            namespaces: RefCell::new(HashMap::new()),
            const_diag_seen: RefCell::new(HashSet::new()),
            dim_decls: RefCell::new(HashMap::new()),
            dim_memo: RefCell::new(HashMap::new()),
            unit_decls: RefCell::new(HashMap::new()),
            unit_memo: RefCell::new(HashMap::new()),
            base_unit_of: RefCell::new(HashMap::new()),
            space_diags: RefCell::new(vec![]),
            type_order: RefCell::new(vec![]),
            unit_order: RefCell::new(vec![]),
            const_diag_sink: RefCell::new(None),
            tagger: RefCell::new(None),
        };
        env.seed_units();
        Rc::new(env)
    }

    // std.units — the SI catalog generated from the §13.10 prefix rule (D15)
    fn seed_units(&self) {
        let unit = |sym: &str, dim: Option<&str>, factor: f64, base: &str| {
            let mut m = self.unit_decls.borrow_mut();
            if m.contains_key(sym) {
                return;
            }
            self.unit_order.borrow_mut().push(sym.to_string());
            m.insert(
                sym.to_string(),
                match dim {
                    Some(d) => UnitDecl {
                        dim: Some(d.to_string()),
                        factor: None,
                        base: None,
                    },
                    None => UnitDecl {
                        dim: None,
                        factor: Some(Rc::new(Expr::Lit(Value::Float(factor)))),
                        base: Some(base.to_string()),
                    },
                },
            );
        };
        let bases = [
            ("Time", "s"),
            ("Length", "m"),
            ("Mass", "kg"),
            ("Current", "A"),
            ("Temperature", "K"),
            ("Amount", "mol"),
            ("LuminousIntensity", "cd"),
        ];
        for (d, _) in bases {
            self.dim_decls.borrow_mut().insert(d.to_string(), None);
        }
        let t = |n: &str, e: i32| (n.to_string(), e);
        let derived: Vec<(&str, Option<Vec<(String, i32)>>, &str)> = vec![
            ("Frequency", Some(vec![t("Time", -1)]), "Hz"),
            (
                "Force",
                Some(vec![t("Mass", 1), t("Length", 1), t("Time", -2)]),
                "N",
            ),
            (
                "Pressure",
                Some(vec![t("Mass", 1), t("Length", -1), t("Time", -2)]),
                "Pa",
            ),
            (
                "Energy",
                Some(vec![t("Mass", 1), t("Length", 2), t("Time", -2)]),
                "J",
            ),
            (
                "Power",
                Some(vec![t("Mass", 1), t("Length", 2), t("Time", -3)]),
                "W",
            ),
            ("Charge", Some(vec![t("Current", 1), t("Time", 1)]), "C"),
            (
                "Voltage",
                Some(vec![
                    t("Mass", 1),
                    t("Length", 2),
                    t("Time", -3),
                    t("Current", -1),
                ]),
                "V",
            ),
            (
                "Resistance",
                Some(vec![
                    t("Mass", 1),
                    t("Length", 2),
                    t("Time", -3),
                    t("Current", -2),
                ]),
                "Ohm",
            ),
            (
                "Capacitance",
                Some(vec![
                    t("Mass", -1),
                    t("Length", -2),
                    t("Time", 4),
                    t("Current", 2),
                ]),
                "F",
            ),
            ("DataSize", None, "bit"),
        ];
        for (d, terms, _) in &derived {
            self.dim_decls
                .borrow_mut()
                .insert(d.to_string(), terms.clone());
        }
        for (d, s) in bases {
            unit(s, Some(d), 1.0, "");
        }
        for (d, _, s) in &derived {
            unit(s, Some(d), 1.0, "");
        }
        unit("B", None, 8.0, "bit");
        unit("g", None, 1e-3, "kg");
        let mut prefixable: Vec<&str> = bases
            .iter()
            .map(|(_, s)| *s)
            .filter(|s| *s != "kg")
            .collect();
        prefixable.extend(derived.iter().map(|(_, _, s)| *s).filter(|s| *s != "bit"));
        prefixable.push("g");
        for u0 in prefixable {
            for (p, f) in SI_PREFIXES {
                unit(&format!("{p}{u0}"), None, f, u0);
            }
        }
        for u0 in ["bit", "B"] {
            for (p, f) in [
                ("Ki", 1024f64),
                ("Mi", 1024f64.powi(2)),
                ("Gi", 1024f64.powi(3)),
                ("Ti", 1024f64.powi(4)),
                ("Pi", 1024f64.powi(5)),
                ("Ei", 1024f64.powi(6)),
            ] {
                unit(&format!("{p}{u0}"), None, f, u0);
            }
            for (p, f) in SI_PREFIXES {
                if ["k", "M", "G", "T", "P", "E"].contains(&p) {
                    unit(&format!("{p}{u0}"), None, f, u0);
                }
            }
        }
    }

    /// Load declarations into the environment (§5, §8).
    pub fn load(&self, decls: &[Decl]) {
        let mut seen: HashSet<String> = HashSet::new();
        for d in decls {
            if let Some(n) = d.name() {
                if !matches!(d.body, DeclBody::Unit { .. } | DeclBody::Dimension { .. })
                    && !seen.insert(n.to_string())
                {
                    self.duplicates.borrow_mut().push(n.to_string());
                }
            }
            match &d.body {
                DeclBody::Dimension { name, terms } => {
                    if self.dim_decls.borrow().contains_key(name) {
                        self.space_diags.borrow_mut().push(Diag::error(
                            format!("dimension {name} redeclared"),
                            String::new(),
                            Some("E3001"),
                        ));
                    } else {
                        self.dim_decls
                            .borrow_mut()
                            .insert(name.clone(), terms.clone());
                    }
                }
                DeclBody::Unit {
                    name,
                    dim,
                    factor,
                    base,
                } => {
                    if self.unit_decls.borrow().contains_key(name) {
                        self.space_diags.borrow_mut().push(Diag::error(
                            format!("unit {name} redeclared"),
                            String::new(),
                            Some("E4073"),
                        ));
                    } else {
                        self.unit_order.borrow_mut().push(name.clone());
                        self.unit_decls.borrow_mut().insert(
                            name.clone(),
                            UnitDecl {
                                dim: dim.clone(),
                                factor: factor.clone(),
                                base: base.clone(),
                            },
                        );
                    }
                }
                DeclBody::Type {
                    name,
                    params,
                    ty,
                    tail,
                } => {
                    if !self.type_asts.borrow().contains_key(name) {
                        self.type_order.borrow_mut().push(name.clone());
                    }
                    self.type_asts.borrow_mut().insert(
                        name.clone(),
                        Rc::new(TypeEntry {
                            ast: ty.clone(),
                            tail: tail.clone(),
                            params: params.clone(),
                        }),
                    );
                }
                DeclBody::Const { name, ty, expr } => {
                    self.consts.borrow_mut().insert(
                        name.clone(),
                        Rc::new(ConstEntry {
                            expr: expr.clone(),
                            ty: ty.clone(),
                            state: Cell::new(false),
                            value: RefCell::new(Value::Null),
                        }),
                    );
                }
                DeclBody::Func {
                    name,
                    params,
                    ret,
                    body,
                } => {
                    self.funcs.borrow_mut().insert(
                        name.clone(),
                        Rc::new(FuncEntry {
                            params: params.clone(),
                            ret: ret.clone(),
                            body: body.clone(),
                        }),
                    );
                }
                DeclBody::Output { name, ty, expr } => {
                    self.outputs
                        .borrow_mut()
                        .push((name.clone(), ty.clone(), expr.clone()))
                }
                DeclBody::Input { name, ty, fallback } => {
                    self.inputs
                        .borrow_mut()
                        .insert(name.clone(), (ty.clone(), fallback.clone()));
                }
                DeclBody::Diagnostic {
                    name,
                    params,
                    severity,
                    template,
                } => {
                    self.diags.borrow_mut().insert(
                        name.clone(),
                        Rc::new(DiagDecl {
                            params: params.clone(),
                            severity: severity.clone(),
                            template: template.clone(),
                        }),
                    );
                }
                _ => {}
            }
        }
    }

    /// Raise a diagnostic.
    pub fn report(&self, d: Diag) {
        let by = self.tagger.borrow().as_ref().and_then(|t| t());
        let mut d = d;
        if by.is_some() {
            d.by = by;
        }
        self.diagnostics.borrow().borrow_mut().push(d);
    }
    /// replace every diagnostic (the run entry points sort them, §6.7)
    pub fn diag_set(&self, diags: Vec<Diag>) {
        let rc = self.diagnostics.borrow().clone();
        *rc.borrow_mut() = diags;
    }
    /// Forget a root.
    pub fn remove_root(&self, name: &str) {
        let rc = self.roots.borrow().clone();
        rc.borrow_mut().retain(|(n, _)| n != name);
    }
    /// The roots' names, in order.
    pub fn root_names(&self) -> Vec<String> {
        self.roots
            .borrow()
            .borrow()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }
    /// Keep the instances a predicate accepts.
    pub fn registry_retain(&self, mut pred: impl FnMut(&Rc<RefCell<RecInst>>) -> bool) {
        let rc = self.registry.borrow().clone();
        rc.borrow_mut().retain(|i| pred(i));
    }
    /// The diagnostics raised so far.
    pub fn diagnostics_vec(&self) -> Vec<Diag> {
        self.diagnostics.borrow().borrow().clone()
    }
    /// How many diagnostics were raised.
    pub fn diag_len(&self) -> usize {
        self.diagnostics.borrow().borrow().len()
    }
    /// Forget the diagnostics after the first `n`.
    pub fn diag_truncate(&self, n: usize) {
        self.diagnostics.borrow().borrow_mut().truncate(n);
    }
    /// A root's value.
    pub fn root(&self, name: &str) -> Option<Value> {
        self.roots
            .borrow()
            .borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }
    /// Set a root's value.
    pub fn set_root(&self, name: &str, v: Value) {
        let rc = self.roots.borrow().clone();
        let mut roots = rc.borrow_mut();
        if let Some(e) = roots.iter_mut().find(|(n, _)| n == name) {
            e.1 = v;
        } else {
            roots.push((name.to_string(), v));
        }
    }
    /// The roots' values, in order.
    pub fn root_values(&self) -> Vec<Value> {
        self.roots
            .borrow()
            .borrow()
            .iter()
            .map(|(_, v)| v.clone())
            .collect()
    }
    /// Register an instance.
    pub fn registry_push(&self, inst: Rc<RefCell<RecInst>>) {
        self.registry.borrow().borrow_mut().push(inst);
    }
    /// The instances registered so far.
    pub fn registry_snapshot(&self) -> Vec<Rc<RefCell<RecInst>>> {
        self.registry.borrow().borrow().clone()
    }

    // §4.13: a named endpoint in a constant position evaluates at elaboration time
    /// A numeric constant's value where a type expects a number (a size, a bound).
    pub fn const_num(&self, v: &Value) -> Value {
        let name = match v {
            Value::Str(s) => s.clone(),
            other => return other.clone(),
        };
        let ce = self.const_eval.borrow().clone();
        let Some(ce) = ce else { return v.clone() };
        if !self.consts.borrow().contains_key(&name) {
            return v.clone();
        }
        let diag = |code: &str, message: String| {
            let key = format!("{name}{code}");
            if self.const_diag_seen.borrow().contains(&key) {
                return;
            }
            self.const_diag_seen.borrow_mut().insert(key);
            let d = Diag::error(message, String::new(), Some(code));
            match &*self.const_diag_sink.borrow() {
                Some(sink) => sink.borrow_mut().push(d),
                None => self.report(d),
            }
        };
        match ce(&name) {
            Ok(Value::Int(i)) => Value::Int(i),
            Ok(Value::Float(f)) => Value::Float(f),
            Ok(Value::Undef) | Ok(Value::Null) => v.clone(),
            Ok(_) => {
                diag(
                    "E4021",
                    format!("constant {name} is not numeric in a constant position"),
                );
                v.clone()
            }
            Err(Fail::Eval(e)) => {
                let code = if e.msg.contains("zero") {
                    "E5001"
                } else if e.msg.contains("NaN") || e.msg.contains("Infinity") {
                    "E5002"
                } else {
                    "E5001"
                };
                diag(code, format!("evaluating constant {name}: {}", e.msg));
                v.clone()
            }
            Err(_) => v.clone(),
        }
    }

    // ---- unit / dimension name spaces ----
    /// Resolve a dimension by name to its base dimensions.
    pub fn resolve_dim(&self, name: &str, visiting: &mut Vec<String>) -> Result<DimVec, String> {
        if let Some(v) = self.dim_memo.borrow().get(name) {
            return Ok(v.clone());
        }
        if visiting.iter().any(|v| v == name) {
            return Err(format!("circular dimension {name}"));
        }
        let decl = self
            .dim_decls
            .borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown dimension {name}"))?;
        let mut vec = DimVec::new();
        match decl {
            None => {
                vec.insert(name.to_string(), 1);
            }
            Some(terms) => {
                visiting.push(name.to_string());
                for (tn, te) in terms {
                    let sub = self.resolve_dim(&tn, visiting)?;
                    for (n, e) in sub {
                        *vec.entry(n).or_insert(0) += e * te;
                    }
                }
                visiting.pop();
            }
        }
        self.dim_memo
            .borrow_mut()
            .insert(name.to_string(), vec.clone());
        Ok(vec)
    }
    /// A unit's dimension key and factor against the base unit.
    pub fn unit_info(&self, sym: &str) -> Result<(String, f64), String> {
        self.unit_info_v(sym, &mut vec![])
    }
    /// §3.16 unit/dimension-space findings for the checker: the load-time
    /// redeclarations plus unresolvable units and duplicate base units
    pub fn finalize_unit_space(&self) -> Vec<Diag> {
        let mut out = self.space_diags.borrow().clone();
        let mut base_seen: HashMap<String, String> = HashMap::new();
        let syms = self.unit_order.borrow().clone();
        for sym in syms {
            let has_dim = self
                .unit_decls
                .borrow()
                .get(&sym)
                .map(|u| u.dim.is_some())
                .unwrap_or(false);
            match self.unit_info(&sym) {
                Ok((key, _)) => {
                    if has_dim {
                        if let Some(prev) = base_seen.get(&key) {
                            out.push(Diag::error(
                                format!(
                                    "second base unit {sym} for dimension {key} (base is {prev})"
                                ),
                                String::new(),
                                Some("E4073"),
                            ));
                        } else {
                            base_seen.insert(key, sym.clone());
                        }
                    }
                }
                Err(msg) => {
                    let code = if msg.contains("unknown dimension")
                        || msg.contains("circular dimension")
                    {
                        "E3003"
                    } else {
                        "E4073"
                    };
                    out.push(Diag::error(msg, String::new(), Some(code)));
                }
            }
        }
        out
    }
    fn unit_info_v(&self, sym: &str, visiting: &mut Vec<String>) -> Result<(String, f64), String> {
        if let Some(v) = self.unit_memo.borrow().get(sym) {
            return Ok(v.clone());
        }
        if visiting.iter().any(|v| v == sym) {
            return Err(format!("circular unit {sym}"));
        }
        let (dim, factor, base) = {
            let m = self.unit_decls.borrow();
            let u = m.get(sym).ok_or_else(|| format!("unknown unit {sym}"))?;
            (u.dim.clone(), u.factor.clone(), u.base.clone())
        };
        let info = if let Some(d) = dim {
            let key = key_of_vec(&self.resolve_dim(&d, &mut vec![])?);
            self.base_unit_of
                .borrow_mut()
                .entry(key.clone())
                .or_insert_with(|| sym.to_string());
            (key, 1.0)
        } else {
            visiting.push(sym.to_string());
            let b = self.unit_info_v(base.as_deref().unwrap_or(""), visiting)?;
            visiting.pop();
            let mut f: Option<f64> = match factor.as_deref() {
                Some(Expr::Lit(Value::Float(x))) => Some(*x),
                Some(Expr::Lit(Value::Int(i))) => i.to_f64(),
                _ => None,
            };
            if f.is_none() {
                if let (Some(fx), Some(ee)) = (factor.clone(), self.expr_eval.borrow().clone()) {
                    f = match ee(&fx) {
                        Ok(Value::Float(x)) => Some(x),
                        Ok(Value::Int(i)) => i.to_f64(),
                        _ => None,
                    };
                }
            }
            let f = f.ok_or_else(|| format!("unit {sym}: factor is not a numeric constant"))?;
            (b.0, f * b.1)
        };
        self.unit_memo
            .borrow_mut()
            .insert(sym.to_string(), info.clone());
        Ok(info)
    }

    // ---- type resolution ----
    /// Resolve a type annotation to a resolved type (§3), memoized under `name` for a named type.
    pub fn resolve(self: &Rc<Env>, ast: &TypeAst, name: Option<&str>) -> Result<RT, String> {
        Ok(match ast {
            TypeAst::Prim { name: n, .. } => ty(RTk::Prim(n.clone())),
            TypeAst::Lit { v, .. } => ty(RTk::Lit(v.clone())),
            TypeAst::Range { lo, hi, excl, .. } => {
                let lo = self.const_num(lo);
                let hi = self.const_num(hi);
                let is_f = matches!(lo, Value::Float(_)) || matches!(hi, Value::Float(_));
                ty(RTk::Range {
                    lo,
                    hi,
                    excl: *excl,
                    base: if is_f { "float".into() } else { "int".into() },
                })
            }
            TypeAst::Pattern { re: src, .. } => {
                let expanded = self.expand_pattern(src)?;
                if let Some(bad) = pattern_error(&expanded) {
                    return Err(format!("malformed pattern /{src}/: {bad}"));
                }
                let re = compile_pattern(&expanded)
                    .map_err(|e| format!("malformed pattern /{src}/: {e}"))?;
                ty(RTk::Pattern { src: expanded, re })
            }
            TypeAst::Map { key, val, .. } => ty(RTk::Map {
                key: self.resolve(key, None)?,
                val: self.resolve(val, None)?,
            }),
            TypeAst::Array {
                elem, lo, hi, excl, ..
            } => {
                let lo = lo.as_ref().map(|v| self.const_num(v));
                let hi0 = hi.as_ref().map(|v| self.const_num(v));
                let to_i = |v: &Value| match v {
                    Value::Int(i) => i.to_i64(),
                    Value::Float(f) => Some(*f as i64),
                    _ => None,
                };
                let lo_i = lo.as_ref().and_then(&to_i);
                let hi_i = hi0
                    .as_ref()
                    .and_then(to_i)
                    .map(|h| if *excl { h - 1 } else { h });
                ty(RTk::Arr {
                    elem: self.resolve(elem, None)?,
                    lo: lo_i,
                    hi: hi_i,
                })
            }
            TypeAst::Union { arms, .. } => ty(RTk::Union(
                arms.iter()
                    .map(|a| self.resolve(a, None))
                    .collect::<Result<_, _>>()?,
            )),
            TypeAst::Isect { arms, .. } => {
                let arms: Vec<RT> = arms
                    .iter()
                    .map(|a| self.resolve(a, None))
                    .collect::<Result<_, _>>()?;
                if arms.iter().all(is_rec) {
                    self.merge_isect(&arms, name)
                } else {
                    ty(RTk::IsectN(arms))
                }
            }
            TypeAst::Record { members, open, .. } => {
                let rt = ty(RTk::Rec(rec_type(*open)));
                *rt.name.borrow_mut() = name.map(|s| s.to_string());
                self.fill_record(&rt, members)?;
                rt
            }
            TypeAst::Func { params, ret, .. } => ty(RTk::Func {
                params: params
                    .iter()
                    .map(|p| self.resolve(p, None))
                    .collect::<Result<_, _>>()?,
                ret: self.resolve(ret, None)?,
            }),
            TypeAst::Named {
                name: n,
                args,
                preds,
                ext,
                ..
            } => {
                if let Some(preds) = preds {
                    if !preds.is_empty() {
                        let base = self.resolve(
                            &TypeAst::Named {
                                name: n.clone(),
                                args: args.clone(),
                                preds: None,
                                ext: ext.clone(),
                                loc: None,
                            },
                            name,
                        )?;
                        return Ok(ty(RTk::Pred {
                            base,
                            preds: preds.clone(),
                        }));
                    }
                }
                if n == "quantity" {
                    let dn = match args.first() {
                        Some(TypeAst::Named { name, .. }) | Some(TypeAst::Prim { name, .. }) => {
                            name.clone()
                        }
                        _ => return Err("quantity needs a dimension".into()),
                    };
                    return Ok(ty(RTk::Quantity(key_of_vec(
                        &self.resolve_dim(&dn, &mut vec![])?,
                    ))));
                }
                if n == "map" && args.len() == 2 {
                    return Ok(ty(RTk::Map {
                        key: self.resolve(&args[0], None)?,
                        val: self.resolve(&args[1], None)?,
                    }));
                }
                if n == "ref" {
                    return Ok(ty(RTk::Ref(self.resolve(&args[0], None)?)));
                }
                if ["int", "float", "bool", "string"].contains(&n.as_str())
                    && args.is_empty()
                    && ext.is_none()
                {
                    return Ok(ty(RTk::Prim(n.clone())));
                }
                let decl = self.type_asts.borrow().get(n).cloned();
                let Some(decl) = decl else {
                    let im = self.imports.borrow().get(n).cloned();
                    if let Some(im) = im {
                        return im.env.resolve(
                            &TypeAst::Named {
                                name: im.name.clone(),
                                args: args.clone(),
                                preds: None,
                                ext: ext.clone(),
                                loc: None,
                            },
                            name,
                        );
                    }
                    if let Some((ns, rest)) = n.split_once('.') {
                        let ex = self
                            .namespaces
                            .borrow()
                            .get(ns)
                            .and_then(|(_, exports)| exports.borrow().get(rest).cloned());
                        if let Some(ex) = ex {
                            return ex.env.resolve(
                                &TypeAst::Named {
                                    name: ex.name.clone(),
                                    args: args.clone(),
                                    preds: None,
                                    ext: ext.clone(),
                                    loc: None,
                                },
                                name,
                            );
                        }
                    }
                    return Err(format!("unknown type {n}"));
                };
                let memo = self.type_memo.borrow().get(n).cloned();
                let base = if !decl.params.is_empty() {
                    self.instantiate(n, args, &decl)?
                } else if let Some(b) = memo {
                    b
                } else {
                    match &decl.ast {
                        TypeAst::Record { members, open, .. } => {
                            let rt = ty(RTk::Rec(rec_type(*open)));
                            *rt.name.borrow_mut() = Some(n.clone());
                            *rt.tail.borrow_mut() = decl.tail.clone();
                            self.type_memo.borrow_mut().insert(n.clone(), rt.clone());
                            // a member that fails to resolve must not leave a half-filled
                            // record memoized (later lookups would miss its later members)
                            if let Err(e) = self.fill_record(&rt, members) {
                                self.type_memo.borrow_mut().remove(n);
                                return Err(e);
                            }
                            rt
                        }
                        TypeAst::Named {
                            name: pn,
                            args: pa,
                            preds: pp,
                            ext: Some(body),
                            ..
                        } => {
                            // an extension declaration (§3.14) is memoized before its parent
                            // resolves: in a recursive family — `type Base = { kids: { [string]:
                            // Kid } }`, `type Kid = Base { … }` — the parent's body names this
                            // type, and every reference must share the one final record rather
                            // than a snapshot of the parent's members taken mid-fill
                            let rt = ty(RTk::Rec(rec_type(false)));
                            if let RTk::Rec(r) = &rt.k {
                                r.filling.set(true);
                            }
                            *rt.name.borrow_mut() = Some(n.clone());
                            *rt.tail.borrow_mut() = decl.tail.clone();
                            self.type_memo.borrow_mut().insert(n.clone(), rt.clone());
                            let parent_ast = TypeAst::Named {
                                name: pn.clone(),
                                args: pa.clone(),
                                preds: pp.clone(),
                                ext: None,
                                loc: None,
                            };
                            let filled = self.resolve(&parent_ast, None).and_then(|parent| {
                                let extr = self.resolve(body, None)?;
                                self.extend_into(&rt, &parent, &extr);
                                Ok(())
                            });
                            if let Err(e) = filled {
                                self.type_memo.borrow_mut().remove(n);
                                return Err(e);
                            }
                            rt
                        }
                        other => {
                            let rt = self.resolve(other, Some(n))?;
                            if matches!(rt.k, RTk::Rec(_) | RTk::Union(_)) {
                                *rt.name.borrow_mut() = Some(n.clone());
                            }
                            if rt.tail.borrow().is_none() {
                                *rt.tail.borrow_mut() = decl.tail.clone();
                            }
                            self.type_memo.borrow_mut().insert(n.clone(), rt.clone());
                            rt
                        }
                    }
                };
                if let Some(ext) = ext {
                    // an inline extension in a type position: anonymous, never memoized
                    let extr = self.resolve(ext, None)?;
                    if !is_rec(&base) {
                        return Ok(base);
                    }
                    let merged = ty(RTk::Rec(rec_type(false)));
                    if let RTk::Rec(r) = &merged.k {
                        r.filling.set(true);
                    }
                    *merged.name.borrow_mut() = base.name.borrow().clone();
                    self.extend_into(&merged, &base, &extr);
                    return Ok(merged);
                }
                base
            }
        })
    }

    // §3.14: fill `target` as `base` extended by the override body `ext` —
    // base members copied, overrides replacing or adding, asserts appended,
    // and a context declaration narrowed by the extension replacing the
    // inherited one (§7.3). A base still being filled (the recursive-family
    // case above) defers the merge until it completes; `target` stays marked
    // filling meanwhile, so an extension of an extension waits in turn.
    fn extend_into(&self, target: &RT, base: &RT, extr: &RT) {
        let (RTk::Rec(tr), RTk::Rec(br), RTk::Rec(er)) = (&target.k, &base.k, &extr.k) else {
            if let RTk::Rec(tr) = &target.k {
                tr.filling.set(false);
            }
            return;
        };
        if br.filling.get() {
            br.pending.borrow_mut().push((target.clone(), extr.clone()));
            return;
        }
        tr.open.set(br.open.get());
        if let Some(t) = base.tail.borrow().clone() {
            *target.tail.borrow_mut() = Some(t);
        }
        let mut members: Vec<Member> = br.members.borrow().clone();
        for om in er.members.borrow().iter() {
            if let Some(i) = members.iter().position(|m| m.name == om.name) {
                members[i] = om.clone();
            } else {
                members.push(om.clone());
            }
        }
        let mut asserts = br.asserts.borrow().clone();
        asserts.extend(er.asserts.borrow().iter().cloned());
        let mut ctx_decls: Vec<(String, RT)> = br.ctx_decls.borrow().clone();
        for cd in er.ctx_decls.borrow().iter() {
            if let Some(i) = ctx_decls.iter().position(|(v, _)| *v == cd.0) {
                ctx_decls[i] = cd.clone();
            } else {
                ctx_decls.push(cd.clone());
            }
        }
        *tr.members.borrow_mut() = members;
        *tr.asserts.borrow_mut() = asserts;
        *tr.ctx_decls.borrow_mut() = ctx_decls;
        self.complete_record(target);
    }

    // a record's members are final: extensions that waited on it merge now
    fn complete_record(&self, rt: &RT) {
        let RTk::Rec(r) = &rt.k else { return };
        r.filling.set(false);
        let pending: Vec<(RT, RT)> = std::mem::take(&mut *r.pending.borrow_mut());
        for (target, extr) in pending {
            self.extend_into(&target, rt, &extr);
        }
    }

    // §3.6: `${T}` inside a pattern splices another type — a string-shaped
    // T (pattern, string literal, union of those) as its regular language,
    // an integer-shaped T (int literal, int range, union) as the decimal
    // representations of its members
    fn expand_pattern(self: &Rc<Env>, re: &str) -> Result<String, String> {
        let hole: &Regex = &PATTERN_HOLE;
        let mut out = String::new();
        let mut last = 0;
        for m in hole.captures_iter(re) {
            let whole = m.get(0).unwrap();
            out.push_str(&re[last..whole.start()]);
            last = whole.end();
            let text = m.get(1).unwrap().as_str().trim().to_string();
            // the spliced type: a union of string literals, int literals, int
            // ranges, and named types — the type-expression subset that fits
            // inside a pattern token
            let arms: Vec<String> = text.split('|').map(|a| a.trim().to_string()).collect();
            let mut frags: Vec<String> = vec![];
            let str_lit: &Regex = &PATTERN_STR_LIT;
            let int_range: &Regex = &PATTERN_INT_RANGE;
            let int_lit: &Regex = &PATTERN_INT_LIT;
            let ident: &Regex = &PATTERN_IDENT;
            for arm in &arms {
                if str_lit.is_match(arm) {
                    let v = crate::parse::json_unquote(arm)?;
                    frags.push(self.pattern_fragment(&ty(RTk::Lit(Value::Str(v))), &text)?);
                    continue;
                }
                if let Some(c) = int_range.captures(arm) {
                    let lo = c[1].parse::<BigInt>().map_err(|e| e.to_string())?;
                    let hi = c[3].parse::<BigInt>().map_err(|e| e.to_string())?;
                    let rt = ty(RTk::Range {
                        lo: Value::Int(lo),
                        hi: Value::Int(hi),
                        excl: &c[2] == "<",
                        base: "int".into(),
                    });
                    frags.push(self.pattern_fragment(&rt, &text)?);
                    continue;
                }
                if int_lit.is_match(arm) {
                    let v = arm.parse::<BigInt>().map_err(|e| e.to_string())?;
                    frags.push(self.pattern_fragment(&ty(RTk::Lit(Value::Int(v))), &text)?);
                    continue;
                }
                if !ident.is_match(arm) {
                    return Err(format!(
                        "pattern interpolation of {text}: not a type (§3.6)"
                    ));
                }
                if self.pattern_visiting.borrow().iter().any(|v| v == arm) {
                    return Err(format!("pattern interpolation of {arm} is circular"));
                }
                self.pattern_visiting.borrow_mut().push(arm.clone());
                let resolved = self.resolve(
                    &TypeAst::Named {
                        name: arm.clone(),
                        args: vec![],
                        preds: None,
                        ext: None,
                        loc: None,
                    },
                    None,
                );
                self.pattern_visiting.borrow_mut().retain(|v| v != arm);
                let rt = match resolved {
                    Ok(rt) => rt,
                    Err(e) => {
                        if e.starts_with("unknown type") {
                            return Err(format!("pattern interpolation of {arm}: unknown type"));
                        }
                        return Err(e);
                    }
                };
                frags.push(self.pattern_fragment(&rt, arm)?);
            }
            if frags.len() == 1 {
                out.push_str(&frags[0]);
            } else {
                out.push_str(&format!("(?:{})", frags.join("|")));
            }
        }
        out.push_str(&re[last..]);
        Ok(out)
    }
    fn pattern_fragment(self: &Rc<Env>, rt: &RT, name: &str) -> Result<String, String> {
        let esc = |s: &str| -> String {
            let mut o = String::with_capacity(s.len());
            for c in s.chars() {
                if ".*+?^${}()|[]\\/".contains(c) {
                    o.push('\\');
                }
                o.push(c);
            }
            o
        };
        let bad = || {
            Err(format!("pattern interpolation of {name}: type is neither string- nor integer-shaped (§3.6)"))
        };
        match &rt.k {
            RTk::Pattern { src, .. } => Ok(format!("(?:{src})")),
            RTk::Lit(Value::Str(s)) => Ok(esc(s)),
            RTk::Lit(Value::Int(i)) => Ok(i.to_string()),
            RTk::Lit(_) => bad(),
            RTk::Range { lo, hi, excl, base } => {
                let (Value::Int(lo), Value::Int(hi)) = (lo, hi) else {
                    return bad();
                };
                if base != "int" {
                    return bad();
                }
                let hi = if *excl { hi - 1 } else { hi.clone() };
                if &hi - lo >= BigInt::from(65536) {
                    return Err(format!(
                        "pattern interpolation of {name}: range too large (limit 65536 values)"
                    ));
                }
                let mut alts: Vec<String> = vec![];
                let mut v = lo.clone();
                while v <= hi {
                    alts.push(v.to_string());
                    v += 1;
                }
                Ok(format!("(?:{})", alts.join("|")))
            }
            RTk::Union(arms) => {
                let parts = arms
                    .iter()
                    .map(|a| self.pattern_fragment(a, name))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("(?:{})", parts.join("|")))
            }
            RTk::Pred { base, .. } => self.pattern_fragment(base, name),
            RTk::Prim(n) if n == "string" => Ok(".*".into()),
            RTk::Prim(n) if n == "int" => Ok("-?[0-9]+".into()),
            _ => bad(),
        }
    }

    // §3.15 generics
    fn instantiate(
        self: &Rc<Env>,
        name: &str,
        args: &[TypeAst],
        decl: &Rc<TypeEntry>,
    ) -> Result<RT, String> {
        let ps = &decl.params;
        if args.len() != ps.len() {
            return Err(format!(
                "generic arity: {name} expects {} argument(s), got {}",
                ps.len(),
                args.len()
            ));
        }
        let mut types: HashMap<String, TypeAst> = HashMap::new();
        let mut values: HashMap<String, Value> = HashMap::new();
        let mut label = Vec::new();
        for (p, a) in ps.iter().zip(args) {
            if let Some(pty) = &p.ty {
                let v = match a {
                    TypeAst::Lit { v, .. } => v.clone(),
                    TypeAst::Named {
                        name: an,
                        args: aa,
                        ext: None,
                        preds: None,
                        ..
                    } if aa.is_empty() => {
                        let v = self.const_num(&Value::Str(an.clone()));
                        if matches!(v, Value::Str(_)) {
                            return Err(format!(
                                "non-constant value argument {an} for {} of {name}",
                                p.name
                            ));
                        }
                        v
                    }
                    _ => {
                        return Err(format!(
                            "generic arity: parameter {} of {name} takes a constant value",
                            p.name
                        ))
                    }
                };
                let bound = self.resolve(&subst_type(pty, &types, &values), None)?;
                if !crate::subsume::subsumes(self, &ty(RTk::Lit(v.clone())), &bound) {
                    return Err(format!(
                        "value argument {v:?} outside parameter {}'s type in {name}",
                        p.name
                    ));
                }
                label.push(format!("{v:?}"));
                values.insert(p.name.clone(), v);
            } else {
                label.push(match a {
                    TypeAst::Named { name, .. } | TypeAst::Prim { name, .. } => name.clone(),
                    _ => "type".into(),
                });
                types.insert(p.name.clone(), a.clone());
            }
        }
        let key = format!(
            "{name}<{}>",
            args.iter().map(type_key).collect::<Vec<_>>().join(",")
        );
        if let Some(rt) = self.type_memo.borrow().get(&key).cloned() {
            return Ok(rt);
        }
        let shown = format!("{name}<{}>", label.join(", "));
        let body = subst_type(&decl.ast, &types, &values);
        let rt = match &body {
            TypeAst::Record { members, open, .. } => {
                let rt = ty(RTk::Rec(rec_type(*open)));
                *rt.name.borrow_mut() = Some(shown);
                *rt.tail.borrow_mut() = decl.tail.clone();
                self.type_memo.borrow_mut().insert(key.clone(), rt.clone());
                if let Err(e) = self.fill_record(&rt, members) {
                    self.type_memo.borrow_mut().remove(&key);
                    return Err(e);
                }
                rt
            }
            other => {
                let rt = self.resolve(other, Some(&shown))?;
                if matches!(rt.k, RTk::Rec(_) | RTk::Union(_)) {
                    *rt.name.borrow_mut() = Some(shown);
                }
                if rt.tail.borrow().is_none() {
                    *rt.tail.borrow_mut() = decl.tail.clone();
                }
                self.type_memo.borrow_mut().insert(key, rt.clone());
                rt
            }
        };
        Ok(rt)
    }

    fn fill_record(self: &Rc<Env>, rt: &RT, members: &[MemberAst]) -> Result<(), String> {
        let RTk::Rec(r) = &rt.k else { return Ok(()) };
        let origin = rt.name.borrow().clone();
        r.filling.set(true);
        for m in members {
            match m {
                MemberAst::Value {
                    name,
                    opt,
                    ty: t,
                    dflt,
                    ..
                } => r.members.borrow_mut().push(Member {
                    kind: if dflt.is_some() {
                        MKind::Dflt
                    } else if *opt {
                        MKind::Opt
                    } else {
                        MKind::Req
                    },
                    name: name.clone(),
                    hidden: false,
                    ty: Some(self.resolve(t, None)?),
                    conj: None,
                    dflt: dflt.clone(),
                    expr: None,
                    menv: Some(self.clone()),
                }),
                MemberAst::Derived {
                    name,
                    ty: t,
                    expr,
                    hidden,
                    ..
                } => r.members.borrow_mut().push(Member {
                    kind: MKind::Der,
                    name: name.clone(),
                    hidden: *hidden,
                    ty: match t {
                        Some(t) => Some(self.resolve(t, None)?),
                        None => None,
                    },
                    conj: None,
                    dflt: None,
                    expr: Some(expr.clone()),
                    menv: Some(self.clone()),
                }),
                MemberAst::Assert {
                    name, cond, tail, ..
                } => r.asserts.borrow_mut().push(AssertItem {
                    when: false,
                    name: name.clone(),
                    cond: cond.clone(),
                    tail: tail.clone(),
                    body: vec![],
                    origin: origin.clone(),
                    menv: Some(self.clone()),
                }),
                MemberAst::When { cond, body, .. } => r.asserts.borrow_mut().push(AssertItem {
                    when: true,
                    name: String::new(),
                    cond: cond.clone(),
                    tail: None,
                    body: body.clone(),
                    origin: origin.clone(),
                    menv: Some(self.clone()),
                }),
                MemberAst::Context {
                    variable, ty: t, ..
                } => r
                    .ctx_decls
                    .borrow_mut()
                    .push((variable.clone(), self.resolve(t, None)?)),
            }
        }
        self.complete_record(rt);
        Ok(())
    }

    fn merge_isect(&self, arms: &[RT], name: Option<&str>) -> RT {
        let mut members: Vec<Member> = vec![];
        let mut asserts: Vec<AssertItem> = vec![];
        let mut open = true;
        for a in arms {
            let RTk::Rec(r) = &a.k else { continue };
            open = open && r.open.get();
            for m in r.members.borrow().iter() {
                if let Some(i) = members.iter().position(|x| x.name == m.name) {
                    let prev = members[i].clone();
                    let mut conj = prev
                        .conj
                        .clone()
                        .unwrap_or_else(|| prev.ty.iter().cloned().collect());
                    if let Some(t) = &m.ty {
                        conj.push(t.clone());
                    }
                    members[i] = Member {
                        conj: Some(conj),
                        kind: if m.kind == MKind::Req {
                            MKind::Req
                        } else {
                            prev.kind
                        },
                        ..prev
                    };
                } else {
                    members.push(m.clone());
                }
            }
            asserts.extend(r.asserts.borrow().iter().map(|x| AssertItem {
                origin: x.origin.clone().or_else(|| a.name.borrow().clone()),
                ..x.clone()
            }));
        }
        let rec = rec_type(open);
        *rec.members.borrow_mut() = members;
        *rec.asserts.borrow_mut() = asserts;
        let rt = ty(RTk::Rec(rec));
        *rt.name.borrow_mut() = name.map(|s| s.to_string());
        rt
    }
}

fn type_key(t: &TypeAst) -> String {
    match t {
        TypeAst::Prim { name: n, .. } => format!("p:{n}"),
        TypeAst::Lit { v, .. } => format!("l:{v:?}"),
        TypeAst::Named { name, args, .. } => format!(
            "n:{name}<{}>",
            args.iter().map(type_key).collect::<Vec<_>>().join(",")
        ),
        TypeAst::Range { lo, hi, excl, .. } => format!("r:{lo:?}..{excl}{hi:?}"),
        TypeAst::Array { elem, lo, hi, .. } => format!("a:{}[{lo:?},{hi:?}]", type_key(elem)),
        TypeAst::Union { arms: a, .. } => {
            format!("u:{}", a.iter().map(type_key).collect::<Vec<_>>().join("|"))
        }
        TypeAst::Isect { arms: a, .. } => {
            format!("i:{}", a.iter().map(type_key).collect::<Vec<_>>().join("&"))
        }
        TypeAst::Map { key, val, .. } => format!("m:{}:{}", type_key(key), type_key(val)),
        TypeAst::Pattern { re: p, .. } => format!("pat:{p}"),
        TypeAst::Record { members, .. } => format!("rec:{}", members.len()),
        TypeAst::Func { params, ret, .. } => format!("f:{}->{}", params.len(), type_key(ret)),
    }
}

// ---------------- generic substitution ----------------
/// Substitute a generic type's parameters (§3.15).
pub fn subst_type(
    ast: &TypeAst,
    types: &HashMap<String, TypeAst>,
    values: &HashMap<String, Value>,
) -> TypeAst {
    let t = |a: &TypeAst| subst_type(a, types, values);
    match ast {
        TypeAst::Named {
            name,
            args,
            preds,
            ext,
            loc,
        } => {
            let plain = args.is_empty() && ext.is_none() && preds.is_none();
            if plain {
                if let Some(x) = types.get(name) {
                    return x.clone();
                }
                if let Some(v) = values.get(name) {
                    return TypeAst::Lit {
                        v: v.clone(),
                        loc: *loc,
                    };
                }
            }
            TypeAst::Named {
                name: name.clone(),
                args: args.iter().map(t).collect(),
                preds: preds
                    .as_ref()
                    .map(|ps| ps.iter().map(|p| subst_expr(p, values)).collect()),
                ext: ext.as_ref().map(|e| Box::new(t(e))),
                loc: *loc,
            }
        }
        TypeAst::Range { lo, hi, excl, loc } => {
            let sub = |v: &Value| match v {
                Value::Str(s) if values.contains_key(s) => values[s].clone(),
                other => other.clone(),
            };
            TypeAst::Range {
                lo: sub(lo),
                hi: sub(hi),
                excl: *excl,
                loc: *loc,
            }
        }
        TypeAst::Array {
            elem,
            lo,
            hi,
            excl,
            loc,
        } => {
            let sub = |v: &Value| match v {
                Value::Str(s) if values.contains_key(s) => values[s].clone(),
                other => other.clone(),
            };
            TypeAst::Array {
                elem: Box::new(t(elem)),
                lo: lo.as_ref().map(sub),
                hi: hi.as_ref().map(sub),
                excl: *excl,
                loc: *loc,
            }
        }
        TypeAst::Record { members, open, loc } => TypeAst::Record {
            members: members
                .iter()
                .map(|m| subst_member(m, types, values))
                .collect(),
            open: *open,
            loc: *loc,
        },
        TypeAst::Map { key, val, loc } => TypeAst::Map {
            key: Box::new(t(key)),
            val: Box::new(t(val)),
            loc: *loc,
        },
        TypeAst::Union { arms: a, loc } => TypeAst::Union {
            arms: a.iter().map(t).collect(),
            loc: *loc,
        },
        TypeAst::Isect { arms: a, loc } => TypeAst::Isect {
            arms: a.iter().map(t).collect(),
            loc: *loc,
        },
        TypeAst::Func { params, ret, loc } => TypeAst::Func {
            params: params.iter().map(t).collect(),
            ret: Box::new(t(ret)),
            loc: *loc,
        },
        other => other.clone(),
    }
}
fn subst_member(
    m: &MemberAst,
    types: &HashMap<String, TypeAst>,
    values: &HashMap<String, Value>,
) -> MemberAst {
    let t = |a: &TypeAst| subst_type(a, types, values);
    match m {
        MemberAst::Value {
            name,
            opt,
            ty,
            dflt,
            loc,
        } => MemberAst::Value {
            name: name.clone(),
            opt: *opt,
            ty: t(ty),
            dflt: dflt.as_ref().map(|d| subst_expr(d, values)),
            loc: *loc,
        },
        MemberAst::Derived {
            name,
            ty,
            expr,
            hidden,
            loc,
        } => MemberAst::Derived {
            name: name.clone(),
            ty: ty.as_ref().map(t),
            expr: subst_expr(expr, values),
            hidden: *hidden,
            loc: *loc,
        },
        MemberAst::Context { variable, ty, loc } => MemberAst::Context {
            variable: variable.clone(),
            ty: t(ty),
            loc: *loc,
        },
        MemberAst::Assert {
            name,
            cond,
            tail,
            loc,
        } => MemberAst::Assert {
            name: name.clone(),
            cond: subst_expr(cond, values),
            tail: tail.clone(),
            loc: *loc,
        },
        MemberAst::When { cond, body, loc } => MemberAst::When {
            cond: subst_expr(cond, values),
            body: body
                .iter()
                .map(|b| subst_member(b, types, values))
                .collect(),
            loc: *loc,
        },
    }
}
/// Substitute values for names in an expression.
pub fn subst_expr(e: &Rc<Expr>, values: &HashMap<String, Value>) -> Rc<Expr> {
    if values.is_empty() {
        return e.clone();
    }
    let s = |x: &Rc<Expr>| subst_expr(x, values);
    let cls = |c: &ForClause| ForClause {
        v: c.v.clone(),
        iter: s(&c.iter),
        filters: c.filters.iter().map(s).collect(),
    };
    let out = Rc::new(match &**e {
        Expr::Name(n) if values.contains_key(n) => Expr::Lit(values[n].clone()),
        Expr::Template(parts) => Expr::Template(
            parts
                .iter()
                .map(|p| match p {
                    TPart::Expr(x) => TPart::Expr(s(x)),
                    other => other.clone(),
                })
                .collect(),
        ),
        Expr::Obj(es) => Expr::Obj(es.iter().map(|(k, v)| (k.clone(), s(v))).collect()),
        Expr::Arr(items) => Expr::Arr(items.iter().map(|(sp, v)| (*sp, s(v))).collect()),
        Expr::Comp { head, clauses } => Expr::Comp {
            head: s(head),
            clauses: clauses.iter().map(cls).collect(),
        },
        Expr::MapComp { key, val, clauses } => Expr::MapComp {
            key: s(key),
            val: s(val),
            clauses: clauses.iter().map(cls).collect(),
        },
        Expr::Bin { op, l, r } => Expr::Bin {
            op: op.clone(),
            l: s(l),
            r: s(r),
        },
        Expr::Un { op, x } => Expr::Un {
            op: op.clone(),
            x: s(x),
        },
        Expr::Paren(x) => Expr::Paren(s(x)),
        Expr::If { c, t, f } => Expr::If {
            c: s(c),
            t: s(t),
            f: s(f),
        },
        Expr::Lambda { params, body } => Expr::Lambda {
            params: params.clone(),
            body: s(body),
        },
        Expr::Call { fun, args } => Expr::Call {
            fun: s(fun),
            args: args.iter().map(s).collect(),
        },
        Expr::Member { x, name, safe } => Expr::Member {
            x: s(x),
            name: name.clone(),
            safe: *safe,
        },
        Expr::Index { x, i } => Expr::Index { x: s(x), i: s(i) },
        Expr::With { base, patch } => Expr::With {
            base: s(base),
            patch: s(patch),
        },
        Expr::Match { subject, arms } => Expr::Match {
            subject: s(subject),
            arms: arms
                .iter()
                .map(|a| MatchArm {
                    v: a.v.clone(),
                    ty: a.ty.clone(),
                    body: s(&a.body),
                })
                .collect(),
        },
        other => other.clone(),
    });
    // a substituted node keeps the source range of the node it replaces (the reference copies `loc`)
    if let Some(l) = expr_loc(e) {
        set_expr_loc(&out, l);
    }
    out
}

// ---------------- helpers ----------------
// ---------------- patterns: the portable core (§3.6) ----------------
// A pattern body is validated against the specification's regular-
// expression core with one fixed set of messages, so every implementation
// reports the same text whatever engine runs the accepted patterns.
// Returns the reason a body is outside the core, or None when it is inside.
const PATTERN_PUNCT: &str = "\\/.*+?()[]{}|^$-";
fn pattern_escape(cs: &[char], i: &mut usize) -> Result<i64, String> {
    if *i + 1 >= cs.len() {
        return Err("trailing backslash".into());
    }
    let e = cs[*i + 1];
    *i += 2;
    if "dwsDWS".contains(e) {
        return Ok(-1);
    }
    match e {
        'n' => return Ok(10),
        't' => return Ok(9),
        'r' => return Ok(13),
        _ => {}
    }
    if PATTERN_PUNCT.contains(e) {
        return Ok(e as i64);
    }
    if e.is_ascii_digit() {
        return Err(format!("backreference \\{e} is not supported"));
    }
    Err(format!("unsupported escape \\{e}"))
}
/// Why a pattern body is outside the §3.6 core (E4119), when it is.
pub fn pattern_error(src: &str) -> Option<String> {
    let cs: Vec<char> = src.chars().collect();
    let n = cs.len();
    let (mut i, mut depth, mut can_repeat) = (0usize, 0i32, false);
    while i < n {
        match cs[i] {
            '\\' => {
                if let Err(r) = pattern_escape(&cs, &mut i) {
                    return Some(r);
                }
                can_repeat = true;
            }
            '[' => {
                i += 1;
                if i < n && cs[i] == '^' {
                    i += 1;
                }
                let mut items = 0;
                loop {
                    if i >= n {
                        return Some("unterminated character class".into());
                    }
                    if cs[i] == ']' {
                        i += 1;
                        break;
                    }
                    let lo = if cs[i] == '\\' {
                        match pattern_escape(&cs, &mut i) {
                            Ok(v) => v,
                            Err(r) => return Some(r),
                        }
                    } else {
                        let v = cs[i] as i64;
                        i += 1;
                        v
                    };
                    if i < n && cs[i] == '-' && i + 1 < n && cs[i + 1] != ']' {
                        i += 1;
                        let hi = if cs[i] == '\\' {
                            match pattern_escape(&cs, &mut i) {
                                Ok(v) => v,
                                Err(r) => return Some(r),
                            }
                        } else {
                            let v = cs[i] as i64;
                            i += 1;
                            v
                        };
                        if lo < 0 || hi < 0 || lo > hi {
                            return Some("invalid range in character class".into());
                        }
                    }
                    items += 1;
                }
                if items == 0 {
                    return Some("empty character class".into());
                }
                can_repeat = true;
            }
            ']' => return Some("unbalanced bracket".into()),
            '(' => {
                i += 1;
                if i < n && cs[i] == '?' {
                    if i + 1 < n && cs[i + 1] == ':' {
                        i += 2;
                    } else {
                        return Some("unsupported construct (?".into());
                    }
                }
                depth += 1;
                can_repeat = false;
            }
            ')' => {
                if depth == 0 {
                    return Some("unbalanced parenthesis".into());
                }
                depth -= 1;
                i += 1;
                can_repeat = true;
            }
            '|' => {
                i += 1;
                can_repeat = false;
            }
            '*' | '+' | '?' => {
                if !can_repeat {
                    return Some("nothing to repeat".into());
                }
                i += 1;
                can_repeat = false;
            }
            '{' => {
                if !can_repeat {
                    return Some("nothing to repeat".into());
                }
                let mut j = i + 1;
                let start = j;
                while j < n && cs[j].is_ascii_digit() {
                    j += 1;
                }
                if j == start {
                    return Some("malformed repetition".into());
                }
                let m: String = cs[start..j].iter().collect();
                let mut hi: Option<String> = None;
                if j < n && cs[j] == ',' {
                    j += 1;
                    let s2 = j;
                    while j < n && cs[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > s2 {
                        hi = Some(cs[s2..j].iter().collect());
                    }
                }
                if j >= n || cs[j] != '}' {
                    return Some("malformed repetition".into());
                }
                if let Some(h) = hi {
                    if h.parse::<BigInt>().unwrap_or_default()
                        < m.parse::<BigInt>().unwrap_or_default()
                    {
                        return Some("malformed repetition".into());
                    }
                }
                i = j + 1;
                can_repeat = false;
            }
            '}' => return Some("malformed repetition".into()),
            '^' | '$' => {
                i += 1;
                can_repeat = false;
            }
            _ => {
                i += 1;
                can_repeat = true;
            }
        }
    }
    if depth > 0 {
        Some("unbalanced parenthesis".into())
    } else {
        None
    }
}
/// Compile a pattern body to a regular expression.
pub fn compile_pattern(src: &str) -> Result<Regex, String> {
    Regex::new(&format!("^(?:{src})$")).map_err(|e| e.to_string())
}

/// A canonical path's text, relative to `rel_root` (`$.…`) when given.
pub fn path_str(segs: &[Seg], rel_root: Option<&str>) -> String {
    let mut out = String::new();
    for (i, s) in segs.iter().enumerate() {
        match s {
            _ if i == 0 => {
                let n = seg_text(s);
                if rel_root == Some(n.as_str()) {
                    out.push('$');
                } else {
                    out.push_str(&n);
                }
            }
            Seg::Idx(k) => out.push_str(&format!("[{k}]")),
            Seg::Key(n) => out.push_str(&format!("[{}]", json_str(n))),
            Seg::Name(n) if dot_spellable(n) => {
                out.push('.');
                out.push_str(n);
            }
            Seg::Name(n) => out.push_str(&format!("[{}]", json_str(n))),
        }
    }
    out
}

/// A path string from a document: `.name` is a member, `["…"]` a bracketed
/// segment (a map key, or a member the dot cannot spell — the canonical walk,
/// §7.5, decides which is legal where), `[n]` an index.
pub fn parse_path(s: &str, root_name: &str) -> R<SegPath> {
    let id_re = Regex::new(r"^[_A-Za-z][_A-Za-z0-9]*").unwrap();
    let mut segs = vec![];
    let mut i = if s.starts_with('$') {
        segs.push(Seg::Name(root_name.to_string()));
        1
    } else {
        let m = id_re
            .find(s)
            .ok_or(())
            .or_else(|_| err(format!("bad path {s}")))?;
        segs.push(Seg::Name(m.as_str().to_string()));
        m.end()
    };
    while i < s.len() {
        let rest = &s[i..];
        if let Some(r) = rest.strip_prefix('.') {
            let m = id_re
                .find(r)
                .ok_or(())
                .or_else(|_| err(format!("bad path {s}")))?;
            segs.push(Seg::Name(m.as_str().to_string()));
            i += 1 + m.end();
        } else if rest.starts_with('[') {
            let j = rest
                .find(']')
                .ok_or(())
                .or_else(|_| err(format!("bad path {s}")))?;
            let inner = &rest[1..j];
            if inner.starts_with('"') {
                segs.push(Seg::Key(
                    crate::parse::json_unquote(inner).unwrap_or_default(),
                ));
            } else {
                segs.push(Seg::Idx(inner.parse().unwrap_or(0)));
            }
            i += j + 1;
        } else {
            return err(format!("bad path {s}"));
        }
    }
    Ok(segs)
}

/// Canonical path order (§7.2): segment-wise, indices numerically, names and
/// keys lexicographically, a prefix first.
pub fn cmp_path(a: &[Seg], b: &[Seg]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        match (x, y) {
            (Seg::Idx(i), Seg::Idx(j)) => {
                if i != j {
                    return i.cmp(j);
                }
            }
            _ => {
                let xs = seg_text(x);
                let ys = seg_text(y);
                if xs != ys {
                    return xs.cmp(&ys);
                }
            }
        }
    }
    a.len().cmp(&b.len())
}

/// Structural equality of two values (§4.5).
pub fn value_eq(a: &Value, b: &Value) -> bool {
    let (pa, pb) = (a.place(), b.place());
    if let (Some(pa), Some(pb)) = (&pa, &pb) {
        if matches!(a, Value::Ref(_)) || matches!(b, Value::Ref(_)) {
            return cmp_path(pa, pb) == std::cmp::Ordering::Equal;
        }
    }
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        // two unforced slots compare equal, as in the reference (undefined === undefined)
        (Value::Undef, Value::Undef) => true,
        (Value::Q { dim: d1, value: v1 }, Value::Q { dim: d2, value: v2 }) => d1 == d2 && v1 == v2,
        (Value::Arr(x), Value::Arr(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.items.len() == y.items.len()
                && x.items.iter().zip(&y.items).all(|(p, q)| value_eq(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.entries.len() == y.entries.len()
                && x.entries
                    .iter()
                    .all(|(k, v)| y.get(k).map(|w| value_eq(v, w)).unwrap_or(false))
        }
        (Value::Rec(x), Value::Rec(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            for (n, s) in &x.slots {
                if s.hidden {
                    continue; // a hidden member is not part of the value (D34)
                }
                let v1 = if s.state == SlotState::Absent {
                    Value::Absent
                } else {
                    s.value.clone()
                };
                let v2 = match y.slot(n) {
                    Some(s2) if s2.state != SlotState::Absent => s2.value.clone(),
                    _ => Value::Absent,
                };
                match (&v1, &v2) {
                    (Value::Absent, Value::Absent) => continue,
                    (Value::Absent, _) | (_, Value::Absent) => return false,
                    _ => {
                        if !value_eq(&v1, &v2) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        _ => false,
    }
}

// ---------------- lexical JSON (int/float by lexeme) ----------------
/// Read a JSON document (§10.2): objects keep their key order, integers stay
/// exact; trailing characters are an error.
pub fn read_json(src: &str) -> R<Value> {
    let b = src.as_bytes();
    let mut i = 0usize;
    fn ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
            *i += 1;
        }
    }
    fn string(src: &str, b: &[u8], i: &mut usize) -> R<String> {
        let mut j = *i + 1;
        let mut out = String::new();
        while j < b.len() && b[j] != b'"' {
            if b[j] == b'\\' {
                let e = b[j + 1] as char;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let cp = u32::from_str_radix(&src[j + 2..j + 6], 16).unwrap_or(0xfffd);
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        j += 4;
                    }
                    other => out.push(other),
                }
                j += 2;
            } else {
                let ch = src[j..].chars().next().unwrap();
                out.push(ch);
                j += ch.len_utf8();
            }
        }
        *i = j + 1;
        Ok(out)
    }
    fn val(src: &str, b: &[u8], i: &mut usize) -> R<Value> {
        ws(b, i);
        if *i >= b.len() {
            return err("bad JSON: unexpected end");
        }
        match b[*i] {
            b'{' => {
                *i += 1;
                let mut entries = vec![];
                ws(b, i);
                if b[*i] == b'}' {
                    *i += 1;
                    return Ok(Value::JObj(Rc::new(entries)));
                }
                loop {
                    ws(b, i);
                    let k = string(src, b, i)?;
                    ws(b, i);
                    *i += 1;
                    let v = val(src, b, i)?;
                    entries.push((k, v));
                    ws(b, i);
                    if b[*i] == b',' {
                        *i += 1;
                        continue;
                    }
                    *i += 1;
                    return Ok(Value::JObj(Rc::new(entries)));
                }
            }
            b'[' => {
                *i += 1;
                let mut items = vec![];
                ws(b, i);
                if b[*i] == b']' {
                    *i += 1;
                    return Ok(Value::JArr(Rc::new(items)));
                }
                loop {
                    items.push(val(src, b, i)?);
                    ws(b, i);
                    if b[*i] == b',' {
                        *i += 1;
                        continue;
                    }
                    *i += 1;
                    return Ok(Value::JArr(Rc::new(items)));
                }
            }
            b'"' => Ok(Value::Str(string(src, b, i)?)),
            _ => {
                let rest = &src[*i..];
                if rest.starts_with("true") {
                    *i += 4;
                    return Ok(Value::Bool(true));
                }
                if rest.starts_with("false") {
                    *i += 5;
                    return Ok(Value::Bool(false));
                }
                if rest.starts_with("null") {
                    *i += 4;
                    return Ok(Value::Null);
                }
                let re = Regex::new(r"^-?(?:0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?").unwrap();
                let m = re
                    .captures(rest)
                    .ok_or(())
                    .or_else(|_| err(format!("bad JSON at {i}")))?;
                let whole = m.get(0).unwrap().as_str();
                *i += whole.len();
                if m.get(1).is_some() || m.get(2).is_some() {
                    Ok(Value::Float(whole.parse::<f64>().unwrap_or(0.0)))
                } else {
                    Ok(Value::Int(
                        whole.parse::<BigInt>().unwrap_or_else(|_| BigInt::zero()),
                    ))
                }
            }
        }
    }
    let v = val(src, b, &mut i)?;
    ws(b, &mut i);
    if i < b.len() {
        return err("bad JSON: trailing characters");
    }
    Ok(v)
}

// ---------------- JS-compatible number printing ----------------
/// ECMAScript Number::toString for finite doubles (shortest round trip)
pub fn js_num_str(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let k = digits.len() as i32;
    let n = exp + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let mant = if k > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.to_string()
        };
        format!("{mant}e{}{}", if e > 0 { "+" } else { "-" }, e.abs())
    };
    if x < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// A string as JSON text, escaped.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
