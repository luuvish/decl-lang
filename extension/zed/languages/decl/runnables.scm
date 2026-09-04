; run buttons in the gutter (docs/tooling/04_extension.md §14): an output
; evaluates (`decl evaluate --output <name>`), a fixture's @expect-* header
; judges its corpus (`decl validate <dir>`); the tags bind to the task
; templates the README gives for .zed/tasks.json
(
  (output_declaration name: (identifier) @run @decl_output)
  (#set! tag decl-evaluate)
)
(
  (line_comment) @run
  (#match? @run "^// @expect-")
  (#set! tag decl-fixture)
)
