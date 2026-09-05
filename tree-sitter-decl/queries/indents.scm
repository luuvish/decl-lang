; Indentation (nvim-treesitter's dialect): a bracketed body indents the
; lines inside it; its closer sits back on the opener's column.

[
  (record_type)
  (object)
  (array)
  (paren_expression)
  (paren_type)
  (type_arguments)
  (match_expression)
  (call)
] @indent.begin

[
  "}"
  "]"
  ")"
] @indent.branch

[
  "}"
  "]"
  ")"
] @indent.end
