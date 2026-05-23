;; Rust references query
;; Captures: function calls, macro invocations, field access, type references

;; Function calls
(call_expression
  function: (identifier) @reference.call)

;; Scoped function calls (e.g. std::io::stdout)
(call_expression
  function: (scoped_identifier
    name: (identifier) @reference.call))

;; Method calls (field_expression)
(call_expression
  function: (field_expression
    field: (field_identifier) @reference.call))

;; Macro invocations
(macro_invocation
  macro: (identifier) @reference.call)

;; Scoped macro invocations
(macro_invocation
  macro: (scoped_identifier
    name: (identifier) @reference.call))

;; Field access (non-call field expressions)
(field_expression
  field: (field_identifier) @reference.field)

;; Type references
(type_identifier) @reference.type
