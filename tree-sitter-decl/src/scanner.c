// External scanner:
//  - NEWLINE: the separator rule of spec §2.9 — emitted exactly when
//    the parse state admits it (valid_symbols is "the token before can
//    end an element and the token after can begin one").
//  - BLOCK_COMMENT: /* … */ with nesting (spec §2.2) — beyond regex.

#include "tree_sitter/parser.h"

enum TokenType { NEWLINE, BLOCK_COMMENT };

void *tree_sitter_decl_external_scanner_create(void) { return NULL; }
void tree_sitter_decl_external_scanner_destroy(void *p) {}
unsigned tree_sitter_decl_external_scanner_serialize(void *p, char *b) { return 0; }
void tree_sitter_decl_external_scanner_deserialize(void *p, const char *b, unsigned n) {}

static bool scan_block_comment(TSLexer *lexer) {
  // at '/': commit only if '*' follows (a false return discards)
  lexer->advance(lexer, false);
  if (lexer->lookahead != '*') return false;
  lexer->advance(lexer, false);
  unsigned depth = 1;
  while (depth > 0) {
    if (lexer->eof(lexer)) return false;            // unterminated: E1005 via parse error
    int32_t c = lexer->lookahead;
    lexer->advance(lexer, false);
    if (c == '/' && lexer->lookahead == '*') { depth++; lexer->advance(lexer, false); }
    else if (c == '*' && lexer->lookahead == '/') { depth--; lexer->advance(lexer, false); }
  }
  lexer->result_symbol = BLOCK_COMMENT;
  lexer->mark_end(lexer);
  return true;
}

bool tree_sitter_decl_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
  if (valid_symbols[BLOCK_COMMENT] && lexer->lookahead == '/') {
    return scan_block_comment(lexer);
  }

  if (!valid_symbols[NEWLINE]) return false;

  bool saw_newline = false;
  for (;;) {
    int32_t c = lexer->lookahead;
    if (c == '\n') {
      saw_newline = true;
      lexer->advance(lexer, false);
    } else if (c == ' ' || c == '\t' || c == '\r') {
      lexer->advance(lexer, saw_newline ? false : true);
    } else {
      break;
    }
  }
  if (!saw_newline) return false;
  lexer->result_symbol = NEWLINE;
  lexer->mark_end(lexer);
  return true;
}
