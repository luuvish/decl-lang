"""The query engine's incremental core (decl/qengine/db.py): memoization,
dependency tracking, and the two early cutoffs, proven by recompute counts; and
a differential of decl/qengine/qeval.py against the tree walker (run_pipeline).
The Python counterpart of decl-rs/tests/qengine_test.rs."""

from __future__ import annotations

from decl.parse import parse_source
from decl.pipeline import evaluate_source
from decl.qengine.db import Db, QCycle
from decl.qengine.qeval import Unsupported, qevaluate
from decl.semantics import Env


def test_memoize_and_cutoffs() -> None:
    # sum = a + b ; doubled = sum * 2 ; sign = sign(a)
    calls = {"sum": 0, "doubled": 0, "sign": 0}

    def resolve(k):
        if k == "sum":

            def c(db):
                calls["sum"] += 1
                return db.query("a") + db.query("b")

            return c
        if k == "doubled":

            def c(db):
                calls["doubled"] += 1
                return db.query("sum") * 2

            return c
        if k == "sign":

            def c(db):
                calls["sign"] += 1
                return (db.query("a") > 0) - (db.query("a") < 0)

            return c
        return None

    db = Db(resolve)
    db.set_input("a", 1)
    db.set_input("b", 2)
    assert db.query("doubled") == 6
    assert (calls["sum"], calls["doubled"], calls["sign"]) == (1, 1, 0)

    db.query("doubled")  # nothing changed
    assert (calls["sum"], calls["doubled"]) == (1, 1)

    db.set_input("b", 10)  # sum and doubled both change
    assert db.query("doubled") == 22
    assert (calls["sum"], calls["doubled"]) == (2, 2)

    # a and b change but their sum does not: sum recomputes, its value is
    # unchanged, so doubled cuts off (value cutoff propagates)
    db.set_input("a", 5)
    db.set_input("b", 6)  # 5 + 6 == 11, the same sum as 1 + 10
    assert db.query("doubled") == 22
    assert (calls["sum"], calls["doubled"]) == (3, 2), "value cutoff stops the dependent"


def test_verify_cutoff_unrelated_input() -> None:
    n = {"sum": 0}

    def resolve(k):
        if k == "sum":

            def c(db):
                n["sum"] += 1
                return db.query("a") + db.query("b")

            return c
        return None

    db = Db(resolve)
    db.set_input("a", 1)
    db.set_input("b", 2)
    db.set_input("c", 9)
    db.query("sum")
    db.set_input("c", 99)  # sum never read c
    assert db.query("sum") == 3
    assert n["sum"] == 1, "an unrelated input recomputes nothing"


def test_a_query_that_reads_itself_is_a_cycle() -> None:
    def resolve(k):
        if k == "x":
            return lambda db: db.query("y")
        if k == "y":
            return lambda db: db.query("x")
        return None

    db = Db(resolve)
    try:
        db.query("x")
        raise AssertionError("expected a cycle")
    except QCycle:
        pass


# ---- differential: the query engine vs the tree walker ----


def _same(src: str):
    """run a program through both engines; None = skip (parse/checker-decided, or
    a form the query engine does not compile yet); True = byte-identical"""
    reference = evaluate_source(src)
    if reference["phase"] != "evaluate":
        return None
    parsed = parse_source(src)
    if parsed["errors"]:
        return None
    env = Env()
    env.load(parsed["decls"])
    try:
        rep = qevaluate(env)
    except Unsupported:
        return None
    ref_out = [(o["name"], o["json"]) for o in reference["outputs"]]
    q_out = [(o["name"], o["json"]) for o in rep.outputs]
    return ref_out == q_out and reference["ok"] == rep.ok


def _all_match(programs: list[str]) -> None:
    for src in programs:
        r = _same(src)
        assert r is not False, f"query engine diverged on:\n{src}"
        assert r is not None, f"query engine unexpectedly fell back on:\n{src}"


def test_diff_scalars_and_records() -> None:
    _all_match(
        [
            "export output x: int = 1 + 2 * 3 - 4",
            "export output x: float = 7.0 / 2.0",
            "export output x: int = 17 % 5",
            "export output x: int = -(~3)",
            "export output x: bool = true && (false || (1 == 1))",
            "const a = 2\nconst b = a * 5\nexport output x: int = b + a",
            "export output x: int = 10 / 0",
            "type Point = { x: int, y: int, sum = x + y, scaled = sum * 2 }\n"
            "export output p: Point = { x: 3, y: 4 }",
            'type R = { name: string, retries?: int = 3 }\nexport output r: R = { name: "a" }',
            "type Inner = { a: int, b: int }\n"
            "type Outer = { inner: Inner, total = inner.a + inner.b }\n"
            "export output o: Outer = { inner: { a: 3, b: 4 } }",
        ]
    )


def test_diff_collections_calls_and_with() -> None:
    _all_match(
        [
            "export output x: int[] = [1, 2, 3]",
            "export output x: int = [5, 6, 7][1]",
            "export output x: int[] = [n * n for n in 1..3]",
            'export output x: map<string, int> = { "a": 1, "b": 2 }',
            "export output x: map<string, int> = { `k${n}`: n * 10 for n in 1..3 }",
            'const who = "world"\nexport output x: string = `hello, ${who}!`',
            'export output x: bool = "abc" matches /a.c/',
            "func double(n: int): int = n * 2\nexport output x: int = double(21)",
            "export output x: int = std.array.sum([1, 2, 3, 4])",
            "export output x: int = [1, 2, 3, 4] |> std.array.filter((n) => n % 2 == 0) "
            "|> std.array.count",
            "type P = { x: int, y: int }\nconst base: P = { x: 1, y: 2 }\n"
            "export output p: P = base with { y: 9 }",
            "input n: int = 7\nexport output x: int = n + 1",
            'type C = { kind: "c", r: int }\ntype R = { kind: "r", w: int, h: int }\n'
            'type S = C | R\nconst s: S = { kind: "c", r: 5 }\n'
            "export output x: int = match s {\n (c: C) => c.r * c.r\n (r: R) => r.w * r.h\n}",
        ]
    )


def test_diff_referrers_rounds() -> None:
    # $referrers is answered in rounds over a frozen previous universe (§7.6):
    # the port's inbound$ and the link's target co-depend, settling after a round
    src = (
        "type Port = {\n"
        "    $key: string\n"
        '    inbound$ = $referrers(Link, "target")\n'
        "    open = std.array.count(inbound$) < 100\n"
        "    degree: int = std.array.count(inbound$)\n"
        "}\n"
        "type Link = {\n"
        "    $parent: ref<{ ports: { [string]: Port }, ... }>\n"
        "    name: string\n"
        "    target: ref<Port> =\n"
        "        if ($parent.ports[name]?.open ?? false) then $parent.ports[name] "
        'else $parent.ports["spare"]\n'
        "}\n"
        "type Hub = {\n"
        "    ports: { [string]: Port } = { a: {}, spare: {} }\n"
        '    links: Link[] = [{ name: "a" }]\n'
        "}\n"
        "export output hub: Hub = {}"
    )
    r = _same(src)
    assert r is not False, "query engine diverged on the $referrers rounds program"
    assert r is not None, "query engine unexpectedly fell back on the $referrers rounds program"
