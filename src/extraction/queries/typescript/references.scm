; TypeScript/JavaScript references query
; Captures reference uses: calls, field access, type references, instantiations

(call_expression
  function: (identifier) @reference.call)

(call_expression
  function: (member_expression
    property: (property_identifier) @reference.call))

(new_expression
  constructor: (identifier) @reference.instantiation)

(new_expression
  constructor: (member_expression
    property: (property_identifier) @reference.instantiation))

; Type annotations in variable/parameter declarations
(type_identifier) @reference.type

; Field/member access (not calls)
(member_expression
  property: (property_identifier) @reference.field)

; Simple identifier references (catch-all for variable uses)
(identifier) @reference.usage
