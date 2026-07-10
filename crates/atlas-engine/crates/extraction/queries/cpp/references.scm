;; C++ references query
;; Captures: call, type reference, field access

;; Function/method calls — always capture the *simple* callee name so
;; resolution can use symbols.name / GlobalSymbolIndex.by_name.
;; Nested qualified_identifier (A::B::C) needs recursive `name:` patterns;
;; the normalizer walks to the outermost QI for full text / receiver.
(call_expression (identifier) @reference.call)
(call_expression (field_expression (field_identifier) @reference.call))
;; 1-level: CertUtils::GetDev
(call_expression
  (qualified_identifier
    name: [
      (identifier) @reference.call
      (field_identifier) @reference.call
      (type_identifier) @reference.call
    ]))
;; 2-level: A::B::method
(call_expression
  (qualified_identifier
    name: (qualified_identifier
      name: [
        (identifier) @reference.call
        (field_identifier) @reference.call
        (type_identifier) @reference.call
      ])))
;; 3-level: A::B::C::method (rare; deeper nests need re-index + query extend)
(call_expression
  (qualified_identifier
    name: (qualified_identifier
      name: (qualified_identifier
        name: [
          (identifier) @reference.call
          (field_identifier) @reference.call
          (type_identifier) @reference.call
        ]))))

;; Type references
(type_identifier) @reference.type

;; Field access
(field_expression (field_identifier) @reference.field)
