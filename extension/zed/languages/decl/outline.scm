; the outline panel, breadcrumbs, and symbol search: every declaration,
; and the members of a record type
(type_declaration "type" @context name: (identifier) @name) @item
(const_declaration "const" @context name: (identifier) @name) @item
(func_declaration "func" @context name: (identifier) @name) @item
(output_declaration "output" @context name: (identifier) @name) @item
(input_declaration "input" @context name: (identifier) @name) @item
(diagnostic_declaration "diagnostic" @context name: (identifier) @name) @item
(dimension_declaration "dimension" @context name: (identifier) @name) @item
(unit_declaration "unit" @context name: (identifier) @name) @item
(value_member name: (identifier) @name) @item
(derived_member name: (identifier) @name) @item
(assert_member "assert" @context name: (identifier) @name) @item
