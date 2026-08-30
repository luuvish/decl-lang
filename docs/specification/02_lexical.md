# 02. Lexical Structure

This chapter defines how source text becomes tokens: encoding, comments,
identifiers, keywords, literals, operators, and the separator rules that
make both JSON commas and Decl newlines work (D1).

## 2.1 Source text

- Source files are UTF-8. A file must not begin with a byte-order mark.
- Line terminators are LF or CRLF; both count as one *newline* for the
  separator rules of §2.9. A conforming serializer and formatter emit LF.
- Whitespace is space, tab, and the line terminators. Outside of string,
  template, and pattern literals, whitespace has no meaning except to
  separate tokens — and the newline's separator role (§2.9).
- Style (4-space indentation, no tabs, 100-column lines) is the
  formatter's canonical form, not a lexical requirement: a file that
  violates style still tokenizes.

## 2.2 Comments

| Form | Meaning |
|---|---|
| `// …` | line comment, to end of line |
| `/* … */` | block comment; **nests** |
| `/// …` | documentation comment |

- A `///` comment documents the declaration that follows it; consecutive
  `///` lines form one documentation block. A `///` with no following
  declaration in the same scope is an error.
- An unterminated block comment is a lexical error.

```decl
/// The port a service listens on.
type Port = 1..65535
```

## 2.3 Identifiers

```
identifier = [_A-Za-z][_A-Za-z0-9]*
```

- Identifiers are ASCII in v0.1. This is a decision, not an oversight:
  Unicode identifiers add normalization and confusability rules that P2's
  bit-level determinism would have to specify; they can be admitted by a
  revision that carries those rules (P7).
- Object literal **keys** are not identifiers but strings: any JSON string
  key is valid when quoted (`{ "let!": 1 }`), and quotes may be dropped
  exactly when the key satisfies the identifier rule and is not a
  keyword (§2.4).

## 2.4 Keywords and predeclared names

Reserved keywords — usable nowhere as identifiers:

```
type  const  func  output  input  export  import  from  as
dimension  unit  diagnostic  assert  when
if  then  else  match  for  in  matches  with
true  false  null
```

Contextual keywords — reserved only in the stated position, ordinary
identifiers elsewhere:

- `error`, `warn`, `info` — severity, after `else` (D20) and as the
  `severity` value in a `diagnostic` block.
- `severity`, `message` — field names inside a `diagnostic` block.

Predeclared names — not keywords, but bound in the outermost scope and
protected by the no-shadowing rule (D27), so they cannot be redeclared:

```
bool  int  uint  float  string  quantity  ref  std
```

*Counterexample:* `const type = 3` is a lexical error (keyword);
`const int = 3` is a name-resolution error (shadowing a predeclared
name); `{ "type": "router" }` is a valid object — quoted keys are always
strings.

## 2.5 Context variables

A `$` immediately followed by an identifier is a context-variable token.
The valid context variables are exactly:

```
$this  $parent  $root  $key  $path  $referrers
```

Any other `$`-token (`$value`, `$std`, …) is a lexical error. Semantics
are defined in [07. Relationships](07_relationships.md).

## 2.6 Number literals

Decimal integers and floats follow JSON exactly, with two extensions
(non-decimal bases; digit separators):

```
int-literal   = decimal-int | "0x" hex-digits | "0o" octal-digits | "0b" binary-digits
decimal-int   = "0" | [1-9] digits?
float-literal = decimal-int "." digits exponent? | decimal-int exponent
exponent      = ("e" | "E") ("+" | "-")? digits
```

- Leading zeros are forbidden (`012` is an error), as in JSON.
- A float must have digits on **both** sides of the dot: `0.5`, never
  `.5` or `5.`.
- `_` may separate digits in any literal (`1_000_000`, `0xFFFF_0000`);
  it must stand between two digits — never leading, trailing, adjacent
  to the base prefix, the dot, or the exponent mark.
- There are no `NaN` or `Infinity` literals, and no lexical form produces
  them (D24).
- `-` is not part of a number literal; it is the unary minus operator.
  JSON documents containing `-5` parse as unary minus applied to `5`,
  which evaluates to the same value ([10. Interchange](10_interchange.md)).

A literal's type is `int` for integer forms and `float` for float forms;
the two never convert implicitly (D6, D7).

## 2.7 Unit literals

A decimal number literal **immediately** followed by an identifier — no
whitespace — is a unit literal, a single token denoting a quantity (D15):

```decl
10ms    2.5s    100MHz
```

- Only decimal forms take units: `0x10ms` is a lexical error.
- The identifier must resolve to a declared `unit`; that check is
  semantic, not lexical ([03. Types](03_types.md)).
- With interposed whitespace, the same characters are two tokens
  (`10 ms` — a number and an identifier), which is a parse error in
  value positions.

## 2.8 String, template, and pattern literals

**Strings** use double quotes with exactly JSON's escape set:

```
"\""  "\\"  "\/"  "\b"  "\f"  "\n"  "\r"  "\t"  "\uXXXX"
```

There are no single-quoted strings (P5). An unterminated string or an
unknown escape is a lexical error.

**Templates** use backticks and interpolate expressions with `${…}`:

```decl
const endpoint = `${name}:${port}`
```

Escapes are the string escapes plus `` \` `` and `\$`. A template is an
expression ([04. Expressions](04_expressions.md)); its literal text parts
are tokenized here. Templates do not nest inside their own literal parts;
an interpolation may contain any expression, including another template.

**Patterns** are `/…/` literals used as whole-match string types (D8):

```decl
type ServiceName = /[a-z][a-z0-9-]*/
```

- `\/` escapes a slash; the pattern body is otherwise passed to the
  pattern grammar of [03. Types](03_types.md) §pattern.
- There are no flags after the closing `/` (matching is exact,
  case-sensitive, whole-string).
- An empty pattern `//` is not a pattern literal — it is a line comment.
  A pattern literal must have a non-empty body.
- `${Type}` inside a pattern interpolates another pattern or literal
  type (D8); the syntax is reserved at the lexical level and resolved in
  the type grammar.

## 2.9 Separators: comma and newline

Element separation follows D1 — both JSON commas and Decl newlines:

- Inside `{ … }` (object literals, record types, schema bodies,
  `diagnostic`/`when` blocks) and `[ … ]` (arrays), elements are
  separated by `,` or by a newline. Trailing commas are allowed. Blank
  lines and comment-only lines separate nothing extra.
- At module level, declarations are separated by newlines.
- Inside `( … )` — call arguments, parameter lists, predicate lists,
  parenthesized expressions — newlines are ordinary whitespace, and
  arguments are separated by commas only.
- **The newline-separator rule**: a newline acts as a separator exactly
  when the token before it can end an element and the token after it can
  begin one. Otherwise it is whitespace. Consequently an expression may
  wrap after an infix operator or an opening bracket:

```decl
// one member: the line ends with an operator, so the newline is whitespace
const total = base_cost +
    extra_cost

// two members: both lines are complete
const a = 1
const b = 2
```

- The formatter's canonical form (D1): newline-separated when a construct
  spans lines, comma-separated on one line. Both always parse.
- There are no semicolons; `;` is not a token of the language.

## 2.10 Operators and punctuation

All fixed tokens, longest-match first:

```
...   ..<   ..    ?.    ??    =>    |>    <<    >>    &&    ||
==    !=    <=    >=
{  }  [  ]  (  )  <  >  ,  :  =  ?  .  |  &  ^  ~  !  +  -  *  /  %  @
```

- `<` and `>` open and close type arguments in the type surface
  (`ref<Service>`, `quantity<Time>`) and compare in the expression
  surface; the surfaces are separated by position (D2), so the lexer
  emits the same tokens and the grammar disambiguates.
- `/` begins a pattern literal only where a type or expression may begin
  and a division cannot continue (the grammar states the exact
  positions); elsewhere it divides.
- `->` and `;` are not tokens (D16; §2.9). `$` appears only in context
  variables (§2.5).

## 2.11 Lexical errors

Each of the following is an error condition (codes in
[12. Errors](12_errors.md)): invalid UTF-8; unterminated string,
template, pattern, or block comment; unknown escape; a number with
leading zeros, a misplaced digit separator, or digits missing around a
float dot; a unit literal on a non-decimal base; an unknown
context-variable name; an unknown character; a keyword used as an
identifier; `///` with nothing to document.

## Open questions

None. (Unicode identifiers are decided-deferred, §2.3 — not open.)

---

## Previous / Next

- Previous: [01. Introduction](01_introduction.md)
- Next: [03. Type System](03_types.md)
- Index: [Documentation home](../README.md)
