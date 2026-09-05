; Text objects (the captures Helix and nvim-treesitter-textobjects share):
; a function and its body, a type and its members, a parameter, an entry
; (a member or an object entry), a call, a conditional, a comment.

(func_declaration
  body: (_) @function.inside) @function.around

(type_declaration
  type: (record_type) @class.inside) @class.around
(type_declaration) @class.around

(parameter) @parameter.inside @parameter.around
(lambda_parameter) @parameter.inside @parameter.around

(value_member) @entry.inside @entry.around
(derived_member) @entry.inside @entry.around
(hidden_member) @entry.inside @entry.around
(assert_member) @entry.inside @entry.around
(object_entry) @entry.inside @entry.around

(call) @call.around
(if_expression) @conditional.around
(match_expression) @conditional.around
(match_arm) @entry.around

[
  (line_comment)
  (block_comment)
  (doc_comment)
] @comment.inside @comment.around
