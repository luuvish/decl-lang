# 11. Grammar

The formal grammar of Decl. **When this chapter and any chapter's prose
diverge, this chapter wins**, and the divergence is corrected on
discovery (§1.4). The tree-sitter grammar of Phase 1 is written against
this chapter.

## 11.1 Notation

```
name = …        definition            "x"      literal terminal
a | b           alternation           a?       optional
a*              zero or more          a+       one or more
( … )           grouping              [c-c]    character class (lexical only)
```

- `sep` is the element separator of §2.9 — a comma or a
  separator-position newline; trailing separators are allowed
  everywhere `sep` appears before a closing bracket. Inside `( … )`
  newlines are whitespace and only `","` separates.
- UPPERCASE names (`NEWLINE`, `STRING`, …) are tokens from the lexical
  grammar (§11.2); lowercase names are syntactic.
- Static rules stated in other chapters (the `??` mixing ban §4.3,
  discriminability §3.12, no-shadowing §5.1, …) are **not** encoded in
  the grammar; the grammar is deliberately looser where a later stage
  reports a better diagnostic.

## 11.2 Lexical summary

Normative token definitions are [02. Lexical Structure](02_lexical.md);
this summary names the tokens the syntactic grammar consumes.

```
IDENT     = [_A-Za-z][_A-Za-z0-9]*                 — §2.3
INT       = decimal | 0x… | 0o… | 0b…              — §2.6
FLOAT     = decimal float forms                     — §2.6
UNIT-LIT  = decimal INT or FLOAT immediately
            followed by IDENT (one token)           — §2.7
STRING    = "…" with JSON escapes                   — §2.8
TEMPLATE  = `…${ }…` (text parts and holes)         — §2.8
PATTERN   = /…/ — non-empty body of literal text
            and "${" type "}" holes                 — §2.8, §3.6
CTXVAR    = $this $parent $root $key $path          — §2.5
REFERRERS = $referrers                              — §2.5
NEWLINE   = separator-position line break           — §2.9
DOC       = /// documentation comment line          — §2.2
```

Keywords and predeclared names are §2.4; operator tokens are §2.10.

## 11.3 Modules and declarations

```
module          = (NEWLINE | declaration)*

declaration     = DOC* annotation* (import-decl | re-export-decl | decl)
annotation      = "@" IDENT ( "(" args ")" )?

import-decl     = "import" ( "{" import-items "}" | "*" "as" IDENT )
                  "from" STRING
re-export-decl  = "export" "{" import-items "}" "from" STRING
import-items    = import-item (sep import-item)* sep?
import-item     = IDENT ("as" IDENT)?

decl            = "export"? ( type-decl | const-decl | func-decl
                | output-decl | input-decl | diagnostic-decl
                | dimension-decl | unit-decl )

type-decl       = "type" IDENT type-params? "=" type else-clause?
type-params     = "<" type-param ("," type-param)* ">"
type-param      = IDENT (":" type)?          — with ":" a value parameter

const-decl      = "const" IDENT (":" type)? "=" expr
func-decl       = "func" IDENT "(" params? ")" (":" type)? "=" expr
params          = param ("," param)*
param           = IDENT ":" type

output-decl     = "output" IDENT ":" type "=" expr
input-decl      = "input"  IDENT ":" type ("=" expr)?

diagnostic-decl = "diagnostic" IDENT "(" params? ")"
                  "{" "severity" "=" severity sep "message" "=" TEMPLATE sep? "}"
severity        = "error" | "warn" | "info"

dimension-decl  = "dimension" IDENT ("=" dim-expr)?
dim-expr        = dim-term (("*" | "/") dim-term)*
dim-term        = IDENT ("^" "-"? INT)?

unit-decl       = "unit" IDENT ( ":" IDENT            — base unit of a dimension
                               | "=" expr IDENT )     — factor and base unit

else-clause     = "else" ( severity TEMPLATE
                         | qualified ( "(" args ")" )? )
qualified       = IDENT ("." IDENT)*
```

## 11.4 Types

Type-surface precedence, loosest to tightest: `|` union, `&`
intersection, postfix suffixes, primaries.

```
type            = isect-type ("|" isect-type)*
isect-type      = suffix-type ("&" suffix-type)*
suffix-type     = primary-type suffix*
suffix          = "?"                                — T? ≡ T | null
                | "[" size? "]"                      — array
size            = const-expr
                | const-expr (".." | "..<") const-expr

primary-type    = literal-type
                | range-type
                | PATTERN
                | record-type
                | map-type
                | func-type
                | named-type
                | "(" type ")"

literal-type    = STRING | "-"? INT | "-"? FLOAT | "true" | "false" | "null"
range-type      = const-expr (".." | "..<") const-expr
named-type      = qualified type-args? predicates? extension?
type-args       = "<" type-arg ("," type-arg)* ">"
type-arg        = type | const-expr                  — value arguments
predicates      = "(" expr ("," expr)* ")"           — T(p, q): (T) => bool exprs
extension       = record-type                        — Parent { … } inheritance

record-type     = "{" (member (sep member)* sep?)? ("..." sep?)? "}"
map-type        = "{" "[" type "]" ":" type "}"
func-type       = "(" (type ("," type)*)? ")" "=>" type
```

## 11.5 Schema members

```
member          = DOC* annotation* ( value-member | const-member
                                   | assert-member | when-member
                                   | ctx-member )
ctx-member      = CTXVAR ":" type                    — §7.3 context declaration (D30)
value-member    = member-name "?"? ":" type ("=" expr)?
const-member    = "const" member-name (":" type)? "=" expr
assert-member   = "assert" IDENT ":" expr else-clause?
when-member     = "when" expr "{" (guarded (sep guarded)* sep?)? "}"
guarded         = assert-member | when-member
member-name     = IDENT | STRING                     — §3.11 naming rules
```

## 11.6 Expressions

Precedence encoded structurally, per the table of §4.3 (loosest first).

```
expr            = lambda | if-expr | match-expr | pipe-expr

lambda          = "(" lambda-params? ")" "=>" expr
lambda-params   = lambda-param ("," lambda-param)*
lambda-param    = IDENT (":" type)?

if-expr         = "if" expr "then" expr "else" expr
match-expr      = "match" expr "{" match-arm (sep match-arm)* sep? "}"
match-arm       = "(" IDENT (":" type)? ")" "=>" expr

pipe-expr       = nullish ("|>" nullish)*
nullish         = logical-or ("??" logical-or)*
logical-or      = logical-and ("||" logical-and)*
logical-and     = bit-or ("&&" bit-or)*
bit-or          = bit-xor ("|" bit-xor)*
bit-xor         = bit-and ("^" bit-and)*
bit-and         = equality ("&" equality)*
equality        = relational (("==" | "!=") relational)?
relational      = range-expr ( ("<"|"<="|">"|">="|"in") range-expr
                             | "matches" PATTERN )?
range-expr      = shift ((".." | "..<") shift)?
shift           = additive (("<<" | ">>") additive)*
additive        = multiplicative (("+" | "-") multiplicative)*
multiplicative  = unary (("*" | "/" | "%") unary)*
unary           = ("!" | "-" | "~") unary | with-expr
with-expr       = postfix ("with" object-literal)*
postfix         = primary ( "." member-key
                          | "?." member-key
                          | "[" expr "]"
                          | "(" args? ")" )*
member-key      = IDENT | STRING                     — §4.3 access forms
args            = expr ("," expr)*

primary         = INT | FLOAT | UNIT-LIT | STRING | TEMPLATE
                | "true" | "false" | "null"
                | IDENT                              — dotted paths are postfix access
                | CTXVAR
                | referrers-expr
                | object-literal
                | array-literal
                | "(" expr ")"

referrers-expr  = REFERRERS "(" type "," STRING ")"  — §7.6 (type argument)

object-literal  = "{" (obj-entry (sep obj-entry)* sep?)? "}"
                | "{" expr ":" expr for-clauses "}"  — map comprehension
obj-entry       = member-key ":" expr
                | "..." expr

array-literal   = "[" (arr-entry (sep arr-entry)* sep?)? "]"
                | "[" expr for-clauses "]"           — array comprehension
arr-entry       = expr | "..." expr                  — element or spread
for-clauses     = ("for" IDENT "in" expr ("if" expr)*)+
```

**Constant expressions** (`const-expr`, used by §11.3–11.4) are the
`expr` grammar restricted by the static rule of §4.13 — no `input` or
`output` references, no context variables; the grammar itself is
shared.

## 11.7 Data documents

The grammar of an interchange document ([10. Interchange](10_interchange.md))
is **exactly JSON** (RFC 8259) — no extra productions. This is a
theorem of the design, not a coincidence: the two Decl value kinds JSON
cannot carry natively cross the boundary as ordinary JSON shapes,
type-directed — quantities as `{ "value": …, "unit": "…" }` objects and
references as canonical path strings (§10.1). A data document therefore
never needs Decl syntax, and a JSON parser is a complete front end for
`input` binding. (The previous iteration's data grammar could not carry
quantities at all — the hole this section exists to keep closed,
00_vision §3.)

The canonical path grammar for reference strings (§7.2, §10.2):

```
ref-path        = (root | "$") path-seg*
root            = IDENT
path-seg        = "." IDENT | "[" INT "]" | "[" STRING "]"
```

## 11.8 Disambiguation notes

The known points where the grammar relies on context or lookahead —
listed so implementations agree:

1. **Lambda vs parenthesized expression**: `(x)` alone is a
   parenthesized name; `(x) =>` is a lambda. Resolved by lookahead to
   `=>` after the closing parenthesis (likewise `()` and typed
   parameter lists, which cannot be expressions).
2. **`<` in types vs comparison**: the surfaces are separated by
   position (D2) — after `:` and inside type contexts, `<` opens type
   arguments; in expression contexts it compares. No token-level
   resolution is needed.
3. **`/` pattern vs division**: `/` begins a PATTERN token exactly
   where the grammar expects a type primary or the right operand of
   `matches`; everywhere else it divides (§2.10).
4. **`{` object vs map comprehension vs block**: there are no block
   expressions; in expression positions `{` opens an object literal or
   map comprehension (distinguished by the `for` following the first
   entry); in type positions it opens a record or map type
   (distinguished by a leading `[`).
5. **Extension juxtaposition**: in a type position,
   `qualified { … }` is inheritance (§3.14); a record type not
   preceded by a name is a plain record. No expression form puts `{`
   after a name — there are no trailing blocks or struct-literal
   calls (§4.9) — so the value surface has no counterpart to confuse.
6. **`?` three ways**: `x?:` optional member (declaration), `T?`
   nullable suffix (type), `a?.b` safe navigation (expression) —
   distinct positions, one token (§4.10).
7. **Unit declaration juxtaposition**: `unit ms = 1e-3 s` — the
   trailing IDENT after the factor expression is the base-unit name;
   this is the only place an expression is followed by a bare
   identifier, and the production closes it immediately.
8. **Newline separators**: NEWLINE is a token only in separator
   positions per §2.9's rule (previous token can end an element, next
   can begin one); the grammar writes `sep` where that applies.
9. **Types outside type positions** appear in exactly two token-level
   places, each closed by its own production: the first argument of
   `$referrers`, and the `${ type }` holes inside a PATTERN body.
10. **Dotted names are postfix access**: expression `primary` admits a
    bare IDENT only, so `std.array.count` and `source.width` each have
    one derivation — a chain of `.` accesses; whether a prefix names a
    namespace, a declaration, or a value member is decided by name
    resolution, not the grammar.

## 11.9 Syntax error conditions

The parse-stage error conditions this grammar gives rise to (codes in
[12. Errors](12_errors.md) §E2xxx): an unexpected token where no
production continues; an unclosed bracket, brace, or parenthesis at end
of input; a declaration whose head keyword is not followed by its
production's shape; a separator where no element may end or begin
(§2.9). Recovery quality — how much of the surrounding file still
parses — is an implementation concern bounded by §12.3's conformance
rule.

## Open questions

None.

---

## Previous / Next

- Previous: [10. Data Interchange](10_interchange.md)
- Next: [12. Errors and Diagnostic Codes](12_errors.md)
- Index: [Documentation home](../README.md)
