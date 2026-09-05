# The REPL — `decl repl`

`decl repl` is an interactive session over an evaluation universe: the
modules loaded from an entry file, the documents bound to their roots,
the outputs the session adds, and the edits it makes to documents. It
runs the same checker, inference, and engine as the `decl` command line —
nothing it prints comes from a second interpreter — and the three
implementations (TypeScript, Rust, Python) answer one scripted session
with identical bytes (`tests/parity/differential.py`). This document is
informative: the language it evaluates is the
[specification](../specification/01_introduction.md), and the commands
that share a name with a `decl` subcommand mean what
[01. Command line](01_cli.md) says.

```
decl repl [file.decl] [--input name=doc.json]... [--script session.txt | --script -] [--compact]
```

## 1. The session

A session is a **universe** plus an **operation log**:

- the universe: the modules `:load` opened (entry, imports, package);
- the log: every operation since, in order — bindings, document edits,
  session declarations and their removal, reloads.

The session's state is the universe with the log applied, and it is
recomputed deterministically: applying the same log to the same universe
gives the same documents, the same diagnostics, the same bytes. That is
what makes `:undo` and `:redo` exact (§5) and what lets `--script`
reproduce a session from a text file (§9).

Session declarations form a **scratch module** evaluated in the entry
module's own scope — as if appended to it — so they and the bare
expressions see everything the entry sees: its roots, types, constants,
functions, and imports by their local names, and `std` as always. The
session never edits the entry's file; the scratch is written out on
request (`:write`, with the entry's exports it uses imported explicitly).
There is no mutable state anywhere: a session declaration can be
redeclared, which replaces it; a document can be edited, which produces
the next document.

`decl repl file.decl` starts with `:load file.decl`, and each `--input
name=doc.json` with `:bind name=doc.json` — the command line's binding,
so a session begins where `decl evaluate` would. `decl repl` alone
starts with an empty universe, where expressions over literals and `std`
already evaluate.

## 2. Input

Each input is an expression, a session declaration, or a command. The
prompt is `> `; a line whose last token leaves an expression open — an
unclosed bracket, a trailing binary operator, `=`, `then`, `else` —
continues on the next line behind the continuation prompt `. `, the §2.9
rule the formatter also applies (a command continues only while a
bracket is open: an inline document). A blank line is ignored; `//` and
`/* */` comments are allowed as in a module. The line editor offers
history (previous inputs, searchable, kept across sessions in
`~/.decl_history`), **completion** on Tab (§7), and the usual editing
keys; Ctrl-D at an empty prompt is `:quit`.

### 2.1 An expression — partial evaluation

A bare expression is evaluated as an anonymous derived member of the
scratch module and printed. This is **partial evaluation** in the
specification's sense (§9.8, D25): the engine evaluates the minimal
dependency set of the expression, prints its value, and prints the
diagnostics *arising from that set only*. The result is marked `partial`
and is never a verdict on the document — the diagnostics partial
evaluation cannot produce (assertions of instances it never visited,
errors in members it never demanded, inputs it never needed, references it
never resolved) are simply not there. `:validate` is the whole-document
question.

```
> site.label
"web:443"
(partial)
> extra
error [E5006]: input extra is not bound
(invalid)
(partial)
```

The diagnostics printed are those the expression's own demand produced,
plus the diagnostic of a root the expression names whose binding had
already failed; `(partial)` closes every answer, `(invalid)` stands where
a value would be.

Values print as canonical JSON, pretty-printed (`:set compact` switches to
the wire form). A reference prints as its canonical path; a record, array,
or map prints whole. A navigation that names a place prints the place's
value — to see the path instead, `:path`.

### 2.2 A session output — `x = expr`

`x = expr` adds `output x = expr` to the scratch module: a root, fully
evaluated when demanded, addressable by name in later input and by
`:evaluate x` / `:validate x`. It is the by-the-book way to give a name
to a value that reads the loaded roots (a module `const` may not, §5.2).
An annotation is allowed (`x: T = expr`), and the type is otherwise
inferred — `:fmt` and `:write` spell it out (`output x: (1..65535)[] =
…`), so what leaves the session is a declaration a module accepts.

### 2.3 Other session declarations

Any other module-level declaration form is accepted as written in a
module and added to the scratch module: `const x = expr` (a constant,
§5.2 — a constant expression, usable in type positions, never a reference
target), `type`, `func`, `diagnostic`, `dimension`, `unit`, and `import`
(another module of the workspace, or a package the lock resolves). The
REPL invents no declaration syntax: whatever a session declares can be
pasted into a module unchanged.

### 2.4 Commands

Commands start with `:`. The ones that change the session are operations
of the log (§5); the ones that only ask questions are not logged. An
unknown command, a command with the wrong arguments, or a root or path
the session does not have is reported on one line and changes nothing.

**The universe** — what is loaded

| Command | Meaning |
|---|---|
| `:load file.decl` | open the universe from an entry module: its imports, and its package's `decl.toml` and lock when present. Starts a new session: a new log, no bindings, no declarations |
| `:reload` | re-read every module of the current universe from disk, keeping the log (an operation, so `:undo` restores the previous texts) |
| `:roots` | the roots of the universe: outputs with their export status and module, inputs with their binding state (file, inline, expression, fallback, unbound), session outputs, and which documents carry edits |

**Documents** — binding and editing (§3)

| Command | Meaning |
|---|---|
| `:bind name=doc.json` | bind the document in the file to input `name` |
| `:bind name { … }` | bind an inline JSON document (multi-line by the §2.9 rule) |
| `:bind name = expr` | bind the value of an expression as `name`'s document: `:bind oad = u_oic` puts an output's document into an input — the round trip of §10.5, inside the session |
| `:unbind name` | drop the binding; the input falls back to its fallback, or is unbound |
| `:create path = expr` | add a member, map entry, or array element at a canonical path of a document; an error if the path already holds a value |
| `:update path = expr` | replace the value at a canonical path of a document; an error if there is none |
| `:remove path` | remove the member, entry, or element at a canonical path of a document; an error if there is none |
| `:diff name` | the document of root `name` as the session holds it against the file it was bound from, or the source literal it came from (`(no changes)` when they agree) |
| `:save name=file` | write the document of root `name`, as the session holds it, to a file |

**Session declarations** (§4)

| Command | Meaning |
|---|---|
| `x = expr`, `const x = expr`, `type …`, `func …`, … | a session output, or any other module-level declaration (§2.2, §2.3) |
| `:drop name` | remove a session declaration (an output or a constant) |
| `:write file.decl` | write the scratch module — the session's declarations, canonically formatted, with the imports the session added — to a file: a session becomes a module (§4) |
| `:session` | the session's declarations and the documents it holds, with their origins |
| `:reset` | keep the universe, drop every binding, edit, and declaration (one operation; `:undo` brings them back) |

**Evaluation and validation** — the command-line verbs, root for root

| Command | Meaning |
|---|---|
| `:check` | static diagnostics of every module, as `decl check` prints them |
| `:evaluate [root…]` | full evaluation of the named roots — every member, every assertion — printing each root's document; with no root, the entry module's exported outputs, as one object keyed by name. Exactly `decl evaluate --output root…` |
| `:validate [root…]` | full validation of the named roots: every diagnostic of every severity, then a verdict per root (`ok`, or the count of errors); with no root, every root of the universe, bound inputs included — the only way to a whole-document verdict |
| `:fmt` | the session's scratch module, canonically formatted |

**Inspection** — questions about one expression or place

| Command | Meaning |
|---|---|
| `:type expr` | the static type of the expression as inference sees it, with the absence flag when the expression is maybe-absent |
| `:doc name` | the declaration `name` resolves to, with its `///` / `@doc` documentation; for a member, `type.member` |
| `:path expr` | the canonical path (§7.2) of the place a navigation names, absolute |
| `:trace path` | for a valid place, its derivation: the member expression and the places it read, recursively, down to supplied values and literals; for an invalid place, the chain of invalidation down to the root-cause diagnostic (§6.6) |
| `:complete text` | the candidates Tab would offer at the end of `text` (§7) — the scripted form of completion |

**History** (§5)

| Command | Meaning |
|---|---|
| `:undo [n]` | step the log back one operation, or `n` |
| `:redo [n]` | step forward again |
| `:history [file]` | the log, with the cursor; with a file, write the operations up to the cursor as a session file `--script` replays (§9) |

**The session itself**

| Command | Meaning |
|---|---|
| `:time` | wall time of the last evaluation, and of its parts (load, check, bind, evaluate — and what the incremental step recomputed, §6) |
| `:set pretty` / `:set compact` | value printing |
| `:help [command]` | these tables, or one entry |
| `:quit` | end the session (also end of input) |

## 3. Editing documents

The language never mutates a value; what a session edits is a
**document** — the data a root is built from — and every edit yields the
next document, which the same pipeline evaluates again. Three commands
add, change, and remove parts of it: `:create`, `:update`, `:remove`,
each at a canonical path (§7.2) under a root, each an operation of the
log.

- The value of `:create` and `:update` is an expression, evaluated in the
  scratch module and serialized as a document fragment (§10.3): a literal
  (`{ type: "port.ext", mode: "si" }`, `32`), or an expression over the
  roots, whose value is **copied** into the document — a navigation in a
  `ref<T>` position of the target type is stored as its canonical path.
- The three distinguish themselves on purpose: `:create` refuses a path
  that already holds a value, `:update` and `:remove` refuse one that
  does not. A mistyped path is an error, never a silent new key.
- **An input root** edits its bound document (or its fallback's document,
  when nothing is bound: the fallback's value, projected, becomes the
  document). **An output root** has no document of its own — its value
  comes from a source literal — so the first edit **detaches** it: the
  session serializes the output's value *without its derived members*
  (the settable projection: required, optional, and defaulted members
  only — the tool option D29 provides), turns the declaration into `input
  name: T` in its copy of the module, and binds that document to it; the
  output's literal no longer takes part, and everything that read the
  output reads the document. Derived and hidden members are recomputed
  from the edited document, so an edit never trips the restatement rule
  (D4). `:session` and `:roots` say which roots are detached; `:unbind`,
  `:reload`, and `:undo` reattach. A session output (`x = expr`) has no
  document either and cannot be edited: edit the roots it reads.
- After an edit, evaluation runs again incrementally (§6). `:validate
  name` gives the document's new verdict; `:diff name` shows what
  changed; `:save name=file` writes it out; the original file is never
  touched by an edit.

```
> :bind deployed=doc.json
> :update deployed.port = 9100
> :create deployed.replicas = 9
> :validate deployed
error [E6001] Cfg.sane at deployed: too many replicas
deployed: 1 error
> :diff deployed
  {
-   "port": 9000,
-   "name": "doc"
+   "port": 9100,
+   "name": "doc",
+   "replicas": 9
  }
> :undo
> :validate deployed
deployed: ok
```

## 4. Session declarations

`x = expr` and the other declaration forms add to the scratch module
(§2.2, §2.3); `:drop x` removes; both are operations of the log. A declaration that
reads a document sees the document as the session currently holds it,
and is recomputed when the document changes (§6).

The scratch module is a module: `:fmt` prints it in canonical form and
`:write file.decl` writes it out, its `import` of the entry's exports
made explicit, so what was tried in a session is committed as source
without retyping. A session output that reads a bound input is written
as it was declared; the binding itself is not part of the module — it is
the command line's `--input` again.

## 5. History: `:undo`, `:redo`

The log is linear and the session keeps a cursor into it. `:undo`
moves the cursor back one operation (or `n`), `:redo` forward; a new
operation after an `:undo` discards the operations beyond the cursor.
Because the state is the universe with the log applied, undoing is
exact: not "reverse the effect", but "the state without that operation",
recomputed — the same documents and diagnostics as if the operation had
never been typed. `:reload` is an operation too: the session keeps the
module texts it replaced, so undoing a reload restores them. `:load`
begins a new log; there is no undo across it. `:history` prints the log
with the cursor; questions (`:evaluate`, `:validate`, `:type`, …) are
not in it.

## 6. Incremental re-evaluation

The session re-evaluates after every operation, but not from scratch. The
engine records, for every evaluated slot, what it read: the slots, the
document places, and — for a `$referrers` result — the set of instances
of the queried type (§7.6). An operation invalidates exactly the slots
that read what it changed, transitively, and the next question recomputes
those and no others; a `$referrers` result is recomputed whenever an
instance of its type is created, removed, or has its referencing member
changed. Everything else is reused.

The contract is §9.4's: **the incremental result is observationally
identical to a full re-evaluation** — the same values, the same
diagnostics, in the same order. The parity harness checks it: after every
operation of a scripted session, the session's answers are compared with
a fresh session that replayed the log from the start. `:time` reports
what the last step recomputed.

Status: delivered. The engine records, for every step — a slot being
computed, a root being bound, an instance's asserts — the slots, roots,
and `$referrers` queries it read, and tags the diagnostics it produced;
a document operation rebinds the roots that changed (and the roots that
read them at binding), resets the slots that read anything under them
transitively, drops their diagnostics and instances, and forces the
universe again — what stayed `ok` is not recomputed — then re-runs the
asserts of the instances that are new or read what changed. Questions
without an operation reuse the last run; a bare expression evaluates
over it and leaves it as it was. `DECL_FULL_RECOMPUTE=1` makes every
question a full recomputation: the test suites run the corpus both ways
and require the same bytes.

## 7. Completion

Tab completes the input at the cursor with the same completion engine the
language server uses (03_lsp.md §5), applied to the scratch module's
scope — the loaded universe, the session's declarations, and `std` — so
the REPL and the editor never disagree about what a name can be:

| At | Candidates |
|---|---|
| the start of a line, after `:` | the commands, then the arguments each takes |
| a name | declarations of the universe and the session (roots, types, constants, functions), context variables where they are meaningful, `std` |
| after `.` or `?.` | the members of the expression's static type — hidden members as `x$` — with their kind and type; on a union, the members every arm declares; after `std.`, the namespaces, then the functions with signatures |
| inside `[…]` on a map | the map's keys, from the evaluated value when the map is under a root, else nothing |
| a key inside a literal checked against a record type (`{ na▌`, also in `:create` / `:update` values and in `with { … }`) | the members not yet written — required first, hidden members never, quoted when the dot cannot spell them; under `with`, the settable members only |
| a value after `key:` | what the member's type admits: the literals of a literal union or discriminant, `true` / `false` / `null`, the unit symbols of a `quantity<D>`, and for a `ref<T>` the navigations to places of type `T` under the roots (`ports["si0"]`, `nodes["u_mst0"].ports["si"]`) |
| a name in scope | also the parameters of the enclosing `func` or lambda, the variables of the enclosing comprehension, the bindings of the enclosing `match` arm — each followed by its members after `.` |
| after `match e {` | the arms of `e`'s union not yet covered, as `(name: Arm) =>` snippets |
| a string with a known domain | `"m" in e`: the optional members of `e`'s type; `import { … }`: the module's exports; `from "…"`: module paths of the workspace and packages of the lock |
| a template or pattern hole (`` `${…}` ``, `/${…}/`) | names as above; in a pattern, string- and integer-shaped types |
| inside a session declaration | after `$parent: ref<`, `$root: ref<`: types; after `else`: `error`, `warn`, `info`, and `diagnostic` names; after `unit x:`: dimensions; after `@`: `deprecated`, `doc(` |
| a keyword position | `if` / `then` / `else`, `for` / `in`, `match`, `with`, `matches`, `assert`, `when`; at the start of a line, the declaration keywords |
| after `$referrers(` | the types that carry a `ref` to the enclosing type, then their referencing members as strings |
| a root argument (`:evaluate`, `:validate`, `:bind`, `:unbind`, `:diff`, `:save`) | the roots the command accepts |
| a path argument (`:trace`, `:path`, `:create`, `:update`, `:remove`) | canonical paths, segment by segment, from the evaluated documents (`nodes["u_dom"]` offers the map's keys; `.ports` the record's members) |
| a file argument (`:load`, `:bind name=`, `:save`) | files of the working directory, `.decl` and `.json` first |
| the other command arguments | `:drop`: the session's declarations; `:set`: the options; `:help`: the commands; `:bind name = `: an expression, as above |
| a type position (`x: `, `:type`, `<…>`) | type names, generic parameters, dimensions, `ref<`, `quantity<` |
| after a number | unit symbols of the catalog |

A single candidate is inserted; several are listed with their kind and
type, and a common prefix is inserted. Inside a call, after `(` and each
`,`, the line editor shows the callee's signature with the active
parameter marked (signature help). Completion never evaluates more than
the candidates need: listing a map's keys, or the places a `ref<T>` may
name, demands those places and nothing else (partial evaluation, §2.1).

`:complete text` prints the candidates Tab would offer at the end of
`text`, one per line, in the order the editor would show them — the form
scripted sessions use, so that completion is covered by the parity
harness like every other answer (§9).

Status: today the session completes commands and their arguments (roots,
declarations, options, canonical paths segment by segment, files), names
in scope, members after `.` with their kind and type, `std.` namespaces
and functions, and context variables; the rows of the table that need
the expected type of a position (literal keys, typed values, match arms,
string domains, declaration positions) come with the language server's
completion engine (03_lsp.md §5), which the REPL will share.

## 8. Output

Everything the session prints goes to standard output, in the order the
inputs came — the REPL is a conversation, not a pipeline, and one stream
is what a transcript diffs (§9).

- Values: canonical JSON (§10.4), pretty-printed by default (`:set
  compact`, or `--compact`, for the wire form). Integers at full
  precision, floats in shortest round-trip form with `.0` where needed,
  quantities as `{ "value", "unit" }`, references as paths — exactly what
  `decl evaluate` writes.
- Diagnostics: the command line's form without the file, `severity
  [code] id at path: message`, in the command line's order (§6.7); a
  diagnostic of a module other than the entry carries the module's path
  after the message, as `(in path)`.
- Markers: `(partial)` after a bare expression's value or invalidity;
  `(invalid)` for a value the diagnostics excluded; a verdict line only
  after `:validate`.
- `:evaluate` with one root prints its document; with several, each
  document under a line naming the root (`u_oic:`); with none, the
  exported outputs as one object, as `decl evaluate` does. An
  error-severity diagnostic in a root prints the diagnostics and
  `(invalid)` in the document's place.
- `:validate` prints every diagnostic of the roots asked, then one
  verdict line per root — `oad: ok`, or `oad: 2 errors, 1 warning` — and,
  for several roots, nothing more: the verdict lines are the summary.
- `:roots` prints one line per root — kind, name, status (`exported`,
  `local`, `detached`, `session`; `bound`, `fallback`, `unbound`), the
  module, and the bound file — with `(edited)` after a document the
  session changed; `:session` one line per declaration and document;
  `:history` the operations numbered from `0  (start)`, the cursor marked
  `*`; `:time` milliseconds with one decimal, `total … (load …, check …,
  bind …, evaluate …)`, and after an incremental step `, recomputed N of
  M slots`.
- A command that cannot be carried out — an unknown command, wrong
  arguments, a root or path the session does not have, a file that cannot
  be read or written — prints one line, `error: <message>`, and changes
  nothing; it is not logged and does not stop a script.
- Every line the REPL prints is the same in the three implementations for
  the same session — the parity harness feeds each a session transcript
  and diffs the whole output.

### 8.1 Interrupting

Ctrl-C during an evaluation cancels it: the engine checks for
cancellation between slots (the mechanism the language server uses for
a cancelled request), the session prints `interrupted`, and the operation
that was running is not logged — the state is exactly what it was before
the input. Ctrl-C at the prompt discards the line being edited.

## 9. Scripted sessions

`decl repl --script session.txt` reads the session from a file instead of
the terminal (`--script -`: from standard input) and prints a transcript:
each input echoed behind the prompt, then its answers — exactly what the
terminal would have shown, so a transcript in a document or a test reads
like a session. There is no line editor, completion is `:complete`, and
the session ends at the end of the file or at `:quit`. The parity
harness and the corpus use this form; a session file is a plain text file
of inputs, one per line, with the same continuation rule as the terminal,
and `:history file` writes one from a live session — the durable form of
an editing session: the log, written down.
When standard input is not a terminal, `decl repl` without `--script`
reads it as `--script -` would: a session piped in prints its
transcript, the same bytes from every implementation.

The exit status is 0 when every input was accepted and 1 when some
input was refused (`error:` lines, §8): the language's diagnostics are
answers, not failures, so a script that must fail on an invalid document
ends with `:validate` and checks the verdict line, or runs
`decl validate` on what `:save` wrote.

## 10. What the REPL is not

- Not a debugger: there is nothing to step; `:trace` explains a value
  after the fact.
- Not a mutable store: an edit produces the next document, a declaration
  is replaced, never assigned; `:undo` recomputes, it does not reverse.
- Not a verdict on a document unless asked with `:validate`.
