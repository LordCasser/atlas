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

; Note: no catch-all @reference.usage — avoids capturing local variables,
; loop counters, params, and identifiers already matched by specific patterns.
; Simple identifier reads can be added later via a refined query.
