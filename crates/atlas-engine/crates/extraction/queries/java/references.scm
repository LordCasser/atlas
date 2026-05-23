;; Java references query
;; Captures: method calls, field access, type references

;; Method calls
(method_invocation (identifier) @reference.call)

;; Field access
(field_access (identifier) @reference.field)

;; Object instantiation
(object_creation_expression (type_identifier) @reference.instantiation)

;; Type references (extends, implements, variable types, return types)
(type_identifier) @reference.type
