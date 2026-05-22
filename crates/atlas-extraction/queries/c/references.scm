;; C references query
;; Captures: call, type reference, field access

;; Function calls
(call_expression (identifier) @reference.call)

;; Type references in declarations
(type_identifier) @reference.type

;; Field access via dot/arrow
(field_expression (field_identifier) @reference.field)
