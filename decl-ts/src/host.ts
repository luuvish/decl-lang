// The host: how the platform-neutral core reaches files, paths, and the
// environment. Node installs a file-system host (node.ts); a browser
// installs an in-memory one fed by the editor (lsp-web.ts); nothing
// below this line imports a Node built-in. Paths are POSIX strings
// everywhere in the core; a host on another convention translates.

export interface Host {
  /** the text of a file, or null when it cannot be read */
  readFile(path: string): string | null;
  /** the entries of a directory (any order), [] when it cannot be read */
  readDir(path: string): string[];
  isDir(path: string): boolean;
  exists(path: string): boolean;
  /** write a file; throws when it cannot */
  writeFile(path: string, text: string): void;
  cwd(): string;
  pathOf(uri: string): string;
  uriOf(path: string): string;
  env(name: string): string | undefined;
}

/** an in-memory host over a map of path → text (the browser, the tests) */
export function memoryHost(
  files: Map<string, string>,
  opts: { cwd?: string; uriPrefix?: string } = {},
): Host & { files: Map<string, string>; uriPrefix: string } {
  const h = {
    files,
    uriPrefix: opts.uriPrefix ?? 'file://',
    readFile: (p: string) => files.get(normalize(p)) ?? null,
    readDir: (p: string) => {
      const dir = normalize(p).replace(/\/$/, '') + '/';
      const out = new Set<string>();
      for (const f of files.keys())
        if (f.startsWith(dir)) out.add(f.slice(dir.length).split('/')[0]);
      return [...out];
    },
    isDir: (p: string) => {
      const dir = normalize(p).replace(/\/$/, '') + '/';
      for (const f of files.keys()) if (f.startsWith(dir)) return true;
      return false;
    },
    exists: (p: string) => files.has(normalize(p)) || h.isDir(p),
    writeFile: (p: string, text: string) => {
      files.set(normalize(p), text);
    },
    cwd: () => opts.cwd ?? '/',
    pathOf: (uri: string) => {
      const m = /^([a-z][a-z0-9+.-]*:\/\/[^/]*)(\/.*)$/i.exec(uri);
      if (!m) return uri;
      h.uriPrefix = m[1];
      return decodeURIComponent(m[2]);
    },
    uriOf: (p: string) =>
      h.uriPrefix +
      p
        .split('/')
        .map((s) => encodeURIComponent(s))
        .join('/'),
    env: () => undefined,
  };
  return h;
}

export let host: Host = memoryHost(new Map());
export function setHost(h: Host): void {
  host = h;
}

// ---------------- paths (POSIX) ----------------
export function normalize(p: string): string {
  const abs = p.startsWith('/');
  const out: string[] = [];
  for (const seg of p.split('/')) {
    if (!seg || seg === '.') continue;
    if (seg === '..') {
      if (out.length && out[out.length - 1] !== '..') out.pop();
      else if (!abs) out.push('..');
    } else out.push(seg);
  }
  const body = out.join('/');
  return abs ? '/' + body : body || '.';
}
/** an absolute path: the parts joined, the last absolute one winning, relative to the host's cwd */
export function resolvePath(...parts: string[]): string {
  let p = '';
  for (const part of parts) {
    if (!part) continue;
    p = part.startsWith('/') || !p ? part : `${p}/${part}`;
  }
  if (!p.startsWith('/')) p = `${host.cwd()}/${p}`;
  return normalize(p);
}
export function dirname(p: string): string {
  const n = p.replace(/\/+$/, '');
  const i = n.lastIndexOf('/');
  return i < 0 ? '.' : i === 0 ? '/' : n.slice(0, i);
}
export function basename(p: string): string {
  const n = p.replace(/\/+$/, '');
  return n.slice(n.lastIndexOf('/') + 1);
}
export function join(...parts: string[]): string {
  return normalize(parts.filter(Boolean).join('/'));
}
export function relative(from: string, to: string): string {
  const f = normalize(from).split('/').filter(Boolean),
    t = normalize(to).split('/').filter(Boolean);
  let i = 0;
  while (i < f.length && i < t.length && f[i] === t[i]) i++;
  return [...f.slice(i).map(() => '..'), ...t.slice(i)].join('/');
}

// ---------------- SHA-256 (the package content hash, §8.7) ----------------
const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);
export function sha256Hex(chunks: (string | Uint8Array)[]): string {
  const enc = new TextEncoder();
  const parts = chunks.map((c) => (typeof c === 'string' ? enc.encode(c) : c));
  const len = parts.reduce((n, p) => n + p.length, 0);
  const padded = new Uint8Array(((len + 9 + 63) >> 6) << 6);
  let off = 0;
  for (const p of parts) {
    padded.set(p, off);
    off += p.length;
  }
  padded[len] = 0x80;
  const dv = new DataView(padded.buffer);
  dv.setUint32(padded.length - 8, Math.floor((len * 8) / 0x100000000));
  dv.setUint32(padded.length - 4, (len * 8) >>> 0);
  const H = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const W = new Uint32Array(64);
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n));
  for (let i = 0; i < padded.length; i += 64) {
    for (let t = 0; t < 16; t++) W[t] = dv.getUint32(i + t * 4);
    for (let t = 16; t < 64; t++) {
      const s0 = rotr(W[t - 15], 7) ^ rotr(W[t - 15], 18) ^ (W[t - 15] >>> 3);
      const s1 = rotr(W[t - 2], 17) ^ rotr(W[t - 2], 19) ^ (W[t - 2] >>> 10);
      W[t] = (W[t - 16] + s0 + W[t - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = H;
    for (let t = 0; t < 64; t++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K[t] + W[t]) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    H[0] = (H[0] + a) >>> 0;
    H[1] = (H[1] + b) >>> 0;
    H[2] = (H[2] + c) >>> 0;
    H[3] = (H[3] + d) >>> 0;
    H[4] = (H[4] + e) >>> 0;
    H[5] = (H[5] + f) >>> 0;
    H[6] = (H[6] + g) >>> 0;
    H[7] = (H[7] + h) >>> 0;
  }
  return [...H].map((x) => x.toString(16).padStart(8, '0')).join('');
}
