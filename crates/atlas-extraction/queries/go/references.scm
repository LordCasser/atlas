;; Go references query
;; Captures: function calls, field access, type references

;; Function calls
(call_expression
  function: (identifier) @reference.call)

;; Selector expression calls (e.g. pkg.Func, obj.Method)
(call_expression
  function: (selector_expression
    field: (field_identifier) @reference.call))

;; Field access (non-call selector expressions)
(selector_expression
  field: (field_identifier) @reference.field)

;; Type references
(type_identifier) @reference.type
