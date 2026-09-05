// The language-server corpus (tests/lsp/<case>/) replayed over the stdio
// server: every session's answers, normalized, against its transcript.
import { check, total, lspServer } from './common/check.ts';
import { Session, StdioTransport, cases, compare } from './common/lsp-replay.ts';

console.log('== lsp: the session corpus over the stdio server ==');
for (const dir of cases()) {
  const got = await new Session(dir, new StdioTransport([process.execPath, lspServer])).run();
  compare(dir, got, check);
}
total();
