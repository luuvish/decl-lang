// The Decl website: Starlight over the repository's docs/ (synced at
// build time by scripts/sync-docs.mjs — never edit the synced pages
// here), a landing page, and the browser playground.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { readFileSync } from 'node:fs';

const decl = JSON.parse(readFileSync(new URL('./grammars/decl.tmLanguage.json', import.meta.url), 'utf8'));
const base = process.env.SITE_BASE ?? '/decl-lang';

export default defineConfig({
  site: process.env.SITE_URL ?? 'https://luuvish.github.io',
  base,
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Decl',
      description: 'A declarative language for describing, generating, and validating structured data.',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/luuvish/decl-lang' }],
      customCss: ['./src/styles/custom.css'],
      expressiveCode: {
        shiki: { langs: [decl] },
      },
      sidebar: [
        {
          label: 'Start',
          items: [
            { label: 'Install', slug: 'start/install' },
            { label: 'Command line', slug: 'start/cli' },
            { label: 'Playground', slug: 'playground' },
          ],
        },
        { label: 'Guide', items: [{ autogenerate: { directory: 'guide' } }] },
        { label: 'Examples', items: [{ autogenerate: { directory: 'examples' } }] },
        { label: 'Specification', items: [{ autogenerate: { directory: 'specification' } }] },
        { label: 'Tooling', items: [{ autogenerate: { directory: 'tooling' } }] },
        { label: 'Design', collapsed: true, items: [{ autogenerate: { directory: 'design' } }] },
        { label: 'Revisions', slug: 'revisions' },
      ],
    }),
  ],
});
