// External scanner: the newline-separator rule of spec §2.9.
// NEWLINE is emitted exactly when the parse state admits it — "the
// token before can end an element and the token after can begin one"
// is what valid_symbols encodes. Everywhere else, line breaks are
// plain whitespace.

#include "tree_sitter/parser.h"

enum TokenType { NEWLINE };

void *tree_sitter_decl_external_scanner_create(void) { return NULL; }
void tree_sitter_decl_external_scanner_destroy(void *p) {}
unsigned tree_sitter_decl_external_scanner_serialize(void *p, char *b) { return 0; }
void tree_sitter_decl_external_scanner_deserialize(void *p, const char *b, unsigned n) {}

bool tree_sitter_decl_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
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
