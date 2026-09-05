" Vim syntax for Decl: the tokens of docs/specification/02_lexical.md,
" the groups of tree-sitter-decl/queries/highlights.scm (a regex
" approximation: a capitalized identifier is a type, an identifier
" before `:` a member, before `(` a function).
if exists('b:current_syntax') | finish | endif

syn case match
syn iskeyword @,48-57,_,$

" operators and punctuation (defined first: a later `//` comment wins)
syn match   declOperator     "[-+*/%!~^&|<>=?]\+"
syn match   declOperator     "\.\.<\?"
syn match   declOperator     "?\."
syn match   declPunctuation  "[{}\[\]();,:]"
syn match   declSpread       "\.\.\."

" numbers and unit literals (250ms, 1.5s, 3e9bps)
syn match   declNumber       "\<\d\+\%(\.\d\+\)\?\%([eE][+-]\?\d\+\)\?\%(\h\w*\)\?\>"

" names
syn match   declFunction     "\<\h\w*\ze\s*("
syn match   declProperty     "\<\h\w*\ze?\?\s*:"
syn match   declType         "\<[A-Z]\w*\>"
syn match   declTypeName     "\<type\s\+\zs\h\w*"
syn match   declContextVar   "\$\h\w*"
syn match   declBuiltin      "\$referrers\>"

" keywords
syn keyword declKeyword      type const func output input export import from as dimension unit diagnostic
syn keyword declKeyword      assert when if then else match for in matches with
syn keyword declBoolean      true false
syn keyword declNull         null

" strings, template strings, `${...}` interpolation, patterns
syn match   declEscape       "\\." contained
syn region  declInterpolation matchgroup=declInterpolationDelim start="\${" end="}" contained
      \ contains=declString,declNumber,declOperator,declFunction,declContextVar,declBuiltin,declBoolean,declNull
syn region  declString       start=+"+ skip=+\\"+ end=+"+ contains=declEscape,declInterpolation
syn region  declTemplate     start=+`+ skip=+\\`+ end=+`+ contains=declEscape,declInterpolation
syn match   declPattern      "/\%(\\/\|[^/ *\n]\)\+/"

" comments (last: they win over the `/` operators)
syn keyword declTodo         TODO FIXME XXX contained
syn match   declLineComment  "//.*$" contains=declTodo,@Spell
syn match   declDocComment   "///.*$" contains=declTodo,@Spell
syn region  declBlockComment start="/\*" end="\*/" contains=declTodo,@Spell

hi def link declLineComment    Comment
hi def link declBlockComment   Comment
hi def link declDocComment     SpecialComment
hi def link declTodo           Todo
hi def link declKeyword        Keyword
hi def link declBoolean        Boolean
hi def link declNull           Constant
hi def link declNumber         Number
hi def link declString         String
hi def link declTemplate       String
hi def link declEscape         SpecialChar
hi def link declInterpolationDelim Special
hi def link declPattern        Special
hi def link declType           Type
hi def link declTypeName       Type
hi def link declProperty       Identifier
hi def link declFunction       Function
hi def link declContextVar     Identifier
hi def link declBuiltin        Function
hi def link declOperator       Operator
hi def link declPunctuation    Delimiter
hi def link declSpread         Special

let b:current_syntax = 'decl'
