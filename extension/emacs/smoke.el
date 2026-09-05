;;; smoke.el --- the batch check extension/emacs/smoke.sh runs  -*- lexical-binding: t; -*-

;; Loaded after decl-mode.el with DECL_ROOT (the repository), DECL_LSP
;; (the server), and DECL_WORK (a scratch directory holding
;; tree-sitter/libtree-sitter-decl.*) in the environment: the grammar
;; library loads, a fixture opens in the mode with keywords and comments
;; fontified and its body indented, eglot connects to the server and
;; flymake shows the fixture's diagnostic, and hover on a type name
;; answers with its declaration.

(require 'eglot)
(require 'flymake)
(require 'jsonrpc)

(defvar decl-smoke--failed nil)
(defun decl-smoke (label ok &optional detail)
  (princ (format "emacs: %s %s%s\n" (if ok "ok  " "FAIL") label
                 (if (and detail (not ok)) (format " — %s" detail) "")))
  (unless ok (setq decl-smoke--failed t)))

(defun decl-smoke--face-at (pos)
  (let ((f (get-text-property pos 'face)))
    (if (listp f) (car f) f)))

(defun decl-smoke--hover-text (result)
  (let ((contents (plist-get result :contents)))
    (cond ((stringp contents) contents)
          ((and (listp contents) (plist-get contents :value)) (plist-get contents :value))
          ((vectorp contents) (mapconcat (lambda (c) (if (stringp c) c (plist-get c :value))) contents "\n"))
          (t (format "%S" contents)))))

(let* ((root (file-name-as-directory (getenv "DECL_ROOT")))
       (lsp (getenv "DECL_LSP"))
       (work (file-name-as-directory (getenv "DECL_WORK")))
       (fixture (concat root "tests/validation/constraints/invalid/assert_no_name.decl"))
       (example (concat root "docs/examples/02_config.decl")))
  (setq treesit-extra-load-path (list (concat work "tree-sitter")))
  (decl-smoke "the grammar library loads (treesit-ready-p 'decl)" (treesit-ready-p 'decl))

  ;; ---- highlighting and indentation, on the invalid fixture
  (find-file fixture)
  (decl-smoke "a .decl file opens in decl-ts-mode" (eq major-mode 'decl-ts-mode) (format "%S" major-mode))
  (font-lock-ensure)
  (goto-char (point-min))
  (decl-smoke "a comment has font-lock-comment-face"
              (eq (decl-smoke--face-at (point)) 'font-lock-comment-face) (format "%S" (decl-smoke--face-at (point))))
  (search-forward "type Bad")
  (let ((kw (match-beginning 0)) (name (- (match-end 0) 3)))
    (decl-smoke "`type` has font-lock-keyword-face"
                (eq (decl-smoke--face-at kw) 'font-lock-keyword-face) (format "%S" (decl-smoke--face-at kw)))
    (decl-smoke "the declared name has font-lock-type-face"
                (eq (decl-smoke--face-at name) 'font-lock-type-face) (format "%S" (decl-smoke--face-at name))))
  (goto-char (point-min)) (forward-line 3)         ; `    assert: x > 0`
  (indent-according-to-mode)
  (decl-smoke "a member indents by 4 inside the record" (= (current-indentation) 4) (format "%d" (current-indentation)))
  (forward-line 1)                                  ; `}`
  (indent-according-to-mode)
  (decl-smoke "the closer sits back on column 0" (= (current-indentation) 0) (format "%d" (current-indentation)))
  (set-buffer-modified-p nil)

  ;; ---- the language server through eglot: diagnostics through flymake
  (setq eglot-server-programs `(((decl-ts-mode :language-id "decl") . (,lsp))))
  (setq eglot-sync-connect t
        eglot-connect-timeout 30
        eglot-autoshutdown t)
  (let ((server (apply #'eglot--connect (eglot--guess-contact))))
    (decl-smoke "eglot connects to decl-lsp" (and server (eq (eglot-current-server) server)))
    (flymake-mode 1)
    (flymake-start nil t)
    (let ((deadline (+ (float-time) 15)))
      (while (and (null (flymake-diagnostics)) (< (float-time) deadline))
        (accept-process-output nil 0.2)))
    (let ((diags (flymake-diagnostics)))
      (dolist (d diags) (princ (format "emacs:      %s\n" (flymake-diagnostic-text d))))
      (decl-smoke "flymake shows the fixture's diagnostic"
                  (seq-some (lambda (d) (string-match-p "syntax error" (flymake-diagnostic-text d))) diags)
                  (format "%d diagnostics" (length diags))))

    ;; ---- hover on a type name, in the clean example
    (find-file example)
    (let ((deadline (+ (float-time) 5)))
      (while (and (not (eglot-current-server)) (< (float-time) deadline))
        (accept-process-output nil 0.2)))
    (decl-smoke "the example is managed by the same server" (eq (eglot-current-server) server))
    (font-lock-ensure)
    (goto-char (point-min))
    (search-forward "log_level?")
    (decl-smoke "a member's name has font-lock-property-name-face"
                (eq (decl-smoke--face-at (match-beginning 0)) 'font-lock-property-name-face)
                (format "%S" (decl-smoke--face-at (match-beginning 0))))
    (goto-char (point-min))
    (search-forward "type LogLevel")
    (goto-char (- (match-end 0) 8))
    (let* ((result (jsonrpc-request (eglot-current-server) :textDocument/hover
                                    (list :textDocument (eglot--TextDocumentIdentifier)
                                          :position (eglot--pos-to-lsp-position))
                                    :timeout 10))
           (text (decl-smoke--hover-text result)))
      (princ (format "emacs:      hover: %s\n" (replace-regexp-in-string "\n" " | " text)))
      (decl-smoke "hover on LogLevel shows its declaration" (string-match-p "type LogLevel" text)))
    (eglot-shutdown server nil nil t)))

(kill-emacs (if decl-smoke--failed 1 0))
;;; smoke.el ends here
