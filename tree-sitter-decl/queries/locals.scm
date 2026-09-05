; Scopes, definitions, and references (tree-sitter locals): the module,
; a function body, a lambda, a comprehension, and a record type each
; scope the names declared in them.

[
  (module)
  (func_declaration)
  (lambda)
  (array_comprehension)
  (map_comprehension)
  (record_type)
] @local.scope

(type_declaration name: (identifier) @local.definition)
(const_declaration name: (identifier) @local.definition)
(func_declaration name: (identifier) @local.definition)
(output_declaration name: (identifier) @local.definition)
(input_declaration name: (identifier) @local.definition)
(diagnostic_declaration name: (identifier) @local.definition)
(dimension_declaration name: (identifier) @local.definition)
(unit_declaration name: (identifier) @local.definition)
(parameter (identifier) @local.definition)
(lambda_parameter (identifier) @local.definition)
(for_clause variable: (identifier) @local.definition)
(value_member name: (identifier) @local.definition)
(derived_member name: (identifier) @local.definition)

(identifier) @local.reference
