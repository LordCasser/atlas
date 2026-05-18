;; C++ references query
;; Captures: call, type reference, field access

;; Function/method calls — simple call or qualified call
(call_expression (identifier) @reference.call)
(call_expression (field_expression (field_identifier) @reference.call))
(call_expression (qualified_identifier) @reference.call)

;; Type references
(type_identifier) @reference.type

;; Field access
(field_expression (field_identifier) @reference.field)
