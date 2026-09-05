// decl-lsp in a web worker (docs/tooling/04_extension.md §13): the
// server's core over an in-memory host. The client (the extension's
// browser entry) posts JSON-RPC messages to the worker, hands over the
// grammar and runtime wasm as base64 in `initializationOptions.wasm`,
// and keeps the host's files current with `decl/files` notifications.
import { connect } from './lsp-core.ts';
import { initParser } from './parse.ts';
import { memoryHost, setHost } from './host.ts';

const files = new Map<string, string>();
setHost(memoryHost(files, { cwd: '/' }));

const bytes = (b64: string): Uint8Array => {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
};

const worker: any = self;
const feed = connect({
  send: (msg) => worker.postMessage(msg),
  init: async (opts) => {
    const w = opts?.wasm ?? {};
    await initParser({
      grammar: bytes(w.grammar),
      runtime: w.runtime ? bytes(w.runtime) : undefined,
    });
  },
  exit: () => worker.close(),
});
worker.onmessage = (e: MessageEvent) => feed(e.data);
