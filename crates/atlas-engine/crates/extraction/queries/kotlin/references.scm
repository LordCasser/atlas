;; Kotlin references query (tree-sitter-kotlin v0.4.0)
;; Captures: function calls, navigation access, type references

;; Direct function calls
(call_expression
  (simple_identifier) @reference.call)

;; Navigation expression calls (e.g. obj.method())
(call_expression
  (navigation_expression
    (simple_identifier) @reference.call))

;; Navigation access (field/property access)
(navigation_expression
  (simple_identifier) @reference.field)

;; Type references
(type_identifier) @reference.type
