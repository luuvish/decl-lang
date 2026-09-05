#!/usr/bin/env node
// decl-lsp over stdio (docs/tooling/03_lsp.md): the Node transport of the
// server's core (lsp-core.ts). Messages are handled strictly in order,
// and the server exits when its input closes.
import { initParser } from './node.ts';
import { connect, drained } from './lsp-core.ts';

const feed = connect({
  send: msg => {
    const body = JSON.stringify(msg);
    process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
  },
  init: () => initParser(),
  exit: () => process.exit(0),
});

let buffer = Buffer.alloc(0);
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  for (; ;) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const header = buffer.subarray(0, headerEnd).toString();
    const m = /Content-Length: (\d+)/i.exec(header);
    if (!m) { buffer = buffer.subarray(headerEnd + 4); continue; }
    const len = parseInt(m[1], 10);
    if (buffer.length < headerEnd + 4 + len) return;
    const body = buffer.subarray(headerEnd + 4, headerEnd + 4 + len).toString();
    buffer = buffer.subarray(headerEnd + 4 + len);
    feed(JSON.parse(body));
  }
});
process.stdin.on('end', () => { drained().then(() => process.exit(0)); });
