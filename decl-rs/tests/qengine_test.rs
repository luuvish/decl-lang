//! The query engine's incremental core (src/qengine/db.rs): memoization,
//! dependency tracking, and the two early cutoffs. Recompute counts prove the
//! cutoffs. The Rust counterpart of decl-ts/tests/qengine/db_test.ts.
use decl_lang::parse::parse_source;
use decl_lang::pipeline::{evaluate_source, Phase};
use decl_lang::qengine::db::{Db, DbErr};
use decl_lang::qengine::qeval::qevaluate;
use decl_lang::semantics::{Env, Num, Value};
use std::cell::Cell;
use std::rc::Rc;

fn int(n: i64) -> Value {
    Value::Int(Num::from(n))
}
fn as_int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => n.to_i64().expect("small int"),
        _ => panic!("not an int: {v:?}"),
    }
}
fn ok(r: Result<Value, DbErr>) -> Value {
    match r {
        Ok(v) => v,
        Err(_) => panic!("query failed"),
    }
}

#[test]
fn memoize_and_cutoffs() {
    // sum = a + b ; doubled = sum * 2 ; sign = sign(a)
    let calls = Rc::new((Cell::new(0i32), Cell::new(0i32), Cell::new(0i32))); // sum, doubled, sign
    let c = calls.clone();
    let db = Rc::new(Db::new(Box::new(move |k: &str| -> Option<_> {
        let c = c.clone();
        match k {
            "sum" => Some(Box::new(move |db: &Db| {
                c.0.set(c.0.get() + 1);
                Ok(int(as_int(&db.query("a")?) + as_int(&db.query("b")?)))
            }) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>),
            "doubled" => Some(Box::new(move |db: &Db| {
                c.1.set(c.1.get() + 1);
                Ok(int(as_int(&db.query("sum")?) * 2))
            })),
            "sign" => Some(Box::new(move |db: &Db| {
                c.2.set(c.2.get() + 1);
                Ok(int(as_int(&db.query("a")?).signum()))
            })),
            _ => None,
        }
    })));

    db.set_input("a", int(1));
    db.set_input("b", int(2));
    assert_eq!(as_int(&ok(db.query("doubled"))), 6);
    assert_eq!(
        (calls.0.get(), calls.1.get(), calls.2.get()),
        (1, 1, 0),
        "only the demanded chain computes"
    );

    ok(db.query("doubled")); // nothing changed
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (1, 1),
        "no change recomputes nothing"
    );

    db.set_input("b", int(10)); // sum and doubled both change
    assert_eq!(as_int(&ok(db.query("doubled"))), 22);
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (2, 2),
        "a real change recomputes the chain"
    );

    // a and b change but their sum does not: sum recomputes, its value is
    // unchanged, so doubled cuts off (value cutoff propagates)
    db.set_input("a", int(5));
    db.set_input("b", int(6)); // 5 + 6 == 11, the same sum as 1 + 10
    assert_eq!(as_int(&ok(db.query("doubled"))), 22);
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (3, 2),
        "value cutoff stops the dependent"
    );
}

#[test]
fn verify_cutoff_unrelated_input() {
    let sum = Rc::new(Cell::new(0i32));
    let s = sum.clone();
    let db = Db::new(Box::new(move |k: &str| -> Option<_> {
        if k == "sum" {
            let s = s.clone();
            Some(Box::new(move |db: &Db| {
                s.set(s.get() + 1);
                Ok(int(as_int(&db.query("a")?) + as_int(&db.query("b")?)))
            }) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>)
        } else {
            None
        }
    }));
    db.set_input("a", int(1));
    db.set_input("b", int(2));
    db.set_input("c", int(9));
    ok(db.query("sum"));
    db.set_input("c", int(99)); // sum never read c
    assert_eq!(as_int(&ok(db.query("sum"))), 3);
    assert_eq!(sum.get(), 1, "an unrelated input recomputes nothing");
}

#[test]
fn a_query_that_reads_itself_is_a_cycle() {
    let db = Db::new(Box::new(|k: &str| -> Option<_> {
        match k {
            "x" => {
                Some(Box::new(|db: &Db| db.query("y")) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>)
            }
            "y" => Some(Box::new(|db: &Db| db.query("x"))),
            _ => None,
        }
    }));
    match db.query("x") {
        Err(DbErr::Cycle(_)) => {}
        other => panic!("expected a cycle, got {:?}", other.is_ok()),
    }
}

// ---- differential: the query engine vs the tree walker (grows per stage) ----

/// run a program through both engines; None = skip (parse/checker-decided, or a
/// form the query engine does not compile yet); Some(true) = byte-identical
fn same(src: &str) -> Option<bool> {
    let reference = evaluate_source(src);
    if !matches!(reference.phase, Phase::Evaluate) {
        return None; // the parser or checker decided it, not the evaluator
    }
    let parsed = parse_source(src);
    if !parsed.errors.is_empty() {
        return None;
    }
    let env = Env::new();
    env.load(&parsed.decls);
    let got = qevaluate(env).ok()?; // Unsupported form -> skip
    Some(reference.outputs == got.outputs && reference.ok == got.ok)
}

#[test]
fn diff_scalars_and_consts() {
    let programs = [
        "export output x: int = 1 + 2 * 3 - 4",
        "export output x: int = (1 + 2) * (3 - 5)",
        "export output x: float = 7.0 / 2.0",
        "export output x: int = 7 / 2",
        "export output x: int = 17 % 5",
        "export output x: int = -(~3)",
        "export output x: int = (5 & 3) | (1 << 3)",
        "export output x: bool = 3 < 5",
        "export output x: int = if 3 < 5 then 10 else 20",
        "export output x: bool = true && (false || (1 == 1))",
        "export output x: string = \"a\" + \"b\" + \"c\"",
        "const a = 2\nconst b = a * 5\nexport output x: int = b + a",
        "const base = 10\nconst step = base / 2\nexport output x: int = base + step * 3",
        "export output x: int = 10 / 0",
        "export output x: int = 10 % 0",
    ];
    let mut matched = 0;
    for src in programs {
        match same(src) {
            Some(true) => matched += 1,
            Some(false) => panic!("query engine diverged on:\n{src}"),
            None => {}
        }
    }
    assert!(
        matched >= 13,
        "expected most scalar programs to match, got {matched}"
    );
}

#[test]
fn diff_records() {
    let programs = [
        "type Point = { x: int, y: int, sum = x + y, scaled = sum * 2 }\nexport output p: Point = { x: 3, y: 4 }",
        "const k = 100\ntype R = { base: int, total = base + k }\nexport output r: R = { base: 5 }",
        "type T = { n: int, name: string, doubled = n * 2, tag = name }\nexport output t: T = { n: 21, name: \"hi\" }",
        "type C = { a: int, b = a + 1, c = b + 1, d = c + a }\nexport output c: C = { a: 10 }",
        "type Inner = { a: int, b: int }\ntype Outer = { inner: Inner, total = inner.a + inner.b }\nexport output o: Outer = { inner: { a: 3, b: 4 } }",
        "type L2 = { v: int }\ntype L1 = { deep: L2, twice = deep.v * 2 }\ntype Top = { mid: L1, plus = mid.deep.v + mid.twice }\nexport output t: Top = { mid: { deep: { v: 5 } } }",
        "type R = { name: string, nick?: string }\nexport output r: R = { name: \"a\", nick: \"b\" }",
        "type R = { name: string, nick?: string }\nexport output r: R = { name: \"a\" }",
        "type R = { name: string, retries?: int = 3 }\nexport output r: R = { name: \"a\" }",
        "type R = { name: string, retries?: int = 3 }\nexport output r: R = { name: \"a\", retries: 5 }",
        "type R = { name: string, nick?: string, display = nick ?? \"none\" }\nexport output r: R = { name: \"a\" }",
    ];
    let mut matched = 0;
    for src in programs {
        match same(src) {
            Some(true) => matched += 1,
            Some(false) => panic!("query engine diverged on:\n{src}"),
            None => {}
        }
    }
    assert!(
        matched >= 10,
        "expected most record programs to match, got {matched}"
    );
}

#[test]
fn diff_collections_and_control() {
    let programs = [
        // arrays, spreads, indexing
        "export output x: int[] = [1, 2, 3]",
        "const ys = [10, 20]\nexport output x: int[] = [0, ...ys, 30]",
        "export output x: int = [5, 6, 7][1]",
        "export output x: bool = 2 in [1, 2, 3]",
        "export output x: bool = 4 in [1, 2, 3]",
        "export output x: bool = 3 in 1..5",
        "export output x: bool = 5 in 1..<5",
        // comprehensions
        "export output x: int[] = [n * n for n in 1..3]",
        "export output x: int[] = [n for n in 1..10 if n % 2 == 0]",
        "export output x: int[] = [a * b for a in 1..2 for b in 1..3]",
        // maps
        "export output x: map<string, int> = { \"a\": 1, \"b\": 2 }",
        "export output x: int = ({ \"a\": 1, \"b\": 2 })[\"b\"]",
        "export output x: map<string, int> = { `k${n}`: n * 10 for n in 1..3 }",
        // templates
        "const who = \"world\"\nexport output x: string = `hello, ${who}!`",
        "export output x: string = `sum is ${1 + 2}`",
        // match over a discriminable union (literals and records)
        "type L = \"low\" | \"mid\" | \"high\"\nconst pick: L = \"mid\"\nexport output x: int = match pick {\n    (l: \"low\") => 1\n    (l: \"mid\") => 2\n    (l: \"high\") => 3\n}",
        "type C = { kind: \"c\", r: int }\ntype R = { kind: \"r\", w: int, h: int }\ntype S = C | R\nconst s: S = { kind: \"c\", r: 5 }\nexport output x: int = match s {\n    (c: C) => c.r * c.r\n    (r: R) => r.w * r.h\n}",
        // pattern / matches
        "export output x: bool = \"abc\" matches /a.c/",
        // ctx / references inside records
        "type P = { x: int, here = $path }\nexport output p: P = { x: 1 }",
        "type P = { x: int, me = $this }\nexport output p: P = { x: 1 }",
        // records inside arrays / maps
        "type Pt = { x: int, y: int, s = x + y }\nexport output ps: Pt[] = [{ x: 1, y: 2 }, { x: 3, y: 4 }]",
    ];
    // these forms are all implemented, so every one must run through the query
    // engine (not fall back) and match the tree walker byte for byte
    for src in programs {
        match same(src) {
            Some(true) => {}
            Some(false) => panic!("query engine diverged on:\n{src}"),
            None => panic!("query engine unexpectedly fell back on:\n{src}"),
        }
    }
}

#[test]
fn diff_calls_pipes_and_with() {
    let programs = [
        // module function, call, first-argument pipeline
        "func double(n: int): int = n * 2\nexport output x: int = double(21)",
        "func add(a: int, b: int): int = a + b\nexport output x: int = add(add(1, 2), 3)",
        "export output x: int = 21 |> double\nfunc double(n: int): int = n * 2",
        // stdlib calls (a std path callee, not a data access)
        "export output x: int = std.math.min(3, 7)",
        "export output x: int = std.array.sum([1, 2, 3, 4])",
        "export output x: int[] = std.array.sort([3, 1, 2])",
        "export output x: string = std.string.join([\"a\", \"b\", \"c\"], \"-\")",
        // lambdas: as std arguments, in a pipeline, and immediately applied
        "export output x: int = std.array.fold([1, 2, 3, 4], 0, (acc, n) => acc + n)",
        "export output x: int = [1, 2, 3, 4] |> std.array.filter((n) => n % 2 == 0) |> std.array.count",
        "export output x: int = ((n) => n * 3)(14)",
        // `with` over a record-valued base
        "type P = { x: int, y: int }\nconst base: P = { x: 1, y: 2 }\nexport output p: P = base with { y: 9 }",
        // an input read through its fallback
        "input n: int = 7\nexport output x: int = n + 1",
    ];
    for src in programs {
        match same(src) {
            Some(true) => {}
            Some(false) => panic!("query engine diverged on:\n{src}"),
            None => panic!("query engine unexpectedly fell back on:\n{src}"),
        }
    }
}
