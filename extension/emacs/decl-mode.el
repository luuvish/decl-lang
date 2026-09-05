;;; decl-mode.el --- Major mode for Decl over tree-sitter  -*- lexical-binding: t; -*-

;; Decl in Emacs 29+ (docs/tooling/04_extension.md §17): not an
;; extension — the built-in `treesit' for highlighting, indentation, and
;; imenu over the tree-sitter grammar, and the built-in `eglot' for
;; `decl-lsp'.  The grammar library must be loadable as
;; libtree-sitter-decl.dylib (or .so): from the repository,
;;
;;   cd tree-sitter-decl && tree-sitter build -o ~/.emacs.d/tree-sitter/libtree-sitter-decl.dylib
;;
;; or, with the source alist below, M-x treesit-install-language-grammar.
;; `treesit-extra-load-path' names other directories to search.

;;; Code:

(require 'treesit)
(require 'prog-mode)

(defgroup decl nil
  "Decl: a declarative language for structured data."
  :group 'languages)

(defcustom decl-ts-mode-indent-offset 4
  "Indentation inside a bracketed body (the language's canonical form)."
  :type 'integer
  :safe 'integerp
  :group 'decl)

(add-to-list 'treesit-language-source-alist
             '(decl "https://github.com/luuvish/decl-lang" nil "tree-sitter-decl"))

(defvar decl-ts-mode--keywords
  '("type" "const" "func" "output" "input" "export" "import" "from" "as"
    "dimension" "unit" "diagnostic" "assert" "when"
    "if" "then" "else" "match" "for" "in" "matches" "with")
  "The keywords, as tree-sitter-decl/queries/highlights.scm lists them.")

(defvar decl-ts-mode--operators
  '("==" "!=" "<=" ">=" "&&" "||" "??" "|>" "=>" "<<" ">>"
    "+" "-" "*" "/" "%" "!" "~" "^" "&" "|" ".." "..<" "?.")
  "The operators, as tree-sitter-decl/queries/highlights.scm lists them.")

(defvar decl-ts-mode--font-lock-settings
  (treesit-font-lock-rules
   :language 'decl :feature 'comment
   '((line_comment) @font-lock-comment-face
     (block_comment) @font-lock-comment-face
     (doc_comment) @font-lock-doc-face)

   :language 'decl :feature 'keyword
   `([,@decl-ts-mode--keywords] @font-lock-keyword-face
     (severity) @font-lock-keyword-face)

   :language 'decl :feature 'string
   '((string) @font-lock-string-face
     (template_string) @font-lock-string-face
     (template_chars) @font-lock-string-face
     (template_escape) @font-lock-escape-face
     (pattern) @font-lock-regexp-face
     (interpolation "${" @font-lock-misc-punctuation-face "}" @font-lock-misc-punctuation-face))

   :language 'decl :feature 'constant
   '(["true" "false" "null"] @font-lock-constant-face)

   :language 'decl :feature 'number
   '((int) @font-lock-number-face
     (float) @font-lock-number-face
     (unit_literal) @font-lock-number-face)

   :language 'decl :feature 'type
   '((type_declaration name: (identifier) @font-lock-type-face)
     (named_type (qualified_name (identifier) @font-lock-type-face :anchor))
     (type_parameter (identifier) @font-lock-type-face))

   :language 'decl :feature 'definition
   '((func_declaration name: (identifier) @font-lock-function-name-face)
     (value_member name: (identifier) @font-lock-property-name-face)
     (derived_member name: (identifier) @font-lock-property-name-face)
     (hidden_member name: (hidden_name) @font-lock-property-name-face)
     (object_entry key: (identifier) @font-lock-property-name-face)
     (assert_member name: (identifier) @font-lock-constant-face)
     (diagnostic_declaration name: (identifier) @font-lock-constant-face)
     (parameter (identifier) @font-lock-variable-name-face)
     (lambda_parameter (identifier) @font-lock-variable-name-face)
     (for_clause variable: (identifier) @font-lock-variable-name-face))

   :language 'decl :feature 'function
   '((call (member_access (identifier) @font-lock-function-call-face :anchor))
     (call (identifier) @font-lock-function-call-face))

   :language 'decl :feature 'property
   '((member_access (identifier) @font-lock-property-use-face :anchor))

   :language 'decl :feature 'builtin
   '((context_variable) @font-lock-builtin-face
     "$referrers" @font-lock-builtin-face)

   :language 'decl :feature 'operator
   `([,@decl-ts-mode--operators] @font-lock-operator-face)

   :language 'decl :feature 'bracket
   '(["{" "}" "[" "]" "(" ")"] @font-lock-bracket-face)

   :language 'decl :feature 'delimiter
   '(["," ":" "=" "." "?"] @font-lock-delimiter-face
     "..." @font-lock-misc-punctuation-face))
  "Font-lock rules: what tree-sitter-decl/queries/highlights.scm captures.")

(defvar decl-ts-mode--indent-rules
  `((decl
     ((parent-is "module") column-0 0)
     ((node-is "}") parent-bol 0)
     ((node-is "]") parent-bol 0)
     ((node-is ")") parent-bol 0)
     ((parent-is "record_type") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "object") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "array") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "paren_expression") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "paren_type") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "type_arguments") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "match_expression") parent-bol decl-ts-mode-indent-offset)
     ((parent-is "call") parent-bol decl-ts-mode-indent-offset)
     (no-node parent-bol 0)
     (catch-all parent-bol 0)))
  "Indentation: a body indents; its closer sits on the opener's column.")

(defvar decl-ts-mode--syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?/ ". 124b" table)
    (modify-syntax-entry ?* ". 23" table)
    (modify-syntax-entry ?\n "> b" table)
    (modify-syntax-entry ?\" "\"" table)
    (modify-syntax-entry ?` "\"" table)
    (modify-syntax-entry ?\\ "\\" table)
    (modify-syntax-entry ?_ "_" table)
    (modify-syntax-entry ?$ "_" table)
    (modify-syntax-entry ?' "." table)
    table)
  "Syntax table: `//' and `/* */' comments, \" and ` strings.")

(defun decl-ts-mode--defun-name (node)
  "The name of the declaration NODE, for imenu and `which-function'."
  (when-let* ((name (treesit-node-child-by-field-name node "name")))
    (treesit-node-text name t)))

;;;###autoload
(define-derived-mode decl-ts-mode prog-mode "Decl"
  "Major mode for Decl, over tree-sitter."
  :group 'decl
  :syntax-table decl-ts-mode--syntax-table
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local comment-start-skip "\\(?://+\\|/\\*+\\)\\s-*")
  (setq-local indent-tabs-mode nil)
  (setq-local tab-width decl-ts-mode-indent-offset)
  (when (treesit-ready-p 'decl)
    (treesit-parser-create 'decl)
    (setq-local treesit-font-lock-settings decl-ts-mode--font-lock-settings)
    (setq-local treesit-font-lock-feature-list
                '((comment definition)
                  (keyword string type)
                  (constant number function property builtin)
                  (operator bracket delimiter)))
    (setq-local treesit-simple-indent-rules decl-ts-mode--indent-rules)
    (setq-local treesit-defun-type-regexp
                (rx bos (or "type_declaration" "const_declaration" "func_declaration"
                            "output_declaration" "input_declaration" "diagnostic_declaration"
                            "dimension_declaration" "unit_declaration")
                    eos))
    (setq-local treesit-defun-name-function #'decl-ts-mode--defun-name)
    (setq-local treesit-simple-imenu-settings
                '(("Type" "\\`type_declaration\\'" nil nil)
                  ("Function" "\\`func_declaration\\'" nil nil)
                  ("Const" "\\`const_declaration\\'" nil nil)
                  ("Output" "\\`output_declaration\\'" nil nil)
                  ("Input" "\\`input_declaration\\'" nil nil)
                  ("Diagnostic" "\\`diagnostic_declaration\\'" nil nil)))
    (treesit-major-mode-setup)))

;;;###autoload
(add-to-list 'auto-mode-alist '("\\.decl\\'" . decl-ts-mode))

;; the language server (docs/tooling/03_lsp.md): `decl-lsp' from PATH,
;; the language id the server expects
(defvar eglot-server-programs)
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '((decl-ts-mode :language-id "decl") . ("decl-lsp"))))

(provide 'decl-mode)
;;; decl-mode.el ends here
