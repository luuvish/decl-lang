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
- The playground runs the reference implementation in the browser:
  `npm run build` in packages/typescript bundles `src/web.ts` with esbuild into `dist/web.js` (`decl-lang/web`); `scripts/playground.mjs` copies it
  to `public/playground/decl.js` next to the two wasm files.

```bash
npm install                       # once, at the repository root (npm workspaces)
cd site
npm run dev                       # sync + bundle + astro dev  (http://localhost:4321/decl-lang/)
npm run build                     # sync + bundle + astro build -> dist/
SITE_BASE=/ npm run build         # for a custom domain at the root
```
