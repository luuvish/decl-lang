"""CST -> AST lowering over the compiled tree-sitter grammar — a port of
the reference implementation's parse.ts. The AST is the same dict shape
the TypeScript code uses."""

from __future__ import annotations

import json
import re
from typing import Any

from tree_sitter import Node, Parser

from ._tree_sitter import LANGUAGE

_parser: Parser | None = None


def _text(n: Node) -> str:
    return (n.text or b"").decode("utf-8")


def _field(n: Node, name: str) -> Node | None:
    return n.child_by_field_name(name)


# `true` / `false` / `null` are anonymous keyword tokens in the grammar:
# an operand position may hold one, so operands are the named children
# plus those literals (never the operator or punctuation tokens)
def _is_lit_keyword(c: Node | None) -> bool:
    return c is not None and not c.is_named and _text(c) in ("true", "false", "null")


def _operands(n: Node) -> list[Any]:
    return [c for c in n.children if c.is_named or _is_lit_keyword(c)]


def _req(n: Node, name: str) -> Node:
    c = n.child_by_field_name(name)
    if c is None:
        raise RuntimeError(f"lower: {n.type} missing field {name}")
    return c


def _kids(n: Node, type_: str) -> list[Any]:
    return [c for c in n.named_children if c.type == type_]


def _kid_req(n: Node, type_: str) -> Node:
    c = _kid(n, type_)
    if c is None:
        raise RuntimeError(f"lower: {n.type} missing {type_}")
    return c


def _kid(n: Node, type_: str) -> Node | None:
    for c in n.named_children:
        if c.type == type_:
            return c
    return None


# the same text parses to the same result: the session and the language
# server re-load the unchanged modules of a universe on every question,
# and the AST is never mutated after lowering (a small bounded cache)
_parse_cache: dict[str, Any] = {}


def parse_source(src: str) -> dict[str, Any]:
    hit = _parse_cache.get(src)
    if hit is not None:
        return hit
    r = _parse_source_uncached(src)
    if len(_parse_cache) >= 64:
        del _parse_cache[next(iter(_parse_cache))]
    _parse_cache[src] = r
    return r


def _parse_source_uncached(src: str) -> dict[str, Any]:
    global _parser
    if _parser is None:
        _parser = Parser(LANGUAGE)
    tree = _parser.parse(src.encode("utf-8"))
    errors: list[Any] = []
    _collect_errors(tree.root_node, errors)
    decls: list[Any] = []
    # annotations precede the declaration they attach to as its siblings (§5.10)
    pending: list[Any] = []
    for c in tree.root_node.named_children:
        if c.type == "ERROR":
            continue
        try:
            if c.type == "annotation":
                pending.append(lower_annotation(c))
                continue
            d = lower_decl(c)
        except Exception:
            if not errors:
                errors.append({"row": c.start_point[0], "col": c.start_point[1]})
            pending = []
            continue
        if d is not None:
            prev = c.prev_sibling
            exported = prev is not None and _text(prev) == "export"
            if exported or d["d"] == "re_export":
                d["exported"] = True
            if pending:
                d["annotations"] = pending
            pending = []
            # the declaration's source range (Phase 6 foundations): the `export`
            # keyword, when present, is the previous sibling and is included
            start = prev.start_point if exported and prev is not None else c.start_point
            d["loc"] = {"sl": start[0], "sc": start[1], "el": c.end_point[0], "ec": c.end_point[1]}
            decls.append(d)
    return {"decls": decls, "errors": errors}


def _collect_errors(n: Node, out: list[Any]) -> None:
    if n.type == "ERROR" or n.is_missing:
        out.append({"row": n.start_point[0], "col": n.start_point[1]})
    if n.has_error:
        for c in n.children:
            _collect_errors(c, out)


# ---------------- annotations ----------------
def lower_annotation(n: Node) -> dict[str, Any]:
    return {
        "name": _text(_req(n, "name")),
        "args": [lower_expr(c) for c in _operands(n)[1:]],
        "loc": _loc_of(n),
    }


# ---------------- declarations ----------------
def lower_decl(n: Node) -> dict[str, Any] | None:
    t = n.type
    if t == "type_declaration":
        params = _kid(n, "type_parameters")
        return {
            "d": "type",
            "name": _text(_req(n, "name")),
            "params": [
                {
                    "name": _text(p.named_children[0]),
                    "type": lower_type(p.named_children[1]) if len(p.named_children) > 1 else None,
                }
                for p in _kids(params, "type_parameter")
            ]
            if params
            else None,
            "type": lower_type(_req(n, "type")),
            "tail": _maybe_tail(n),
        }
    if t == "const_declaration":
        return {
            "d": "const",
            "name": _text(_req(n, "name")),
            "type": lower_type(_req(n, "type")) if _field(n, "type") else None,
            "expr": lower_expr(_req(n, "value")),
        }
    if t == "func_declaration":
        return {
            "d": "func",
            "name": _text(_req(n, "name")),
            "params": [
                {"name": _text(p.named_children[0]), "type": lower_type(p.named_children[1])}
                for p in _kids(n, "parameter")
            ],
            "ret": lower_type(_req(n, "return_type")) if _field(n, "return_type") else None,
            "body": lower_expr(_req(n, "body")),
        }
    if t == "output_declaration":
        return {
            "d": "output",
            "name": _text(_req(n, "name")),
            "type": lower_type(_req(n, "type")),
            "expr": lower_expr(_req(n, "value")),
        }
    if t == "input_declaration":
        return {
            "d": "input",
            "name": _text(_req(n, "name")),
            "type": lower_type(_req(n, "type")),
            "fallback": lower_expr(_req(n, "fallback")) if _field(n, "fallback") else None,
        }
    if t == "diagnostic_declaration":
        sev = _kid_req(n, "severity")
        tmpl = _kid_req(n, "template_string")
        return {
            "d": "diagnostic",
            "name": _text(_req(n, "name")),
            "params": [
                {"name": _text(p.named_children[0]), "type": lower_type(p.named_children[1])}
                for p in _kids(n, "parameter")
            ],
            "severity": _text(sev),
            "template": lower_template_parts(tmpl),
        }
    if t == "dimension_declaration":
        e = _kid(n, "dimension_expression")
        return {
            "d": "dimension",
            "name": _text(_req(n, "name")),
            "terms": lower_dim_expr(e) if e else None,
        }
    if t == "unit_declaration":
        dim = _field(n, "dimension")
        if dim is not None:
            return {"d": "unit", "name": _text(_req(n, "name")), "dim": _text(dim)}
        return {
            "d": "unit",
            "name": _text(_req(n, "name")),
            "factor": lower_expr(_req(n, "factor")),
            "base": _text(_req(n, "base")),
        }
    if t == "import_declaration":
        frm = json.loads(_text(_kid_req(n, "string")))
        ni = _kid(n, "named_imports")
        if ni is not None:
            return {
                "d": "import",
                "from": frm,
                "names": [_lower_import_item(i) for i in _kids(ni, "import_item")],
            }
        return {"d": "import", "from": frm, "ns": _text(_kid_req(n, "identifier"))}
    if t == "re_export_declaration":
        return {
            "d": "re_export",
            "from": json.loads(_text(_kid_req(n, "string"))),
            "names": [_lower_import_item(i) for i in _kids(n, "import_item")],
        }
    return None


def _lower_import_item(it: Node) -> dict[str, Any]:
    ids = it.named_children
    return {"name": _text(ids[0]), "as": _text(ids[1]) if len(ids) > 1 else None}


def _maybe_tail(n: Node) -> dict[str, Any] | None:
    t = _kid(n, "else_clause")
    return _lower_tail(t) if t is not None else None


def _lower_tail(n: Node) -> dict[str, Any]:
    sev = _kid(n, "severity")
    if sev is not None:
        return {
            "t": "inline",
            "severity": _text(sev),
            "template": lower_template_parts(_kid_req(n, "template_string")),
        }
    name = _text(_kid_req(n, "qualified_name"))
    args = [lower_expr(c) for c in n.named_children if c.type != "qualified_name"]
    return {"t": "ref", "name": name, "args": args}


_UNESC = {"n": "\n", "t": "\t", "r": "\r"}


def lower_template_parts(n: Node) -> list[Any]:
    parts: list[Any] = []
    for c in n.named_children:
        if c.type == "template_chars":
            parts.append(_text(c))
        elif c.type == "template_escape":
            parts.append(re.sub(r"\\(.)", lambda m: _UNESC.get(m.group(1), m.group(1)), _text(c)))
        elif c.type == "interpolation":
            parts.append(lower_expr(_operands(c)[0]))
    return parts


# ---------------- types ----------------
def _loc_of(n: Node) -> dict[str, Any]:
    return {
        "sl": n.start_point[0],
        "sc": n.start_point[1],
        "el": n.end_point[0],
        "ec": n.end_point[1],
    }


def lower_type(n: Node) -> dict[str, Any]:
    t = _lower_type0(n)
    t["loc"] = _loc_of(n)
    return t


def _lower_type0(n: Node) -> dict[str, Any]:
    t = n.type
    if t == "union_type":
        return {"k": "union", "arms": [lower_type(c) for c in n.named_children]}
    if t == "intersection_type":
        return {"k": "isect", "arms": [lower_type(c) for c in n.named_children]}
    if t == "nullable_type":
        return {
            "k": "union",
            "arms": [lower_type(n.named_children[0]), {"k": "prim", "name": "null"}],
        }
    if t == "array_type":
        elem = lower_type(n.named_children[0])
        rng = _kid(n, "array_size_range")
        if rng is None:
            sz = _field(n, "size")
            rng = sz if (sz is not None and sz.type == "range_expression") else None
        if rng is not None:
            ends = [_const_num(c) for c in rng.named_children]
            lo, hi = [v if isinstance(v, str) else _num(v) for v in ends]
            excl = any((not c.is_named) and _text(c) == "..<" for c in rng.children)
            if not isinstance(hi, str):
                return {"k": "array", "elem": elem, "lo": lo, "hi": hi - 1 if excl else hi}
            return {"k": "array", "elem": elem, "lo": lo, "hi": hi, "excl": excl}
        size = _field(n, "size")
        if size is not None:
            v0 = _const_num(size)
            v = v0 if isinstance(v0, str) else _num(v0)
            return {"k": "array", "elem": elem, "lo": v, "hi": v}
        return {"k": "array", "elem": elem}
    if t == "range_type":
        a, b = n.named_children[:2]
        return {"k": "range", "lo": _const_num(a), "hi": _const_num(b), "excl": "..<" in _text(n)}
    if t == "number_literal":
        return {"k": "lit", "v": _const_num(n)}
    if t == "string":
        return {"k": "lit", "v": json.loads(_text(n).replace("\n", "\\n"))}
    if t == "pattern":
        return {"k": "pattern", "re": _text(n)[1:-1]}
    if t == "paren_type":
        return lower_type(n.named_children[0])
    if t == "record_type":
        open_ = False
        members: list[Any] = []
        pending: list[Any] = []  # a member's annotations precede it as siblings (§5.10)
        for c in n.named_children:
            if c.type == "open_marker":
                open_ = True
                continue
            if c.type == "annotation":
                pending.append(lower_annotation(c))
                continue
            m = lower_member(c)
            if m is not None:
                if pending:
                    m["annotations"] = pending
                pending = []
                members.append(m)
        return {"k": "record", "members": members, "open": open_}
    if t == "map_type":
        return {"k": "map", "key": lower_type(_req(n, "key")), "val": lower_type(_req(n, "value"))}
    if t == "function_type":
        cs = [lower_type(c) for c in n.named_children]
        return {"k": "func", "params": cs[:-1], "ret": cs[-1]}
    if t == "named_type":
        name = _text(_kid_req(n, "qualified_name"))
        args_n = _kid(n, "type_arguments")
        args = [lower_type(c) for c in args_n.named_children] if args_n else []
        preds_n = _field(n, "predicates")
        preds = [lower_expr(c) for c in preds_n.named_children] if preds_n else None
        ext_n = _field(n, "extension")
        ext = lower_type(ext_n) if ext_n else None
        if (
            name in ("int", "uint", "float", "bool", "string")
            and not args
            and not preds
            and not ext
        ):
            return {"k": "prim", "name": name}
        return {"k": "named", "name": name, "args": args, "preds": preds, "ext": ext}
    txt = _text(n)
    if txt == "true":
        return {"k": "lit", "v": True}
    if txt == "false":
        return {"k": "lit", "v": False}
    if txt == "null":
        return {"k": "prim", "name": "null"}
    raise RuntimeError(f"lower_type: unhandled {t} '{txt[:30]}'")


def _num(v: Any) -> Any:
    return v if isinstance(v, float) else int(v)


def lower_dim_expr(n: Node) -> list[Any]:
    out: list[Any] = []
    sign = 1
    for c in n.children:
        if not c.is_named:
            tx = _text(c)
            if tx == "/":
                sign = -1
            elif tx == "*":
                sign = 1
            continue
        if c.type == "dimension_term":
            ident = next(x for x in c.named_children if x.type == "identifier")
            num = next((x for x in c.named_children if x.type == "int"), None)
            exp = int(_text(num)) if num is not None else 1
            if any((not x.is_named) and _text(x) == "-" for x in c.children):
                exp = -exp
            out.append({"name": _text(ident), "exp": exp * sign})
            sign = 1
    return out


def _const_num(n: Node) -> Any:
    if n.type == "number_literal":
        neg = _text(n).lstrip().startswith("-")
        v = _const_num(n.named_children[0])
        return -v if neg else v
    if n.type == "int":
        return _parse_int(_text(n))
    if n.type == "float":
        return float(_text(n).replace("_", ""))
    if n.type in ("qualified_name", "identifier"):
        return _text(n)
    raise RuntimeError(f"const_num: {n.type}")


def _parse_int(text: str) -> int:
    return int(text.replace("_", ""), 0)


# ---------------- members ----------------
def lower_member(n: Node) -> dict[str, Any] | None:
    m = _lower_member0(n)
    if m is not None:
        m["loc"] = _loc_of(n)
    return m


def _lower_member0(n: Node) -> dict[str, Any] | None:
    t = n.type
    # member kinds by syntax (D4, v0.3): `?` — input may supply it; `= e` —
    # the schema computes it. Both: defaulted; `= e` alone: derived
    if t == "value_member":
        name_n = _req(n, "name")
        name = json.loads(_text(name_n)) if name_n.type == "string" else _text(name_n)
        opt = _field(n, "optional") is not None
        dflt = lower_expr(_req(n, "default")) if _field(n, "default") else None
        if dflt is not None and not opt:
            return {"m": "derived", "name": name, "type": lower_type(_req(n, "type")), "expr": dflt}
        return {
            "m": "value",
            "name": name,
            "opt": opt,
            "type": lower_type(_req(n, "type")),
            "dflt": dflt,
        }
    if t == "derived_member":
        name_n = _req(n, "name")
        return {
            "m": "derived",
            "name": json.loads(_text(name_n)) if name_n.type == "string" else _text(name_n),
            "expr": lower_expr(_req(n, "value")),
        }
    # `x$ [: T] = e` — computed for the schema's own use, never part of the value (D34)
    if t == "hidden_member":
        return {
            "m": "derived",
            "name": _text(_req(n, "name")),
            "type": lower_type(_req(n, "type")) if _field(n, "type") else None,
            "expr": lower_expr(_req(n, "value")),
            "hidden": True,
        }
    if t == "context_declaration":
        return {
            "m": "context",
            "variable": _text(_req(n, "variable")),
            "type": lower_type(_req(n, "type")),
        }
    if t == "assert_member":
        return {
            "m": "assert",
            "name": _text(_req(n, "name")),
            "cond": lower_expr(_req(n, "condition")),
            "tail": _maybe_tail(n),
        }
    if t == "when_member":
        body: list[Any] = []
        for c in n.named_children[1:]:
            m = lower_member(c)
            if m is not None:
                body.append(m)
        return {"m": "when", "cond": lower_expr(_req(n, "condition")), "body": body}
    return None


# ---------------- expressions ----------------
_BIN_NODES = {
    "pipe_expression",
    "nullish_expression",
    "binary_expression_or",
    "binary_expression_and",
    "bit_or_expression",
    "bit_xor_expression",
    "bit_and_expression",
    "equality_expression",
    "relational_expression",
    "range_expression",
    "shift_expression",
    "additive_expression",
    "multiplicative_expression",
}
_KW_LITS = ("true", "false", "null")
_UNIT_RE = re.compile(r"^([0-9._]+(?:[eE][+-]?[0-9]+)?)([A-Za-z][A-Za-z0-9]*)$")


def lower_expr(n: Node) -> dict[str, Any]:
    e = _lower_expr0(n)
    e["loc"] = _loc_of(n)
    return e


def _lower_expr0(n: Node) -> dict[str, Any]:
    t = n.type
    if t == "int":
        return {"e": "lit", "v": _parse_int(_text(n))}
    if t == "float":
        return {"e": "lit", "v": float(_text(n).replace("_", ""))}
    if t == "unit_literal":
        m = _UNIT_RE.match(_text(n))
        assert m is not None  # the grammar's unit_literal is what the regex spells
        return {"e": "unitlit", "num": float(m.group(1).replace("_", "")), "unit": m.group(2)}
    if t == "string":
        return {"e": "lit", "v": json.loads(_text(n).replace("\n", "\\n"))}
    if t == "template_string":
        return {"e": "template", "parts": lower_template_parts(n)}
    if t in ("identifier", "hidden_name"):
        return {"e": "name", "name": _text(n)}
    if t == "context_variable":
        return {"e": "ctx", "name": _text(n)}
    if t == "referrers_expression":
        return {
            "e": "referrers",
            "type": _text(_req(n, "type")),
            "member": json.loads(_text(_req(n, "member"))),
        }
    if t == "paren_expression":
        return {"e": "paren", "x": lower_expr(_operands(n)[0])}
    if t == "unary_expression":
        return {"e": "un", "op": _text(n.children[0]), "x": lower_expr(_operands(n)[0])}
    if t == "if_expression":
        return {
            "e": "if",
            "c": lower_expr(_req(n, "condition")),
            "t": lower_expr(_req(n, "then")),
            "f": lower_expr(_req(n, "else")),
        }
    if t == "lambda":
        return {
            "e": "lambda",
            "params": [_text(p.named_children[0]) for p in _kids(n, "lambda_parameter")],
            "body": lower_expr(_req(n, "body")),
        }
    if t == "with_expression":
        base, patch = _operands(n)[:2]
        return {"e": "with", "base": lower_expr(base), "patch": lower_expr(patch)}
    if t in ("member_access", "safe_access"):
        x, name = _operands(n)[:2]
        return {
            "e": "member",
            "x": lower_expr(x),
            "name": json.loads(_text(name)) if name.type == "string" else _text(name),
            "safe": True if t == "safe_access" else None,
        }
    if t == "index_access":
        x, i = _operands(n)[:2]
        return {"e": "index", "x": lower_expr(x), "i": lower_expr(i)}
    if t == "call":
        cs = [c for c in n.children if c.is_named or _text(c) in _KW_LITS]
        return {"e": "call", "fn": lower_expr(cs[0]), "args": [lower_expr(c) for c in cs[1:]]}
    if t == "object":
        comp = _kid(n, "map_comprehension")
        if comp is not None:
            return lower_expr(comp)
        entries: list[Any] = []
        for en in _kids(n, "object_entry"):
            key = _field(en, "key")
            if key is not None:
                entries.append(
                    {
                        "key": json.loads(_text(key)) if key.type == "string" else _text(key),
                        "val": lower_expr(_req(en, "value")),
                    }
                )
            else:
                entries.append({"key": "...", "val": lower_expr(en.named_children[0])})
        return {"e": "obj", "entries": entries}
    if t == "map_comprehension":
        return {
            "e": "mapcomp",
            "key": lower_expr(_req(n, "key")),
            "val": lower_expr(_req(n, "value")),
            "clauses": [_lower_for(c) for c in _kids(n, "for_clause")],
        }
    if t == "array":
        comp = _kid(n, "array_comprehension")
        if comp is not None:
            return lower_expr(comp)
        items: list[Any] = []
        for en in _kids(n, "array_entry"):
            spread = _text(en).startswith("...")
            inner = next((c for c in en.named_children), None)
            if inner is None:
                inner = next(c for c in en.children if _text(c) in _KW_LITS)
            items.append({"spread": spread, "expr": lower_expr(inner)})
        return {"e": "arr", "items": items}
    if t == "array_comprehension":
        return {
            "e": "comp",
            "head": lower_expr(_req(n, "head")),
            "clauses": [_lower_for(c) for c in _kids(n, "for_clause")],
        }
    if t == "matches_expression":
        l, r = n.named_children[:2]
        return {"e": "bin", "op": "matches", "l": lower_expr(l), "r": lower_expr(r)}
    if t == "pattern":
        return {"e": "pattern", "re": _text(n)[1:-1]}
    if t == "match_expression":
        arms: list[Any] = []
        for a in _kids(n, "match_arm"):
            body = a.child_by_field_name("body")
            others = [c for c in a.named_children if c.id != body.id]
            arms.append(
                {
                    "v": _text(others[0]),
                    "type": lower_type(others[1]) if len(others) > 1 else None,
                    "body": lower_expr(body),
                }
            )
        return {"e": "match", "subject": lower_expr(_req(n, "subject")), "arms": arms}
    if t in _BIN_NODES:
        l, r = _operands(n)[:2]
        # the operator is the one anonymous child that is not an operand
        op = next(
            _text(c)
            for c in n.children
            if not c.is_named and not _is_lit_keyword(c) and _text(c).strip() != ""
        )
        return {"e": "bin", "op": op, "l": lower_expr(l), "r": lower_expr(r)}
    txt = _text(n)
    if txt == "true":
        return {"e": "lit", "v": True}
    if txt == "false":
        return {"e": "lit", "v": False}
    if txt == "null":
        return {"e": "lit", "v": None}
    raise RuntimeError(f"lower_expr: unhandled {t} '{txt[:40]}'")


def _lower_for(n: Node) -> dict[str, Any]:
    return {
        "v": _text(_req(n, "variable")),
        "iter": lower_expr(_req(n, "iterable")),
        "filters": [lower_expr(c) for c in n.children_by_field_name("filter")],
    }


def parse_expr_text(text: str) -> dict[str, Any] | None:
    """parse one expression's text: the text is wrapped in a constant
    declaration; None when it does not parse"""
    r = parse_source(f"const __e = {text}\n")
    decls, errors = r["decls"], r["errors"]
    if errors or len(decls) != 1 or decls[0]["d"] != "const":
        return None
    return decls[0]["expr"]
