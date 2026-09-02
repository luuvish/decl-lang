// Browser stand-ins for the Node built-ins the reference implementation
// imports at module level (fs/path/url/crypto). The playground bundle
// (scripts/build-web.mjs) maps every such import here; nothing that
// touches the file system is ever called in the browser — the entry
// (web.ts) only parses, checks, evaluates, and formats source text.
const unavailable = (name: string) => (p?: string): never => {
  throw new Error(`${name} is unavailable in the browser${p ? `: ${p}` : ''}`);
};

export const existsSync = (_p: string) => false;
export const readFileSync = unavailable('readFileSync');
export const readdirSync = unavailable('readdirSync');
export const statSync = unavailable('statSync');
export const writeFileSync = unavailable('writeFileSync');
export const mkdirSync = unavailable('mkdirSync');
export const readFile = unavailable('readFile');

export const sep = '/';
export const join = (...parts: string[]) => parts.filter(Boolean).join('/').replace(/([^:])\/{2,}/g, '$1/');
export const resolve = join;
export const dirname = (p: string) => p.replace(/\/[^/]*$/, '');
export const basename = (p: string) => p.slice(p.lastIndexOf('/') + 1);
export const relative = (from: string, to: string) => (to.startsWith(from) ? to.slice(from.length).replace(/^\//, '') : to);
export const fileURLToPath = (u: string | URL) => String(u);
export const pathToFileURL = (p: string) => new URL(p, 'file:///');

export const createHash = () => ({ update() { return this; }, digest: () => '' });
export const createRequire = () => (_id: string) => shims;

const shims = {
  existsSync, readFileSync, readdirSync, statSync, writeFileSync, mkdirSync, readFile,
  sep, join, resolve, dirname, basename, relative, fileURLToPath, pathToFileURL, createHash, createRequire,
};
export default shims;
