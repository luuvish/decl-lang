# The Decl website

Astro Starlight over the repository's documentation, plus a browser
playground. Published to GitHub Pages by `.github/workflows/site.yml`
on every push to `main` (repository Settings → Pages → Source: GitHub
Actions, once).

- `docs/` stays the single source of truth. `scripts/sync-docs.mjs`
  copies the specification, guide, design documents, revision log,
  validation cases (with their evaluated output), and the package
  READMEs into `src/content/docs/` at build time, adds frontmatter,
  and rewrites relative links. **Never edit the synced pages** — they
  are gitignored and regenerated; edit `docs/` instead.
- Hand-written pages: `src/content/docs/index.mdx` (landing),
  `start/`, `playground.mdx`.
- `grammars/decl.tmLanguage.json` highlights ```decl blocks
  (Expressive Code / Shiki); `src/components/decl-mode.ts` is the
  editor's stream mode.
- The playground imports `decl-lang/core` (the reference implementation's
  platform-neutral core; Vite bundles it) and loads the grammar's two
  wasm files, which `scripts/playground.mjs` copies into
  `public/playground/`.

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
self-hosted from Fontsource. Square corners, hairlines, no shadows.

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
npm run dev                       # sync + wasm copy + astro dev  (http://localhost:4321/decl-lang/)
npm run build                     # sync + wasm copy + astro build -> dist/
SITE_BASE=/ npm run build         # for a custom domain at the root
```
