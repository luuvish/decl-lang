; a block indents its body and outdents at its closer
(record_type "{" @start "}" @end) @indent
(object "{" @start "}" @end) @indent
(array "[" @start "]" @end) @indent
(_ "(" @start ")" @end) @indent
