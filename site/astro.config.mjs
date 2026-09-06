// The Decl website: Starlight over the repository's docs/ (synced at
// build time by scripts/sync-docs.mjs — never edit the synced pages
// here), a landing page, and the browser playground.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { ExpressiveCodeTheme } from '@astrojs/starlight/expressive-code';
import { readFileSync } from 'node:fs';
import { codeTheme } from './brand/syntax.mjs';

const decl = JSON.parse(readFileSync(new URL('./grammars/decl.tmLanguage.json', import.meta.url), 'utf8'));
// the site lives at the root of its own domain (the repository's CNAME);
// SITE_BASE / SITE_URL rebuild it for a project page (`/decl-lang` under
// luuvish.github.io) or any other host
const base = process.env.SITE_BASE ?? '/';
const site = process.env.SITE_URL ?? 'https://decl-lang.org';
const description = 'A declarative language for describing, generating, and validating structured data.';
const card = `${site}${base.replace(/\/$/, '')}/og.png`;

export default defineConfig({
  site,
  base,
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Decl',
      description,
      // the identity (brand/palette.mjs, src/styles/custom.css, site/README.md):
      // the sign ⊑ beside the wordmark, the tile as the favicon, the card on every page
      logo: { light: './src/assets/sign-light.svg', dark: './src/assets/sign-dark.svg', alt: '' },
      favicon: '/favicon.svg',
      head: [
        { tag: 'link', attrs: { rel: 'icon', href: `${base.replace(/\/$/, '')}/favicon.png`, type: 'image/png', sizes: '64x64' } },
        { tag: 'meta', attrs: { property: 'og:image', content: card } },
        { tag: 'meta', attrs: { property: 'og:image:width', content: '1200' } },
        { tag: 'meta', attrs: { property: 'og:image:height', content: '630' } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
        { tag: 'meta', attrs: { name: 'twitter:image', content: card } },
      ],
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/luuvish/decl-lang' }],
      components: {
        Footer: './src/components/Footer.astro', // the default footer plus the colophon
        ThemeSelect: './src/components/ThemeToggle.astro', // one button, light or dark
        Search: './src/components/Search.astro', // a field in the header; the results are a page
      },
      customCss: [
        '@fontsource-variable/literata/wght.css',
        '@fontsource-variable/literata/wght-italic.css',
        '@fontsource/ibm-plex-sans/400.css',
        '@fontsource/ibm-plex-sans/500.css',
        '@fontsource/ibm-plex-sans/600.css',
        '@fontsource/ibm-plex-mono/400.css',
        '@fontsource/ibm-plex-mono/400-italic.css',
        '@fontsource/ibm-plex-mono/500.css',
        '@fontsource/ibm-plex-mono/600.css',
        './src/styles/tokens.css',
        './src/styles/custom.css',
      ],
      expressiveCode: {
        shiki: { langs: [decl] },
        themes: [new ExpressiveCodeTheme(codeTheme('dark')), new ExpressiveCodeTheme(codeTheme('light'))],
        styleOverrides: {
          borderRadius: '0.125rem',
          borderColor: 'var(--sl-color-hairline)',
          codeFontFamily: 'var(--__sl-font-mono)',
          uiFontFamily: 'var(--decl-font-ui)',
          codeBackground: 'var(--sl-color-gray-6)',
          frames: { frameBoxShadowCssValue: 'none', editorActiveTabIndicatorTopColor: 'var(--sl-color-white)', editorActiveTabIndicatorBottomColor: 'transparent' },
        },
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
        {
          label: 'Reference',
          items: [
            { label: 'Standard library', slug: 'reference/stdlib' },
            { label: 'Diagnostic codes', slug: 'reference/diagnostics' },
          ],
        },
        { label: 'Tooling', items: [{ autogenerate: { directory: 'tooling' } }] },
        { label: 'Design', collapsed: true, items: [{ autogenerate: { directory: 'design' } }] },
        { label: 'Revisions', slug: 'revisions' },
      ],
    }),
  ],
});
