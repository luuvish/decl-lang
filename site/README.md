# The Decl website

Astro Starlight over the repository's documentation, plus a browser
playground. Published to GitHub Pages by `.github/workflows/site.yml`
on every push to `main` (repository Settings → Pages → Source: GitHub
Actions, once).

The homepage leads with **Describe your data.** A paper model shows an ownership
tree with blue reference threads crossing its branches. Its reading order is the Service
Graph, identity in immutable data, the three uses of one model, the hardware
case, core philosophy, and installation. The opening's single action points to
the on-page model; browser execution and the detailed guide follow that model,
and installation lives in the final "Use Decl in your project" section. See
[the positioning document](../docs/design/04_positioning.md) for the message
architecture and the distinction between current capabilities and applications.
The identity panel is evaluated from the guide's shared-golden example; the
Service Graph and identity panels fail the build if their evaluation fails.
The Start and Guide navigation lists each tutorial once in reading order;
update `astro.config.mjs` when adding a guide.

- `docs/` stays the single source of truth. `scripts/sync-docs.mjs`
  copies the specification, guide, design documents, revision log,
  validation cases (with their evaluated output), and the package
  READMEs into `src/content/docs/` at build time, adds frontmatter,
  and rewrites relative links. **Never edit the synced pages** — they
  are gitignored and regenerated; edit `docs/` instead. A tooling
  document whose Status section begins with "Planned" describes a tool
  that does not exist yet; it stays off the site (links to it go to the
  repository) until it ships.
- Hand-written pages: `src/content/docs/index.mdx` (landing),
  `start/`, `playground.mdx`, `search.mdx` (the search results as a
  page). Starlight components overridden in `src/components/`:
  `Hero.astro` (the illustrated opening, responsive images, and a native
  enlargement dialog), `Header.astro` (Guide and Playground links on the
  homepage; documentation pages retain their searchable header),
  `Search.astro` (a field in the header instead of a dialog; Enter opens
  the search page), `ThemeToggle.astro` (one button, light or dark,
  instead of a select), `Footer.astro` (the colophon).
- `grammars/decl.tmLanguage.json` highlights ```decl blocks
  (Expressive Code / Shiki); `src/components/decl-mode.ts` is the
  editor's stream mode.
- The playground imports `decl-lang/core` (the reference implementation's
  platform-neutral core; Vite bundles it) and loads the grammar's two
  wasm files, which `scripts/playground.mjs` copies into
  `public/playground/`. It runs the core's `Session` over an in-memory
  host — the session behind the REPL and the language server — so an
  `input` root can be bound to a JSON document pasted into the Inputs
  panel, and diagnostics are shown in place: in the module by the
  server's own anchoring (`diagnosticsFor`), in a document at the value
  its path leads to. The example picker is `src/samples/synced/examples.json`,
  which `sync-docs.mjs` builds from the landing page's module, the
  tutorials (assembled as the golden corpus assembles them, with the
  documents it binds), and the validation cases; a shared link carries
  the module and its documents (`#code=…&in.<root>=…`).

## The identity

The site is the language's visual identity, and the identity is one
rule and one drawing. The **mark** is the subsumption sign ⊑, the
relation every judgement Decl makes is an instance of (`v ⊑ T`): a
24-unit bracket of 4-unit stroke over a bar, one colour, drawn at the
weight of the wordmark beside it, `decl` in IBM Plex Mono 600. The
**palette** is Graphite: no accent hue. Reading text is graphite, a
soft near-black; anything you can act on (a link, a button, the
current page, the lockup) is ink, the darkest value on the page, and
links are underlined besides; the current page sits on a grey field
with a two-pixel ink bar; selection and search hits take a tint of the
one colour the site already has, the blue that types are set in. The
only chroma on a reading page is the code: six syntax roles (keywords
in bold ink, types blue, strings warm, numbers and quantities green,
comments quiet and italic, punctuation grey) and the four severities.
The **faces**: Literata for prose and headings, IBM Plex Sans for the
interface, IBM Plex Mono for code and the wordmark, with coding
ligatures off so `..`, `||`, and `!=` look like what you type;
self-hosted from Fontsource. Square corners, hairlines, no UI shadows.
The homepage illustration is a deliberate exception: tactile ivory paper,
soft photographic shadows, and blue threads distinguish references from
the graphite ownership tree. The page title, introduction, caption, and
navigation remain live text. Image annotations are also described in alt
text and in the runnable example immediately below. On small screens the
full composition scales down; selecting it opens a keyboard-accessible
dialog with a scrollable full-size image. Escape or Close dismisses it.

Where it lives:

- `brand/palette.mjs` — both themes, the syntax roles, the severities,
  and the mark's path, as data; `brand/syntax.mjs` builds the two
  code-block themes from it.
- `scripts/brand.mjs` (run by `prepare:content`) — writes
  `src/styles/tokens.css` (the custom properties of both themes),
  `public/og.png` (the social card, text set as outlines from the
  same fonts), and `public/favicon.png`; all three gitignored.
- `src/styles/custom.css` — everything that is not a colour: the
  faces, the scale, the ink rule, the sidebar, the surfaces.
- `src/assets/sign-light.svg`, `sign-dark.svg` — the mark, Starlight's
  logo beside the title; `public/favicon.svg` — the mark on its tile,
  ink for dark tabs and paper for light ones through a media query.
- `src/assets/model-paper.png` and `model-paper-dark.png` — the selected paper
  tree/reference graph, extracted from the approved mockup with built-in
  ImageGen. The dark variant changes only the surrounding backdrop to charcoal;
  the physical paper stays ivory. Prompts are preserved in
  `brand/model-paper.prompt.txt` and `brand/model-paper-dark.prompt.txt`.
  Astro produces responsive WebP sizes at build time; the original PNGs stay
  in the source tree. Both the hero and its enlargement follow the page's
  `data-theme`, including an explicit user choice that differs from the OS.
  The enlargement label uses the corresponding theme's ink color. Preserve
  the six nodes, reference destinations, and derived values
  when revising the artwork, and compare them with `src/samples/hero.decl`.
- `src/components/decl-mode.ts` — the playground editor reads the same
  six roles as `--decl-syn-*`.

Credits. The faces are free fonts under the SIL Open Font License 1.1
(<https://openfontlicense.org>): Literata is © 2017 The Literata Project
Authors (<https://github.com/googlefonts/literata>); IBM Plex Sans and
IBM Plex Mono are © 2017–2019 IBM Corp. (<https://github.com/IBM/plex>).
They are served unmodified, and the copyright and license records the
files carry are the notice the license asks for; the site's footer
(`src/components/Footer.astro`) repeats it. The mark is the Unicode
character ⊑ (U+2291) drawn by hand on a grid, not a glyph taken from
any font. The icons on the landing page and in the header are
Starlight's own set (Unicons, Apache 2.0; Simple Icons, CC0); Starlight,
Astro, Expressive Code, CodeMirror, Pagefind, and web-tree-sitter are
MIT, and the build-time renderers sharp (Apache 2.0) and opentype.js
(MIT) ship nothing to the browser.

```bash
npm install                       # once, at the repository root (npm workspaces)
cd site
npm run dev                       # sync + wasm copy + astro dev  (http://localhost:4321/)
npm run build                     # sync + wasm copy + astro build -> dist/, for decl-lang.org (the root)
SITE_BASE=/decl-lang SITE_URL=https://luuvish.github.io npm run build   # for a project page under a path
```
