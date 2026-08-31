// Decl grammar — written against docs/specification/11_grammar.md.
// The newline-separator rule (§2.9) is implemented by the external
// scanner: NEWLINE is emitted exactly where the LR state admits it
// ("the token before can end an element and the token after can begin
// one"), which is the spec's rule computed by valid_symbols.

const SEP = ',';

module.exports = grammar({
  name: 'decl',

  externals: $ => [$._newline, $.block_comment],

  // '\n' is also an extra: the external scanner runs first, so where
  // the parse state admits NEWLINE it becomes a separator token, and
  // everywhere else the line break is plain whitespace — §2.9 exactly.
  extras: $ => [/[ \t\r\n]/, $.doc_comment, $.line_comment, $.block_comment],

  word: $ => $.identifier,

  conflicts: $ => [
    [$._member_name, $._primary],
    [$.lambda_parameter, $._primary],
    [$.array_entry, $.array_comprehension],
    [$.object_entry, $.map_comprehension],
    [$.type_declaration],
    [$.assert_member],
    [$.value_member],
  ],

  rules: {
    // ---------------- module ----------------
    module: $ => repeat(choice($._declaration, $._newline)),

    _declaration: $ => choice(
      $.import_declaration,
      $.re_export_declaration,
      seq(optional('export'), $._plain_declaration),
    ),

    _plain_declaration: $ => choice(
      $.type_declaration,
      $.const_declaration,
      $.func_declaration,
      $.output_declaration,
      $.input_declaration,
      $.diagnostic_declaration,
      $.dimension_declaration,
      $.unit_declaration,
    ),

    import_declaration: $ => seq(
      'import',
      choice($.named_imports, seq('*', 'as', $.identifier)),
      'from', $.string,
    ),
    named_imports: $ => seq('{', sepList($, $.import_item), '}'),
    import_item: $ => seq($.identifier, optional(seq('as', $.identifier))),
    re_export_declaration: $ => seq(
      'export', '{', sepList($, $.import_item), '}', 'from', $.string,
    ),

    type_declaration: $ => seq(
      'type', field('name', $.identifier), optional($.type_parameters),
      '=', field('type', $._type), optional($.else_clause),
    ),
    type_parameters: $ => seq('<', commaList($.type_parameter), '>'),
    type_parameter: $ => seq($.identifier, optional(seq(':', $._type))),

    const_declaration: $ => seq(
      'const', field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
      '=', field('value', $._expression),
    ),

    func_declaration: $ => seq(
      'func', field('name', $.identifier),
      '(', optional(commaList($.parameter)), ')',
      optional(seq(':', field('return_type', $._type))),
      '=', field('body', $._expression),
    ),
    parameter: $ => seq($.identifier, ':', $._type),

    output_declaration: $ => seq(
      'output', field('name', $.identifier), ':', field('type', $._type),
      '=', field('value', $._expression),
    ),
    input_declaration: $ => seq(
      'input', field('name', $.identifier), ':', field('type', $._type),
      optional(seq('=', field('fallback', $._expression))),
    ),

    diagnostic_declaration: $ => seq(
      'diagnostic', field('name', $.identifier),
      '(', optional(commaList($.parameter)), ')',
      '{',
      alias('severity', $.severity_key), '=', $.severity, $._sep,
      alias('message', $.message_key), '=', $.template_string,
      optional($._sep),
      '}',
    ),
    severity: _ => choice('error', 'warn', 'info'),

    dimension_declaration: $ => seq(
      'dimension', field('name', $.identifier),
      optional(seq('=', $.dimension_expression)),
    ),
    dimension_expression: $ => seq(
      $.dimension_term, repeat(seq(choice('*', '/'), $.dimension_term)),
    ),
    dimension_term: $ => seq(
      $.identifier, optional(seq('^', optional('-'), $.int)),
    ),

    unit_declaration: $ => seq(
      'unit', field('name', $.identifier),
      choice(
        seq(':', field('dimension', $.identifier)),
        seq('=', field('factor', $._expression), field('base', $.identifier)),
      ),
    ),

    else_clause: $ => seq(
      optional($._newline), 'else',
      choice(
        seq($.severity, $.template_string),
        seq($.qualified_name, optional(seq('(', optional(commaList($._expression)), ')'))),
      ),
    ),
    qualified_name: $ => seq($.identifier, repeat(seq('.', $.identifier))),

    // ---------------- types ----------------
    _type: $ => choice($.union_type, $._non_union_type),
    union_type: $ => prec.left(1, seq(
      $._non_union_type, repeat1(seq('|', $._non_union_type)),
    )),
    _non_union_type: $ => choice($.intersection_type, $._suffix_type),
    intersection_type: $ => prec.left(2, seq(
      $._suffix_type, repeat1(seq('&', $._suffix_type)),
    )),
    _suffix_type: $ => choice($.array_type, $.nullable_type, $._primary_type),
    array_type: $ => prec(3, seq(
      $._suffix_type, token.immediate('['),
      optional(choice(
        $.array_size_range,
        field('size', $._const_expression),
      )),
      ']',
    )),
    array_size_range: $ => seq(
      $._const_expression, choice('..', '..<'), $._const_expression,
    ),
    nullable_type: $ => prec(3, seq($._suffix_type, token.immediate('?'))),

    _primary_type: $ => choice(
      $.range_type,
      $._literal_type,
      $.pattern,
      $.record_type,
      $.map_type,
      $.function_type,
      $.named_type,
      $.paren_type,
    ),
    paren_type: $ => seq('(', $._type, ')'),

    _literal_type: $ => choice(
      $.string, $.number_literal, 'true', 'false', 'null',
    ),
    number_literal: $ => seq(optional('-'), choice($.int, $.float)),

    range_type: $ => prec.left(seq(
      $._range_endpoint, choice('..', '..<'), $._range_endpoint,
    )),
    _range_endpoint: $ => choice($.number_literal, $.qualified_name),

    named_type: $ => prec.right(seq(
      $.qualified_name,
      optional($.type_arguments),
      optional(field('predicates', $.predicate_arguments)),
      optional(field('extension', $.record_type)),
    )),
    type_arguments: $ => seq(
      token.immediate('<'), commaList($._type_argument), '>',
    ),
    _type_argument: $ => $._type,
    predicate_arguments: $ => seq(
      token.immediate('('), commaList($._expression), ')',
    ),

    record_type: $ => seq(
      '{',
      optional(sepList($, choice($._member, $.open_marker))),
      '}',
    ),
    open_marker: _ => '...',
    map_type: $ => seq('{', '[', field('key', $._type), ']', ':', field('value', $._type), '}'),

    _member: $ => choice(
      $.value_member,
      $.derived_member,
      $.context_declaration,
      $.assert_member,
      $.when_member,
    ),
    value_member: $ => seq(
      field('name', $._member_name), optional(field('optional', '?')),
      ':', field('type', $._type),
      optional(seq(optional($._newline), '=', field('default', $._expression))),
    ),
    derived_member: $ => seq(
      'const', field('name', $._member_name),
      optional(seq(':', field('type', $._type))),
      '=', field('value', $._expression),
    ),
    context_declaration: $ => seq(
      field('variable', $.context_variable), ':', field('type', $._type),
    ),
    assert_member: $ => seq(
      'assert', field('name', $.identifier), ':',
      field('condition', $._expression),
      optional($.else_clause),
    ),
    when_member: $ => seq(
      'when', field('condition', $._expression),
      '{', optional(sepList($, choice($.assert_member, $.when_member))), '}',
    ),
    _member_name: $ => choice($.identifier, $.string),

    function_type: $ => seq(
      '(', optional(commaList($._type)), ')', '=>', $._type,
    ),

    // ---------------- expressions ----------------
    _expression: $ => choice(
      $.lambda,
      $.if_expression,
      $.match_expression,
      $._pipe_expression,
    ),

    lambda: $ => prec.right(seq(
      '(', optional(commaList($.lambda_parameter)), ')',
      '=>', field('body', $._expression),
    )),
    lambda_parameter: $ => seq($.identifier, optional(seq(':', $._type))),

    if_expression: $ => prec.right(seq(
      'if', field('condition', $._expression),
      'then', field('then', $._expression),
      'else', field('else', $._expression),
    )),

    match_expression: $ => seq(
      'match', field('subject', $._expression),
      '{', sepList($, $.match_arm), '}',
    ),
    match_arm: $ => seq(
      '(', $.identifier, optional(seq(':', $._type)), ')',
      '=>', field('body', $._expression),
    ),

    _pipe_expression: $ => choice($.pipe_expression, $._nullish),
    pipe_expression: $ => prec.left(1, seq(
      $._pipe_expression, '|>', $._nullish,
    )),
    _nullish: $ => choice($.nullish_expression, $._logical_or),
    nullish_expression: $ => prec.left(2, seq($._nullish, '??', $._logical_or)),
    _logical_or: $ => choice($.binary_expression_or, $._logical_and),
    binary_expression_or: $ => prec.left(3, seq($._logical_or, '||', $._logical_and)),
    _logical_and: $ => choice($.binary_expression_and, $._bit_or),
    binary_expression_and: $ => prec.left(4, seq($._logical_and, '&&', $._bit_or)),
    _bit_or: $ => choice($.bit_or_expression, $._bit_xor),
    bit_or_expression: $ => prec.left(5, seq($._bit_or, '|', $._bit_xor)),
    _bit_xor: $ => choice($.bit_xor_expression, $._bit_and),
    bit_xor_expression: $ => prec.left(6, seq($._bit_xor, '^', $._bit_and)),
    _bit_and: $ => choice($.bit_and_expression, $._equality),
    bit_and_expression: $ => prec.left(7, seq($._bit_and, '&', $._equality)),
    _equality: $ => choice($.equality_expression, $._relational),
    equality_expression: $ => prec.left(8, seq(
      $._relational, choice('==', '!='), $._relational,
    )),
    _relational: $ => choice($.relational_expression, $.matches_expression, $._range_expr),
    relational_expression: $ => prec.left(9, seq(
      $._range_expr, choice('<', '<=', '>', '>=', 'in'), $._range_expr,
    )),
    matches_expression: $ => prec.left(9, seq($._range_expr, 'matches', $.pattern)),
    _range_expr: $ => choice($.range_expression, $._shift),
    range_expression: $ => prec.left(10, seq($._shift, choice('..', '..<'), $._shift)),
    _shift: $ => choice($.shift_expression, $._additive),
    shift_expression: $ => prec.left(11, seq($._shift, choice('<<', '>>'), $._additive)),
    _additive: $ => choice($.additive_expression, $._multiplicative),
    additive_expression: $ => prec.left(12, seq(
      $._additive, choice('+', '-'), $._multiplicative,
    )),
    _multiplicative: $ => choice($.multiplicative_expression, $._unary),
    multiplicative_expression: $ => prec.left(13, seq(
      $._multiplicative, choice('*', '/', '%'), $._unary,
    )),
    _unary: $ => choice($.unary_expression, $._with_expr),
    unary_expression: $ => prec(14, seq(choice('!', '-', '~'), $._unary)),
    _with_expr: $ => choice($.with_expression, $._postfix),
    with_expression: $ => prec.left(15, seq(
      $._with_expr, 'with', $.object,
    )),

    _postfix: $ => choice(
      $.member_access, $.safe_access, $.index_access, $.call, $._primary,
    ),
    member_access: $ => prec.left(16, seq(
      $._postfix, token.immediate('.'), $._member_name,
    )),
    safe_access: $ => prec.left(16, seq($._postfix, '?.', $._member_name)),
    index_access: $ => prec.left(16, seq(
      $._postfix, token.immediate('['), $._expression, ']',
    )),
    call: $ => prec.left(16, seq(
      $._postfix, token.immediate('('), optional(commaList($._expression)), ')',
    )),

    _primary: $ => choice(
      $.int, $.float, $.unit_literal, $.string, $.template_string,
      'true', 'false', 'null',
      $.identifier,
      $.context_variable,
      $.referrers_expression,
      $.object,
      $.array,
      $.paren_expression,
    ),
    paren_expression: $ => seq('(', $._expression, ')'),

    referrers_expression: $ => seq(
      '$referrers', '(', field('type', $.identifier), ',', field('member', $.string), ')',
    ),
    context_variable: _ => token(choice('$this', '$parent', '$root', '$key', '$path')),

    object: $ => choice(
      seq('{', optional(sepList($, $.object_entry)), '}'),
      $.map_comprehension,
    ),
    object_entry: $ => choice(
      seq(field('key', $._member_name), ':', field('value', $._expression)),
      seq('...', $._expression),
    ),
    map_comprehension: $ => seq(
      '{', field('key', $._expression), ':', field('value', $._expression),
      repeat1(seq(optional($._newline), $.for_clause)),
      optional($._newline), '}',
    ),

    array: $ => choice(
      seq('[', optional(sepList($, $.array_entry)), ']'),
      $.array_comprehension,
    ),
    array_entry: $ => choice($._expression, seq('...', $._expression)),
    array_comprehension: $ => seq(
      '[', field('head', $._expression),
      repeat1(seq(optional($._newline), $.for_clause)),
      optional($._newline), ']',
    ),
    for_clause: $ => seq(
      'for', field('variable', $.identifier), 'in', field('iterable', $._expression),
      repeat(seq('if', field('filter', $._expression))),
    ),

    // ---------------- const expressions ----------------
    // shares the expression grammar; the §4.13 restriction is static
    _const_expression: $ => $._expression,

    // ---------------- separators ----------------
    _sep: $ => choice(SEP, $._newline),

    // ---------------- tokens ----------------
    identifier: _ => /[_A-Za-z][_A-Za-z0-9]*/,
    int: _ => token(choice(
      prec(2, /0[xX][0-9a-fA-F][0-9a-fA-F_]*/),
      prec(2, /0[oO][0-7][0-7_]*/),
      prec(2, /0[bB][01][01_]*/),
      /0|[1-9][0-9_]*/,
    )),
    float: _ => token(choice(
      /(0|[1-9][0-9_]*)\.[0-9][0-9_]*([eE][+-]?[0-9]+)?/,
      /(0|[1-9][0-9_]*)[eE][+-]?[0-9]+/,
    )),
    // unit symbols are excluded from the overlap with numeric tokens by
    // construction (no lookahead in tree-sitter regexes): a unit may not
    // look like a float exponent (e3) and, after a bare 0, may not look
    // like a radix prefix (o755, xFF, b101) -- those lex as numbers; a
    // longer genuine unit (250ms, 0s, 1.5e3s, eV) still matches whole
    unit_literal: _ => token(choice(
      seq(/(0|[1-9][0-9_]*)\.[0-9][0-9_]*([eE][+-]?[0-9]+)?/, choice(
        /[A-DF-Za-df-z][A-Za-z0-9]*/,
        /[eE][A-Za-z][A-Za-z0-9]*/,
        /[eE]/,
      )),
      seq(/[1-9][0-9_]*/, choice(
        /[A-DF-Za-df-z][A-Za-z0-9]*/,
        /[eE][A-Za-z][A-Za-z0-9]*/,
        /[eE]/,
      )),
      seq('0', choice(
        /[ACDF-NP-WYZacdf-np-wyz][A-Za-z0-9]*/,
        /[eEoObBxX][A-Za-z][A-Za-z0-9]*/,
        /[eEoObBxX]/,
      )),
    )),
    string: _ => token(seq('"', repeat(choice(/[^"\\\n]/, /\\./)), '"')),
    pattern: _ => token(seq('/', repeat1(choice(/[^\/\\\n]/, /\\./)), '/')),

    template_string: $ => seq(
      '`',
      repeat(choice(
        $.template_chars,
        $.template_escape,
        $.interpolation,
      )),
      '`',
    ),
    template_chars: _ => token.immediate(prec(1, /[^`$\\]+/)),
    template_escape: _ => token.immediate(/\\./),
    interpolation: $ => seq(token.immediate('${'), $._expression, '}'),

    doc_comment: _ => token(prec(2, /\/\/\/[^\n]*/)),
    line_comment: _ => token(prec(1, /\/\/[^\n]*/)),
    // block_comment (nesting) comes from the external scanner
  },
});

function commaList(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

function sepList($, rule) {
  return seq(
    rule,
    repeat(seq($._sep, rule)),
    optional($._sep),
  );
}
