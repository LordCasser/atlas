;; C++ references query
;; Captures: call, type reference, field access

;; Function/method calls — always capture the *simple* callee name so
;; resolution can use symbols.name / GlobalSymbolIndex.by_name.
;; Qualified calls (CertUtils::GetDev) capture only the last identifier;
;; full text remains available via the parent for diagnostics.
(call_expression (identifier) @reference.call)
(call_expression (field_expression (field_identifier) @reference.call))
(call_expression
  (qualified_identifier
    name: (identifier) @reference.call))
(call_expression
  (qualified_identifier
    name: (field_identifier) @reference.call))
(call_expression
  (qualified_identifier
    name: (type_identifier) @reference.call))

;; Type references
(type_identifier) @reference.type

;; Field access
(field_expression (field_identifier) @reference.field)
