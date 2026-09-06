; Decl syntax highlighting

[
  "type" "const" "func" "output" "input" "export" "import" "from" "as"
  "dimension" "unit" "diagnostic" "assert" "when"
  "if" "then" "else" "match" "for" "in" "matches" "with"
] @keyword

(severity) @keyword.modifier

[ "true" "false" "null" ] @constant.builtin

(int) @number
(float) @number
(unit_literal) @number
(string) @string
(template_string) @string
(template_chars) @string
(pattern) @string.regexp
(interpolation "${" @punctuation.special "}" @punctuation.special)

(doc_comment) @comment.documentation
(line_comment) @comment
(block_comment) @comment

(context_variable) @variable.builtin
"$referrers" @function.builtin

(annotation "@" @punctuation.special name: (identifier) @attribute)

(type_declaration name: (identifier) @type)
(named_type (qualified_name (identifier) @type .))
(type_parameter (identifier) @type)

(func_declaration name: (identifier) @function)
(call (member_access (identifier) @function .))
(call (identifier) @function)

(value_member name: (identifier) @property)
(derived_member name: (identifier) @property)
(object_entry key: (identifier) @property)
(member_access (identifier) @property .)
(assert_member name: (identifier) @label)
(diagnostic_declaration name: (identifier) @label)

(parameter (identifier) @variable.parameter)
(lambda_parameter (identifier) @variable.parameter)
(for_clause variable: (identifier) @variable.parameter)

[
  "==" "!=" "<=" ">=" "&&" "||" "??" "|>" "=>" "<<" ">>"
  "+" "-" "*" "/" "%" "!" "~" "^" "&" "|" ".." "..<" "?."
] @operator

[ "{" "}" "[" "]" "(" ")" ] @punctuation.bracket
[ "," ":" "=" "." "?" ] @punctuation.delimiter
"..." @punctuation.special
