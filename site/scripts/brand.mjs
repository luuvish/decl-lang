// The identity's derived files, from brand/palette.mjs:
//   src/styles/tokens.css   the custom properties of both themes (gitignored)
//   public/og.png           the social card, 1200 × 630: the sign, the wordmark, the
//                           tagline, the version — text as outlines from the site's
//                           own font files, so the card matches the pages
//   public/favicon.png      the ink tile for browsers that take no SVG favicon
// The SVG assets (src/assets/sign-*.svg, public/favicon.svg) are written by hand
// and committed; they carry the same path (SIGN). Fonts are the site's own
// Fontsource packages, so the card matches the pages.
import { writeFileSync, readFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { createRequire } from 'node:module';
import opentype from 'opentype.js';
import sharp from 'sharp';
import { palette, tokensCss, SIGN } from '../brand/palette.mjs';

const SITE = resolve(import.meta.dirname, '..');
const require = createRequire(import.meta.url);
const version = require('decl-lang/package.json').version;

// -- tokens
mkdirSync(resolve(SITE, 'src/styles'), { recursive: true });
writeFileSync(resolve(SITE, 'src/styles/tokens.css'), tokensCss());

// -- text as outlines: opentype.js reads the WOFF files Fontsource ships. Kerning is
// off (the mono has none; the sans loses a little) so that the outlines depend on
// nothing but the glyphs.
const font = (file) => {
  const b = readFileSync(require.resolve(file));
  return opentype.parse(b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength)); // a private buffer: glyphs are read lazily
};
const mono600 = font('@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-600-normal.woff');
const mono400 = font('@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff');
const sans400 = font('@fontsource/ibm-plex-sans/files/ibm-plex-sans-latin-400-normal.woff');
const OPTS = { kerning: false };
// the library's own serializer (toPathData) writes NaN into long paths; this one does not
const num = (v) => String(Number(v.toFixed(2)));
const pathData = (path) =>
  path.commands
    .map((c) =>
      c.type === 'Z' ? 'Z'
      : c.type === 'Q' ? `Q${num(c.x1)} ${num(c.y1)} ${num(c.x)} ${num(c.y)}`
      : c.type === 'C' ? `C${num(c.x1)} ${num(c.y1)} ${num(c.x2)} ${num(c.y2)} ${num(c.x)} ${num(c.y)}`
      : `${c.type}${num(c.x)} ${num(c.y)}`,
    )
    .join('');
/** the path of `text` at `size`, its start (or, anchored at the end, its end) at x, baseline y */
const text = (f, s, x, y, size, anchor = 'start') => {
  const width = f.getAdvanceWidth(s, size, OPTS);
  const x0 = anchor === 'end' ? x - width : x;
  return pathData(f.getPath(s, x0, y, size, OPTS));
};

// -- the social card: paper on ink, no colour
{
  const p = palette.dark;
  const W = 1200, H = 630, PAD = 80;
  const tagline = ['Describe structured data. Generate it.', 'Validate the world against it.'];
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
  <rect width="${W}" height="${H}" fill="#0F1012"/>
  <g fill="${p.ink}">
    <path transform="translate(${PAD} ${PAD}) scale(3)" d="${SIGN}"/>
    <path d="${text(mono600, 'decl', PAD + 150, PAD + 105, 112)}"/>
  </g>
  <g fill="${p.text}">
    <path d="${text(sans400, tagline[0], PAD, 392, 46)}"/>
    <path d="${text(sans400, tagline[1], PAD, 452, 46)}"/>
  </g>
  <g fill="${p.muted}">
    <path d="${text(mono400, `v${version} · one spec, three implementations`, PAD, H - PAD, 24)}"/>
    <path d="${text(mono400, 'decl-lang.org', W - PAD, H - PAD, 24, 'end')}"/>
  </g>
</svg>`;
  if (/NaN/.test(svg)) throw new Error('brand: an outline came out with NaN; the card would be cut short');
  await sharp(Buffer.from(svg)).png().toFile(resolve(SITE, 'public/og.png'));
}

// -- the favicon raster: the ink tile at 64 px
{
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 40 40">
  <rect width="40" height="40" rx="7" fill="#0F1012"/>
  <path transform="translate(4 4) scale(0.8)" fill="#F5F5F2" d="${SIGN}"/>
</svg>`;
  await sharp(Buffer.from(svg)).png().toFile(resolve(SITE, 'public/favicon.png'));
}

console.log('brand: tokens.css, og.png, favicon.png');
