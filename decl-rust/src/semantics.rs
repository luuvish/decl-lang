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

// ---------------- paths ----------------
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Name(String),
    Idx(usize),
}
pub type SegPath = Vec<Seg>;

// ---------------- values ----------------
#[derive(Clone)]
pub enum Value {
    Int(BigInt),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Absent,
    Undef,
    Q { dim: String, value: f64 },
    Ref(Rc<SegPath>),
    Rec(Rc<RefCell<RecInst>>),
    Arr(Rc<RefCell<ArrV>>),
    Map(Rc<RefCell<MapV>>),
    Range { lo: Box<Value>, hi: Box<Value>, excl: bool },
    Clo(Rc<Closure>),
    Nat(NatFn),
    Std(Rc<Vec<String>>),
    NsRef(Rc<NsRefV>),
    Pat(String),
    PreObj(Rc<Vec<(String, Value)>>),
    PreArr(Rc<Vec<(bool, Value)>>),
    PreVal(Rc<PreValV>),
    JObj(Rc<Vec<(String, Value)>>),
    JArr(Rc<Vec<Value>>),
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
    pub fn is_undef(&self) -> bool {
        matches!(self, Value::Undef)
    }
    pub fn is_absent(&self) -> bool {
        matches!(self, Value::Absent)
    }
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

pub type NatFn = Rc<dyn Fn(&[Value]) -> R<Value>>;

pub struct Closure {
    pub params: Vec<String>,
    pub body: Rc<Expr>,
    pub scope: Scope,
}
pub struct NsRefV {
    pub exports: Rc<RefCell<HashMap<String, Export>>>,
}
pub struct PreValV {
    pub expr: Rc<Expr>,
    pub scope: Scope,
}
pub struct ArrV {
    pub items: Vec<Value>,
    pub path: SegPath,
}
pub struct MapV {
    pub entries: Vec<(String, Value)>,
    pub path: SegPath,
}
impl MapV {
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.entries.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    pub fn has(&self, k: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == k)
    }
    pub fn set(&mut self, k: String, v: Value) {
        if let Some(e) = self.entries.iter_mut().find(|(n, _)| *n == k) {
            e.1 = v;
        } else {
            self.entries.push((k, v));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MKind {
    Req,
    Opt,
    Dflt,
    Der,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SlotState {
    Unforced,
    Forcing,
    Ok,
    Invalid,
    Absent,
}

#[derive(Clone)]
pub enum Compute {
    Check { raw: Value, types: Vec<RT>, name: String, root_name: String, menv: Option<Rc<Env>> },
    Default { expr: Rc<Expr>, types: Vec<RT>, name: String, root_name: String, menv: Option<Rc<Env>> },
    Derived { expr: Rc<Expr>, ty: Option<RT>, supplied: Option<Value>, name: String, root_name: String, menv: Option<Rc<Env>> },
}

pub struct Slot {
    pub kind: MKind,
    pub state: SlotState,
    pub value: Value,
    pub compute: Option<Compute>,
}

pub struct RecInst {
    pub type_name: Option<String>,
    pub rt: RT,
    pub path: SegPath,
    pub parent: Option<Rc<RefCell<RecInst>>>,
    // declaration order matters (forcing order drives diagnostic order)
    pub slots: Vec<(String, Slot)>,
    pub entry_order: Vec<String>,
    pub extras: Vec<(String, Value)>,
    pub menv: Option<Rc<Env>>,
}
impl RecInst {
    pub fn extra(&self, n: &str) -> Option<&Value> {
        self.extras.iter().find(|(k, _)| k == n).map(|(_, v)| v)
    }
    pub fn set_extra(&mut self, n: &str, v: Value) {
        if let Some(e) = self.extras.iter_mut().find(|(k, _)| k == n) {
            e.1 = v;
        } else {
            self.extras.push((n.to_string(), v));
        }
    }
    pub fn slot(&self, n: &str) -> Option<&Slot> {
        self.slots.iter().find(|(k, _)| k == n).map(|(_, s)| s)
    }
    pub fn slot_mut(&mut self, n: &str) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|(k, _)| k == n).map(|(_, s)| s)
    }
    pub fn has_slot(&self, n: &str) -> bool {
        self.slots.iter().any(|(k, _)| k == n)
    }
}

#[derive(Clone)]
pub struct Scope {
    pub inst: Option<Rc<RefCell<RecInst>>>,
    pub locals: Rc<HashMap<String, Value>>,
    pub root_name: String,
    pub menv: Option<Rc<Env>>,
}
impl Scope {
    pub fn new(root_name: &str, menv: Option<Rc<Env>>) -> Scope {
        Scope { inst: None, locals: Rc::new(HashMap::new()), root_name: root_name.to_string(), menv }
    }
    pub fn with_locals(&self, locals: HashMap<String, Value>) -> Scope {
        Scope { inst: self.inst.clone(), locals: Rc::new(locals), root_name: self.root_name.clone(), menv: self.menv.clone() }
    }
    pub fn with_inst(&self, inst: Option<Rc<RefCell<RecInst>>>) -> Scope {
        Scope { inst, locals: self.locals.clone(), root_name: self.root_name.clone(), menv: self.menv.clone() }
    }
    pub fn with_menv(&self, menv: Option<Rc<Env>>) -> Scope {
        Scope { inst: self.inst.clone(), locals: self.locals.clone(), root_name: self.root_name.clone(), menv }
    }
}

// ---------------- failures & diagnostics ----------------
pub struct EvalErr {
    pub msg: String,
    pub code: Option<String>,
}
pub enum Fail {
    Taint,
    Defer,
    Eval(EvalErr),
}
pub type R<T> = Result<T, Fail>;
pub fn err<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail::Eval(EvalErr { msg: msg.into(), code: None }))
}
pub fn err_code<T>(msg: impl Into<String>, code: &str) -> R<T> {
    Err(Fail::Eval(EvalErr { msg: msg.into(), code: Some(code.to_string()) }))
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub severity: String,
    pub id: Option<String>,
    pub message: String,
    pub path: String,
    pub code: Option<String>,
}
impl Diag {
    pub fn error(message: impl Into<String>, path: String, code: Option<&str>) -> Diag {
        Diag { severity: "error".into(), id: None, message: message.into(), path, code: code.map(|c| c.to_string()) }
    }
    pub fn to_json(&self, file: Option<&str>) -> String {
        let mut parts = Vec::new();
        if let Some(f) = file {
            parts.push(format!("\"file\":{}", json_str(f)));
        }
        parts.push(format!("\"severity\":{}", json_str(&self.severity)));
        if let Some(id) = &self.id {
            parts.push(format!("\"id\":{}", json_str(id)));
        }
        parts.push(format!("\"message\":{}", json_str(&self.message)));
        parts.push(format!("\"path\":{}", json_str(&self.path)));
        if let Some(c) = &self.code {
            parts.push(format!("\"code\":{}", json_str(c)));
        }
        format!("{{{}}}", parts.join(","))
    }
}

// ---------------- resolved types ----------------
pub type RT = Rc<Ty>;

pub struct Ty {
    pub k: RTk,
    pub name: RefCell<Option<String>>,
    pub tail: RefCell<Option<Tail>>,
}
pub fn ty(k: RTk) -> RT {
    Rc::new(Ty { k, name: RefCell::new(None), tail: RefCell::new(None) })
}

pub enum RTk {
    Prim(String),
    Lit(Value),
    Range { lo: Value, hi: Value, excl: bool, base: String },
    Pattern { src: String, re: Regex },
    Arr { elem: RT, lo: Option<i64>, hi: Option<i64> },
    Map { key: RT, val: RT },
    Union(Vec<RT>),
    IsectN(Vec<RT>),
    Rec(RecType),
    Pred { base: RT, preds: Vec<Rc<Expr>> },
    Ref(RT),
    Quantity(String),
    Func { params: Vec<RT>, ret: RT },
    Any,
}

pub struct RecType {
    pub open: bool,
    pub members: RefCell<Vec<Member>>,
    pub asserts: RefCell<Vec<AssertItem>>,
    /// `context $parent: ref<T>` declarations (D30), checked at embedding sites
    pub ctx_decls: RefCell<Vec<(String, RT)>>,
}
pub fn rec_type(open: bool) -> RecType {
    RecType { open, members: RefCell::new(vec![]), asserts: RefCell::new(vec![]), ctx_decls: RefCell::new(vec![]) }
}

#[derive(Clone)]
pub struct Member {
    pub kind: MKind,
    pub name: String,
    pub ty: Option<RT>,
    pub conj: Option<Vec<RT>>,
    pub dflt: Option<Rc<Expr>>,
    pub expr: Option<Rc<Expr>>,
    pub menv: Option<Rc<Env>>,
}

#[derive(Clone)]
pub struct AssertItem {
    pub when: bool,
    pub name: String,
    pub cond: Rc<Expr>,
    pub tail: Option<Tail>,
    pub body: Vec<MemberAst>,
    pub origin: Option<String>,
    pub menv: Option<Rc<Env>>,
}

pub fn rec_members(t: &RT) -> Vec<Member> {
    match &t.k {
        RTk::Rec(r) => r.members.borrow().clone(),
        _ => vec![],
    }
}
pub fn is_rec(t: &RT) -> bool {
    matches!(t.k, RTk::Rec(_))
}

// ---------------- dimension vectors ----------------
pub type DimVec = BTreeMap<String, i32>;
pub fn key_of_vec(v: &DimVec) -> String {
    v.iter().filter(|(_, e)| **e != 0).map(|(n, e)| if *e == 1 { n.clone() } else { format!("{n}^{e}") }).collect::<Vec<_>>().join("*")
}
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
pub fn vec_combine(a: &DimVec, b: &DimVec, sign: i32) -> DimVec {
    let mut out = a.clone();
    for (n, e) in b {
        *out.entry(n.clone()).or_insert(0) += sign * e;
    }
    out
}

// ---------------- environment ----------------
pub struct Export {
    pub env: Rc<Env>,
    pub name: String,
}
impl Clone for Export {
    fn clone(&self) -> Self {
        Export { env: self.env.clone(), name: self.name.clone() }
    }
}
pub struct ConstEntry {
    pub expr: Rc<Expr>,
    pub ty: Option<TypeAst>,
    pub state: Cell<bool>,
    pub value: RefCell<Value>,
}
pub struct FuncEntry {
    pub params: Vec<Param>,
    pub ret: Option<TypeAst>,
    pub body: Rc<Expr>,
}
pub struct TypeEntry {
    pub ast: TypeAst,
    pub tail: Option<Tail>,
    pub params: Vec<Param>,
}
pub struct DiagDecl {
    pub params: Vec<Param>,
    pub severity: String,
    pub template: Vec<TPart>,
}
pub struct UnitDecl {
    pub dim: Option<String>,
    pub factor: Option<Rc<Expr>>,
    pub base: Option<String>,
}
pub type ConstEval = Rc<dyn Fn(&str) -> R<Value>>;
pub type ExprEval = Rc<dyn Fn(&Rc<Expr>) -> R<Value>>;

pub struct Env {
    pub type_asts: RefCell<HashMap<String, Rc<TypeEntry>>>,
    pub type_memo: RefCell<HashMap<String, RT>>,
    // names being spliced into a pattern right now, across nested
    // resolutions — a mutually recursive pair is a cycle, not a stack overflow
    pub pattern_visiting: RefCell<Vec<String>>,
    pub consts: RefCell<HashMap<String, Rc<ConstEntry>>>,
    pub funcs: RefCell<HashMap<String, Rc<FuncEntry>>>,
    pub duplicates: RefCell<Vec<String>>,
    pub outputs: RefCell<Vec<(String, TypeAst, Rc<Expr>)>>,
    pub inputs: RefCell<HashMap<String, (TypeAst, Option<Rc<Expr>>)>>,
    pub diags: RefCell<HashMap<String, Rc<DiagDecl>>>,
    pub registry: RefCell<Rc<RefCell<Vec<Rc<RefCell<RecInst>>>>>>,
    pub roots: RefCell<Rc<RefCell<Vec<(String, Value)>>>>,
    pub diagnostics: RefCell<Rc<RefCell<Vec<Diag>>>>,
    pub const_eval: RefCell<Option<ConstEval>>,
    pub expr_eval: RefCell<Option<ExprEval>>,
    pub imports: RefCell<HashMap<String, Export>>,
    pub namespaces: RefCell<HashMap<String, (Rc<Env>, Rc<RefCell<HashMap<String, Export>>>)>>,
    const_diag_seen: RefCell<HashSet<String>>,
    pub dim_decls: RefCell<HashMap<String, Option<Vec<(String, i32)>>>>,
    pub dim_memo: RefCell<HashMap<String, DimVec>>,
    pub unit_decls: RefCell<HashMap<String, UnitDecl>>,
    pub unit_memo: RefCell<HashMap<String, (String, f64)>>,
    pub base_unit_of: RefCell<HashMap<String, String>>,
    pub space_diags: RefCell<Vec<Diag>>,
    /// declaration order (HashMaps do not keep it; diagnostics follow it)
    pub type_order: RefCell<Vec<String>>,
    pub unit_order: RefCell<Vec<String>>,
    /// installed by the checker: constant-evaluation errors go here instead of the report
    pub const_diag_sink: RefCell<Option<Rc<RefCell<Vec<Diag>>>>>,
}

const SI_PREFIXES: [(&str, f64); 20] = [
    ("y", 1e-24), ("z", 1e-21), ("a", 1e-18), ("f", 1e-15), ("p", 1e-12), ("n", 1e-9), ("u", 1e-6), ("m", 1e-3),
    ("c", 1e-2), ("d", 1e-1), ("da", 1e1), ("h", 1e2), ("k", 1e3), ("M", 1e6), ("G", 1e9), ("T", 1e12),
    ("P", 1e15), ("E", 1e18), ("Z", 1e21), ("Y", 1e24),
];

impl Env {
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
                    Some(d) => UnitDecl { dim: Some(d.to_string()), factor: None, base: None },
                    None => UnitDecl { dim: None, factor: Some(Rc::new(Expr::Lit(Value::Float(factor)))), base: Some(base.to_string()) },
                },
            );
        };
        let bases = [("Time", "s"), ("Length", "m"), ("Mass", "kg"), ("Current", "A"), ("Temperature", "K"), ("Amount", "mol"), ("LuminousIntensity", "cd")];
        for (d, _) in bases {
            self.dim_decls.borrow_mut().insert(d.to_string(), None);
        }
        let t = |n: &str, e: i32| (n.to_string(), e);
        let derived: Vec<(&str, Option<Vec<(String, i32)>>, &str)> = vec![
            ("Frequency", Some(vec![t("Time", -1)]), "Hz"),
            ("Force", Some(vec![t("Mass", 1), t("Length", 1), t("Time", -2)]), "N"),
            ("Pressure", Some(vec![t("Mass", 1), t("Length", -1), t("Time", -2)]), "Pa"),
            ("Energy", Some(vec![t("Mass", 1), t("Length", 2), t("Time", -2)]), "J"),
            ("Power", Some(vec![t("Mass", 1), t("Length", 2), t("Time", -3)]), "W"),
            ("Charge", Some(vec![t("Current", 1), t("Time", 1)]), "C"),
            ("Voltage", Some(vec![t("Mass", 1), t("Length", 2), t("Time", -3), t("Current", -1)]), "V"),
            ("Resistance", Some(vec![t("Mass", 1), t("Length", 2), t("Time", -3), t("Current", -2)]), "Ohm"),
            ("Capacitance", Some(vec![t("Mass", -1), t("Length", -2), t("Time", 4), t("Current", 2)]), "F"),
            ("DataSize", None, "bit"),
        ];
        for (d, terms, _) in &derived {
            self.dim_decls.borrow_mut().insert(d.to_string(), terms.clone());
        }
        for (d, s) in bases {
            unit(s, Some(d), 1.0, "");
        }
        for (d, _, s) in &derived {
            unit(s, Some(d), 1.0, "");
        }
        unit("B", None, 8.0, "bit");
        unit("g", None, 1e-3, "kg");
        let mut prefixable: Vec<&str> = bases.iter().map(|(_, s)| *s).filter(|s| *s != "kg").collect();
        prefixable.extend(derived.iter().map(|(_, _, s)| *s).filter(|s| *s != "bit"));
        prefixable.push("g");
        for u0 in prefixable {
            for (p, f) in SI_PREFIXES {
                unit(&format!("{p}{u0}"), None, f, u0);
            }
        }
        for u0 in ["bit", "B"] {
            for (p, f) in [("Ki", 1024f64), ("Mi", 1024f64.powi(2)), ("Gi", 1024f64.powi(3)), ("Ti", 1024f64.powi(4)), ("Pi", 1024f64.powi(5)), ("Ei", 1024f64.powi(6))] {
                unit(&format!("{p}{u0}"), None, f, u0);
            }
            for (p, f) in SI_PREFIXES {
                if ["k", "M", "G", "T", "P", "E"].contains(&p) {
                    unit(&format!("{p}{u0}"), None, f, u0);
                }
            }
        }
    }

    pub fn load(&self, decls: &[Decl]) {
        let mut seen: HashSet<String> = HashSet::new();
        for d in decls {
            if let Some(n) = d.name() {
                if !matches!(d.body, DeclBody::Unit { .. } | DeclBody::Dimension { .. }) {
                    if !seen.insert(n.to_string()) {
                        self.duplicates.borrow_mut().push(n.to_string());
                    }
                }
            }
            match &d.body {
                DeclBody::Dimension { name, terms } => {
                    if self.dim_decls.borrow().contains_key(name) {
                        self.space_diags.borrow_mut().push(Diag::error(format!("dimension {name} redeclared"), String::new(), Some("E3001")));
                    } else {
                        self.dim_decls.borrow_mut().insert(name.clone(), terms.clone());
                    }
                }
                DeclBody::Unit { name, dim, factor, base } => {
                    if self.unit_decls.borrow().contains_key(name) {
                        self.space_diags.borrow_mut().push(Diag::error(format!("unit {name} redeclared"), String::new(), Some("E4073")));
                    } else {
                        self.unit_order.borrow_mut().push(name.clone());
                        self.unit_decls.borrow_mut().insert(name.clone(), UnitDecl { dim: dim.clone(), factor: factor.clone(), base: base.clone() });
                    }
                }
                DeclBody::Type { name, params, ty, tail } => {
                    if !self.type_asts.borrow().contains_key(name) {
                        self.type_order.borrow_mut().push(name.clone());
                    }
                    self.type_asts.borrow_mut().insert(name.clone(), Rc::new(TypeEntry { ast: ty.clone(), tail: tail.clone(), params: params.clone() }));
                }
                DeclBody::Const { name, ty, expr } => {
                    self.consts.borrow_mut().insert(name.clone(), Rc::new(ConstEntry { expr: expr.clone(), ty: ty.clone(), state: Cell::new(false), value: RefCell::new(Value::Null) }));
                }
                DeclBody::Func { name, params, ret, body } => {
                    self.funcs.borrow_mut().insert(name.clone(), Rc::new(FuncEntry { params: params.clone(), ret: ret.clone(), body: body.clone() }));
                }
                DeclBody::Output { name, ty, expr } => self.outputs.borrow_mut().push((name.clone(), ty.clone(), expr.clone())),
                DeclBody::Input { name, ty, fallback } => {
                    self.inputs.borrow_mut().insert(name.clone(), (ty.clone(), fallback.clone()));
                }
                DeclBody::Diagnostic { name, params, severity, template } => {
                    self.diags.borrow_mut().insert(name.clone(), Rc::new(DiagDecl { params: params.clone(), severity: severity.clone(), template: template.clone() }));
                }
                _ => {}
            }
        }
    }

    pub fn report(&self, d: Diag) {
        self.diagnostics.borrow().borrow_mut().push(d);
    }
    pub fn diagnostics_vec(&self) -> Vec<Diag> {
        self.diagnostics.borrow().borrow().clone()
    }
    pub fn diag_len(&self) -> usize {
        self.diagnostics.borrow().borrow().len()
    }
    pub fn diag_truncate(&self, n: usize) {
        self.diagnostics.borrow().borrow_mut().truncate(n);
    }
    pub fn root(&self, name: &str) -> Option<Value> {
        self.roots.borrow().borrow().iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }
    pub fn set_root(&self, name: &str, v: Value) {
        let rc = self.roots.borrow().clone();
        let mut roots = rc.borrow_mut();
        if let Some(e) = roots.iter_mut().find(|(n, _)| n == name) {
            e.1 = v;
        } else {
            roots.push((name.to_string(), v));
        }
    }
    pub fn root_values(&self) -> Vec<Value> {
        self.roots.borrow().borrow().iter().map(|(_, v)| v.clone()).collect()
    }
    pub fn registry_push(&self, inst: Rc<RefCell<RecInst>>) {
        self.registry.borrow().borrow_mut().push(inst);
    }
    pub fn registry_snapshot(&self) -> Vec<Rc<RefCell<RecInst>>> {
        self.registry.borrow().borrow().clone()
    }

    // §4.13: a named endpoint in a constant position evaluates at elaboration time
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
                diag("E4021", format!("constant {name} is not numeric in a constant position"));
                v.clone()
            }
            Err(Fail::Eval(e)) => {
                let code = if e.msg.contains("zero") { "E5001" } else if e.msg.contains("NaN") || e.msg.contains("Infinity") { "E5002" } else { "E5001" };
                diag(code, format!("evaluating constant {name}: {}", e.msg));
                v.clone()
            }
            Err(_) => v.clone(),
        }
    }

    // ---- unit / dimension name spaces ----
    pub fn resolve_dim(&self, name: &str, visiting: &mut Vec<String>) -> Result<DimVec, String> {
        if let Some(v) = self.dim_memo.borrow().get(name) {
            return Ok(v.clone());
        }
        if visiting.iter().any(|v| v == name) {
            return Err(format!("circular dimension {name}"));
        }
        let decl = self.dim_decls.borrow().get(name).cloned().ok_or_else(|| format!("unknown dimension {name}"))?;
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
        self.dim_memo.borrow_mut().insert(name.to_string(), vec.clone());
        Ok(vec)
    }
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
            let has_dim = self.unit_decls.borrow().get(&sym).map(|u| u.dim.is_some()).unwrap_or(false);
            match self.unit_info(&sym) {
                Ok((key, _)) => {
                    if has_dim {
                        if let Some(prev) = base_seen.get(&key) {
                            out.push(Diag::error(format!("second base unit {sym} for dimension {key} (base is {prev})"), String::new(), Some("E4073")));
                        } else {
                            base_seen.insert(key, sym.clone());
                        }
                    }
                }
                Err(msg) => {
                    let code = if msg.contains("unknown dimension") || msg.contains("circular dimension") { "E3003" } else { "E4073" };
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
            self.base_unit_of.borrow_mut().entry(key.clone()).or_insert_with(|| sym.to_string());
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
        self.unit_memo.borrow_mut().insert(sym.to_string(), info.clone());
        Ok(info)
    }

    // ---- type resolution ----
    pub fn resolve(self: &Rc<Env>, ast: &TypeAst, name: Option<&str>) -> Result<RT, String> {
        Ok(match ast {
            TypeAst::Prim(n) => ty(RTk::Prim(n.clone())),
            TypeAst::Lit(v) => ty(RTk::Lit(v.clone())),
            TypeAst::Range { lo, hi, excl } => {
                let lo = self.const_num(lo);
                let hi = self.const_num(hi);
                let is_f = matches!(lo, Value::Float(_)) || matches!(hi, Value::Float(_));
                ty(RTk::Range { lo, hi, excl: *excl, base: if is_f { "float".into() } else { "int".into() } })
            }
            TypeAst::Pattern(src) => {
                let expanded = self.expand_pattern(src)?;
                let re = Regex::new(&format!("^(?:{expanded})$")).map_err(|e| format!("malformed pattern /{src}/: {e}"))?;
                ty(RTk::Pattern { src: expanded, re })
            }
            TypeAst::Map { key, val } => ty(RTk::Map { key: self.resolve(key, None)?, val: self.resolve(val, None)? }),
            TypeAst::Array { elem, lo, hi, excl } => {
                let lo = lo.as_ref().map(|v| self.const_num(v));
                let hi0 = hi.as_ref().map(|v| self.const_num(v));
                let to_i = |v: &Value| match v {
                    Value::Int(i) => i.to_i64(),
                    Value::Float(f) => Some(*f as i64),
                    _ => None,
                };
                let lo_i = lo.as_ref().and_then(|v| to_i(v));
                let hi_i = hi0.as_ref().and_then(|v| to_i(v)).map(|h| if *excl { h - 1 } else { h });
                ty(RTk::Arr { elem: self.resolve(elem, None)?, lo: lo_i, hi: hi_i })
            }
            TypeAst::Union(arms) => ty(RTk::Union(arms.iter().map(|a| self.resolve(a, None)).collect::<Result<_, _>>()?)),
            TypeAst::Isect(arms) => {
                let arms: Vec<RT> = arms.iter().map(|a| self.resolve(a, None)).collect::<Result<_, _>>()?;
                if arms.iter().all(is_rec) {
                    self.merge_isect(&arms, name)
                } else {
                    ty(RTk::IsectN(arms))
                }
            }
            TypeAst::Record { members, open } => {
                let rt = ty(RTk::Rec(rec_type(*open)));
                *rt.name.borrow_mut() = name.map(|s| s.to_string());
                self.fill_record(&rt, members)?;
                rt
            }
            TypeAst::Func { params, ret } => ty(RTk::Func { params: params.iter().map(|p| self.resolve(p, None)).collect::<Result<_, _>>()?, ret: self.resolve(ret, None)? }),
            TypeAst::Named { name: n, args, preds, ext } => {
                if let Some(preds) = preds {
                    if !preds.is_empty() {
                        let base = self.resolve(&TypeAst::Named { name: n.clone(), args: args.clone(), preds: None, ext: ext.clone() }, name)?;
                        return Ok(ty(RTk::Pred { base, preds: preds.clone() }));
                    }
                }
                if n == "quantity" {
                    let dn = match args.first() {
                        Some(TypeAst::Named { name, .. }) | Some(TypeAst::Prim(name)) => name.clone(),
                        _ => return Err("quantity needs a dimension".into()),
                    };
                    return Ok(ty(RTk::Quantity(key_of_vec(&self.resolve_dim(&dn, &mut vec![])?))));
                }
                if n == "map" && args.len() == 2 {
                    return Ok(ty(RTk::Map { key: self.resolve(&args[0], None)?, val: self.resolve(&args[1], None)? }));
                }
                if n == "ref" {
                    return Ok(ty(RTk::Ref(self.resolve(&args[0], None)?)));
                }
                if ["int", "float", "bool", "string"].contains(&n.as_str()) && args.is_empty() && ext.is_none() {
                    return Ok(ty(RTk::Prim(n.clone())));
                }
                let decl = self.type_asts.borrow().get(n).cloned();
                let Some(decl) = decl else {
                    let im = self.imports.borrow().get(n).cloned();
                    if let Some(im) = im {
                        return im.env.resolve(&TypeAst::Named { name: im.name.clone(), args: args.clone(), preds: None, ext: ext.clone() }, name);
                    }
                    if let Some((ns, rest)) = n.split_once('.') {
                        let ex = self.namespaces.borrow().get(ns).and_then(|(_, exports)| exports.borrow().get(rest).cloned());
                        if let Some(ex) = ex {
                            return ex.env.resolve(&TypeAst::Named { name: ex.name.clone(), args: args.clone(), preds: None, ext: ext.clone() }, name);
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
                    let b = match &decl.ast {
                        TypeAst::Record { members, open } => {
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
                    };
                    b
                };
                if let Some(ext) = ext {
                    let extr = self.resolve(ext, None)?;
                    return Ok(self.extend(&base, &extr));
                }
                base
            }
        })
    }

    fn extend(&self, base: &RT, extr: &RT) -> RT {
        let (RTk::Rec(br), RTk::Rec(er)) = (&base.k, &extr.k) else { return base.clone() };
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
        // an extension may narrow a context declaration (§7.3): its
        // declaration replaces the inherited one for that variable
        let mut ctx_decls: Vec<(String, RT)> = br.ctx_decls.borrow().clone();
        for cd in er.ctx_decls.borrow().iter() {
            if let Some(i) = ctx_decls.iter().position(|(v, _)| *v == cd.0) {
                ctx_decls[i] = cd.clone();
            } else {
                ctx_decls.push(cd.clone());
            }
        }
        let rt = ty(RTk::Rec(RecType { open: br.open, members: RefCell::new(members), asserts: RefCell::new(asserts), ctx_decls: RefCell::new(ctx_decls) }));
        *rt.name.borrow_mut() = base.name.borrow().clone();
        *rt.tail.borrow_mut() = base.tail.borrow().clone();
        rt
    }

    // §3.6: `${T}` inside a pattern splices another type — a string-shaped
    // T (pattern, string literal, union of those) as its regular language,
    // an integer-shaped T (int literal, int range, union) as the decimal
    // representations of its members
    fn expand_pattern(self: &Rc<Env>, re: &str) -> Result<String, String> {
        let hole = Regex::new(r"\$\{([^}]*)\}").unwrap();
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
            let str_lit = Regex::new(r#"^"((?:[^"\\]|\\.)*)"$"#).unwrap();
            let int_range = Regex::new(r"^(-?[0-9]+)\.\.(<?)(-?[0-9]+)$").unwrap();
            let int_lit = Regex::new(r"^-?[0-9]+$").unwrap();
            let ident = Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*$").unwrap();
            for arm in &arms {
                if str_lit.is_match(arm) {
                    let v = crate::parse::json_unquote(arm)?;
                    frags.push(self.pattern_fragment(&ty(RTk::Lit(Value::Str(v))), &text)?);
                    continue;
                }
                if let Some(c) = int_range.captures(arm) {
                    let lo = c[1].parse::<BigInt>().map_err(|e| e.to_string())?;
                    let hi = c[3].parse::<BigInt>().map_err(|e| e.to_string())?;
                    let rt = ty(RTk::Range { lo: Value::Int(lo), hi: Value::Int(hi), excl: &c[2] == "<", base: "int".into() });
                    frags.push(self.pattern_fragment(&rt, &text)?);
                    continue;
                }
                if int_lit.is_match(arm) {
                    let v = arm.parse::<BigInt>().map_err(|e| e.to_string())?;
                    frags.push(self.pattern_fragment(&ty(RTk::Lit(Value::Int(v))), &text)?);
                    continue;
                }
                if !ident.is_match(arm) {
                    return Err(format!("pattern interpolation of {text}: not a type (§3.6)"));
                }
                if self.pattern_visiting.borrow().iter().any(|v| v == arm) {
                    return Err(format!("pattern interpolation of {arm} is circular"));
                }
                self.pattern_visiting.borrow_mut().push(arm.clone());
                let resolved = self.resolve(&TypeAst::Named { name: arm.clone(), args: vec![], preds: None, ext: None }, None);
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
        let bad = || Err(format!("pattern interpolation of {name}: type is neither string- nor integer-shaped (§3.6)"));
        match &rt.k {
            RTk::Pattern { src, .. } => Ok(format!("(?:{src})")),
            RTk::Lit(Value::Str(s)) => Ok(esc(s)),
            RTk::Lit(Value::Int(i)) => Ok(i.to_string()),
            RTk::Lit(_) => bad(),
            RTk::Range { lo, hi, excl, base } => {
                let (Value::Int(lo), Value::Int(hi)) = (lo, hi) else { return bad() };
                if base != "int" {
                    return bad();
                }
                let hi = if *excl { hi - 1 } else { hi.clone() };
                if &hi - lo >= BigInt::from(65536) {
                    return Err(format!("pattern interpolation of {name}: range too large (limit 65536 values)"));
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
                let parts = arms.iter().map(|a| self.pattern_fragment(a, name)).collect::<Result<Vec<_>, _>>()?;
                Ok(format!("(?:{})", parts.join("|")))
            }
            RTk::Pred { base, .. } => self.pattern_fragment(base, name),
            RTk::Prim(n) if n == "string" => Ok(".*".into()),
            RTk::Prim(n) if n == "int" => Ok("-?[0-9]+".into()),
            _ => bad(),
        }
    }

    // §3.15 generics
    fn instantiate(self: &Rc<Env>, name: &str, args: &[TypeAst], decl: &Rc<TypeEntry>) -> Result<RT, String> {
        let ps = &decl.params;
        if args.len() != ps.len() {
            return Err(format!("generic arity: {name} expects {} argument(s), got {}", ps.len(), args.len()));
        }
        let mut types: HashMap<String, TypeAst> = HashMap::new();
        let mut values: HashMap<String, Value> = HashMap::new();
        let mut label = Vec::new();
        for (p, a) in ps.iter().zip(args) {
            if let Some(pty) = &p.ty {
                let v = match a {
                    TypeAst::Lit(v) => v.clone(),
                    TypeAst::Named { name: an, args: aa, ext: None, preds: None } if aa.is_empty() => {
                        let v = self.const_num(&Value::Str(an.clone()));
                        if matches!(v, Value::Str(_)) {
                            return Err(format!("non-constant value argument {an} for {} of {name}", p.name));
                        }
                        v
                    }
                    _ => return Err(format!("generic arity: parameter {} of {name} takes a constant value", p.name)),
                };
                let bound = self.resolve(&subst_type(pty, &types, &values), None)?;
                if !crate::subsume::subsumes(self, &ty(RTk::Lit(v.clone())), &bound) {
                    return Err(format!("value argument {v:?} outside parameter {}'s type in {name}", p.name));
                }
                label.push(format!("{v:?}"));
                values.insert(p.name.clone(), v);
            } else {
                label.push(match a {
                    TypeAst::Named { name, .. } | TypeAst::Prim(name) => name.clone(),
                    _ => "type".into(),
                });
                types.insert(p.name.clone(), a.clone());
            }
        }
        let key = format!("{name}<{}>", args.iter().map(type_key).collect::<Vec<_>>().join(","));
        if let Some(rt) = self.type_memo.borrow().get(&key).cloned() {
            return Ok(rt);
        }
        let shown = format!("{name}<{}>", label.join(", "));
        let body = subst_type(&decl.ast, &types, &values);
        let rt = match &body {
            TypeAst::Record { members, open } => {
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
        for m in members {
            match m {
                MemberAst::Value { name, opt, ty: t, dflt } => r.members.borrow_mut().push(Member {
                    kind: if dflt.is_some() { MKind::Dflt } else if *opt { MKind::Opt } else { MKind::Req },
                    name: name.clone(),
                    ty: Some(self.resolve(t, None)?),
                    conj: None,
                    dflt: dflt.clone(),
                    expr: None,
                    menv: Some(self.clone()),
                }),
                MemberAst::Derived { name, ty: t, expr } => r.members.borrow_mut().push(Member {
                    kind: MKind::Der,
                    name: name.clone(),
                    ty: match t { Some(t) => Some(self.resolve(t, None)?), None => None },
                    conj: None,
                    dflt: None,
                    expr: Some(expr.clone()),
                    menv: Some(self.clone()),
                }),
                MemberAst::Assert { name, cond, tail } => r.asserts.borrow_mut().push(AssertItem {
                    when: false, name: name.clone(), cond: cond.clone(), tail: tail.clone(), body: vec![], origin: origin.clone(), menv: Some(self.clone()),
                }),
                MemberAst::When { cond, body } => r.asserts.borrow_mut().push(AssertItem {
                    when: true, name: String::new(), cond: cond.clone(), tail: None, body: body.clone(), origin: origin.clone(), menv: Some(self.clone()),
                }),
                MemberAst::Context { variable, ty: t } => r.ctx_decls.borrow_mut().push((variable.clone(), self.resolve(t, None)?)),
            }
        }
        Ok(())
    }

    fn merge_isect(&self, arms: &[RT], name: Option<&str>) -> RT {
        let mut members: Vec<Member> = vec![];
        let mut asserts: Vec<AssertItem> = vec![];
        let mut open = true;
        for a in arms {
            let RTk::Rec(r) = &a.k else { continue };
            open = open && r.open;
            for m in r.members.borrow().iter() {
                if let Some(i) = members.iter().position(|x| x.name == m.name) {
                    let prev = members[i].clone();
                    let mut conj = prev.conj.clone().unwrap_or_else(|| prev.ty.iter().cloned().collect());
                    if let Some(t) = &m.ty {
                        conj.push(t.clone());
                    }
                    members[i] = Member { conj: Some(conj), kind: if m.kind == MKind::Req { MKind::Req } else { prev.kind }, ..prev };
                } else {
                    members.push(m.clone());
                }
            }
            asserts.extend(r.asserts.borrow().iter().map(|x| AssertItem { origin: x.origin.clone().or_else(|| a.name.borrow().clone()), ..x.clone() }));
        }
        let rt = ty(RTk::Rec(RecType { open, members: RefCell::new(members), asserts: RefCell::new(asserts), ctx_decls: RefCell::new(vec![]) }));
        *rt.name.borrow_mut() = name.map(|s| s.to_string());
        rt
    }
}

fn type_key(t: &TypeAst) -> String {
    match t {
        TypeAst::Prim(n) => format!("p:{n}"),
        TypeAst::Lit(v) => format!("l:{v:?}"),
        TypeAst::Named { name, args, .. } => format!("n:{name}<{}>", args.iter().map(type_key).collect::<Vec<_>>().join(",")),
        TypeAst::Range { lo, hi, excl } => format!("r:{lo:?}..{excl}{hi:?}"),
        TypeAst::Array { elem, lo, hi, .. } => format!("a:{}[{lo:?},{hi:?}]", type_key(elem)),
        TypeAst::Union(a) => format!("u:{}", a.iter().map(type_key).collect::<Vec<_>>().join("|")),
        TypeAst::Isect(a) => format!("i:{}", a.iter().map(type_key).collect::<Vec<_>>().join("&")),
        TypeAst::Map { key, val } => format!("m:{}:{}", type_key(key), type_key(val)),
        TypeAst::Pattern(p) => format!("pat:{p}"),
        TypeAst::Record { members, .. } => format!("rec:{}", members.len()),
        TypeAst::Func { params, ret } => format!("f:{}->{}", params.len(), type_key(ret)),
    }
}

// ---------------- generic substitution ----------------
pub fn subst_type(ast: &TypeAst, types: &HashMap<String, TypeAst>, values: &HashMap<String, Value>) -> TypeAst {
    let t = |a: &TypeAst| subst_type(a, types, values);
    match ast {
        TypeAst::Named { name, args, preds, ext } => {
            let plain = args.is_empty() && ext.is_none() && preds.is_none();
            if plain {
                if let Some(x) = types.get(name) {
                    return x.clone();
                }
                if let Some(v) = values.get(name) {
                    return TypeAst::Lit(v.clone());
                }
            }
            TypeAst::Named {
                name: name.clone(),
                args: args.iter().map(t).collect(),
                preds: preds.as_ref().map(|ps| ps.iter().map(|p| subst_expr(p, values)).collect()),
                ext: ext.as_ref().map(|e| Box::new(t(e))),
            }
        }
        TypeAst::Range { lo, hi, excl } => {
            let sub = |v: &Value| match v {
                Value::Str(s) if values.contains_key(s) => values[s].clone(),
                other => other.clone(),
            };
            TypeAst::Range { lo: sub(lo), hi: sub(hi), excl: *excl }
        }
        TypeAst::Array { elem, lo, hi, excl } => {
            let sub = |v: &Value| match v {
                Value::Str(s) if values.contains_key(s) => values[s].clone(),
                other => other.clone(),
            };
            TypeAst::Array { elem: Box::new(t(elem)), lo: lo.as_ref().map(sub), hi: hi.as_ref().map(sub), excl: *excl }
        }
        TypeAst::Record { members, open } => TypeAst::Record { members: members.iter().map(|m| subst_member(m, types, values)).collect(), open: *open },
        TypeAst::Map { key, val } => TypeAst::Map { key: Box::new(t(key)), val: Box::new(t(val)) },
        TypeAst::Union(a) => TypeAst::Union(a.iter().map(t).collect()),
        TypeAst::Isect(a) => TypeAst::Isect(a.iter().map(t).collect()),
        TypeAst::Func { params, ret } => TypeAst::Func { params: params.iter().map(t).collect(), ret: Box::new(t(ret)) },
        other => other.clone(),
    }
}
fn subst_member(m: &MemberAst, types: &HashMap<String, TypeAst>, values: &HashMap<String, Value>) -> MemberAst {
    let t = |a: &TypeAst| subst_type(a, types, values);
    match m {
        MemberAst::Value { name, opt, ty, dflt } => MemberAst::Value { name: name.clone(), opt: *opt, ty: t(ty), dflt: dflt.as_ref().map(|d| subst_expr(d, values)) },
        MemberAst::Derived { name, ty, expr } => MemberAst::Derived { name: name.clone(), ty: ty.as_ref().map(t), expr: subst_expr(expr, values) },
        MemberAst::Context { variable, ty } => MemberAst::Context { variable: variable.clone(), ty: t(ty) },
        MemberAst::Assert { name, cond, tail } => MemberAst::Assert { name: name.clone(), cond: subst_expr(cond, values), tail: tail.clone() },
        MemberAst::When { cond, body } => MemberAst::When { cond: subst_expr(cond, values), body: body.iter().map(|b| subst_member(b, types, values)).collect() },
    }
}
pub fn subst_expr(e: &Rc<Expr>, values: &HashMap<String, Value>) -> Rc<Expr> {
    if values.is_empty() {
        return e.clone();
    }
    let s = |x: &Rc<Expr>| subst_expr(x, values);
    let cls = |c: &ForClause| ForClause { v: c.v.clone(), iter: s(&c.iter), filters: c.filters.iter().map(s).collect() };
    Rc::new(match &**e {
        Expr::Name(n) if values.contains_key(n) => Expr::Lit(values[n].clone()),
        Expr::Template(parts) => Expr::Template(parts.iter().map(|p| match p { TPart::Expr(x) => TPart::Expr(s(x)), other => other.clone() }).collect()),
        Expr::Obj(es) => Expr::Obj(es.iter().map(|(k, v)| (k.clone(), s(v))).collect()),
        Expr::Arr(items) => Expr::Arr(items.iter().map(|(sp, v)| (*sp, s(v))).collect()),
        Expr::Comp { head, clauses } => Expr::Comp { head: s(head), clauses: clauses.iter().map(cls).collect() },
        Expr::MapComp { key, val, clauses } => Expr::MapComp { key: s(key), val: s(val), clauses: clauses.iter().map(cls).collect() },
        Expr::Bin { op, l, r } => Expr::Bin { op: op.clone(), l: s(l), r: s(r) },
        Expr::Un { op, x } => Expr::Un { op: op.clone(), x: s(x) },
        Expr::Paren(x) => Expr::Paren(s(x)),
        Expr::If { c, t, f } => Expr::If { c: s(c), t: s(t), f: s(f) },
        Expr::Lambda { params, body } => Expr::Lambda { params: params.clone(), body: s(body) },
        Expr::Call { fun, args } => Expr::Call { fun: s(fun), args: args.iter().map(s).collect() },
        Expr::Member { x, name, safe } => Expr::Member { x: s(x), name: name.clone(), safe: *safe },
        Expr::Index { x, i } => Expr::Index { x: s(x), i: s(i) },
        Expr::With { base, patch } => Expr::With { base: s(base), patch: s(patch) },
        Expr::Match { subject, arms } => Expr::Match { subject: s(subject), arms: arms.iter().map(|a| MatchArm { v: a.v.clone(), ty: a.ty.clone(), body: s(&a.body) }).collect() },
        other => other.clone(),
    })
}

// ---------------- helpers ----------------
pub fn path_str(segs: &[Seg], rel_root: Option<&str>) -> String {
    let id_re = Regex::new(r"^[_A-Za-z][_A-Za-z0-9]*$").unwrap();
    let mut out = String::new();
    for (i, s) in segs.iter().enumerate() {
        match s {
            Seg::Name(n) if i == 0 => {
                if rel_root == Some(n.as_str()) {
                    out.push('$');
                } else {
                    out.push_str(n);
                }
            }
            Seg::Idx(k) if i == 0 => out.push_str(&k.to_string()),
            Seg::Idx(k) => out.push_str(&format!("[{k}]")),
            Seg::Name(n) if id_re.is_match(n) => {
                out.push('.');
                out.push_str(n);
            }
            Seg::Name(n) => out.push_str(&format!("[{}]", json_str(n))),
        }
    }
    out
}

pub fn parse_path(s: &str, root_name: &str) -> R<SegPath> {
    let id_re = Regex::new(r"^[_A-Za-z][_A-Za-z0-9]*").unwrap();
    let mut segs = vec![];
    let mut i = if s.starts_with('$') {
        segs.push(Seg::Name(root_name.to_string()));
        1
    } else {
        let m = id_re.find(s).ok_or(()).or_else(|_| err(format!("bad path {s}")))?;
        segs.push(Seg::Name(m.as_str().to_string()));
        m.end()
    };
    while i < s.len() {
        let rest = &s[i..];
        if let Some(r) = rest.strip_prefix('.') {
            let m = id_re.find(r).ok_or(()).or_else(|_| err(format!("bad path {s}")))?;
            segs.push(Seg::Name(m.as_str().to_string()));
            i += 1 + m.end();
        } else if rest.starts_with('[') {
            let j = rest.find(']').ok_or(()).or_else(|_| err(format!("bad path {s}")))?;
            let inner = &rest[1..j];
            if inner.starts_with('"') {
                segs.push(Seg::Name(crate::parse::json_unquote(inner).unwrap_or_default()));
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

pub fn cmp_path(a: &[Seg], b: &[Seg]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        match (x, y) {
            (Seg::Idx(i), Seg::Idx(j)) => {
                if i != j {
                    return i.cmp(j);
                }
            }
            _ => {
                let xs = seg_str(x);
                let ys = seg_str(y);
                if xs != ys {
                    return xs.cmp(&ys);
                }
            }
        }
    }
    a.len().cmp(&b.len())
}
fn seg_str(s: &Seg) -> String {
    match s {
        Seg::Name(n) => n.clone(),
        Seg::Idx(i) => i.to_string(),
    }
}

pub fn value_eq(a: &Value, b: &Value) -> bool {
    let (pa, pb) = (a.place(), b.place());
    if (matches!(a, Value::Ref(_)) || matches!(b, Value::Ref(_))) && pa.is_some() && pb.is_some() {
        return cmp_path(&pa.unwrap(), &pb.unwrap()) == std::cmp::Ordering::Equal;
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
            x.items.len() == y.items.len() && x.items.iter().zip(&y.items).all(|(p, q)| value_eq(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.entries.len() == y.entries.len() && x.entries.iter().all(|(k, v)| y.get(k).map(|w| value_eq(v, w)).unwrap_or(false))
        }
        (Value::Rec(x), Value::Rec(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            for (n, s) in &x.slots {
                let v1 = if s.state == SlotState::Absent { Value::Absent } else { s.value.clone() };
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
                let m = re.captures(rest).ok_or(()).or_else(|_| err(format!("bad JSON at {i}")))?;
                let whole = m.get(0).unwrap().as_str();
                *i += whole.len();
                if m.get(1).is_some() || m.get(2).is_some() {
                    Ok(Value::Float(whole.parse::<f64>().unwrap_or(0.0)))
                } else {
                    Ok(Value::Int(whole.parse::<BigInt>().unwrap_or_else(|_| BigInt::zero())))
                }
            }
        }
    }
    let v = val(src, b, &mut i)?;
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
        let mant = if k > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits.to_string() };
        format!("{mant}e{}{}", if e > 0 { "+" } else { "-" }, e.abs())
    };
    if x < 0.0 { format!("-{body}") } else { body }
}

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
