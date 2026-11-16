;; C++ references query
;; Captures: call, type reference, field access

;; Preserve every written callee independently, including calls on return values.
;; The adapter derives ordinary names recursively and retains other expressions
;; without substituting a nested getter's name for the invoked callable.
(call_expression function: (_) @reference.expression_call)

;; Type references
(type_identifier) @reference.type

;; Field access
(field_expression (field_identifier) @reference.field)
