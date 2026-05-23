;; PHP references query
;; Captures: function calls, method calls, static calls, object creation,
;;           field access, type references

;; Function calls
(function_call_expression
  function: (name) @reference.call)

;; Qualified function calls (e.g. \strlen)
(function_call_expression
  function: (qualified_name) @reference.call)

;; Method calls (->)
(member_call_expression
  name: (name) @reference.call)

;; Static method calls (::)
(scoped_call_expression
  name: (name) @reference.call)

;; Object creation (new)
(object_creation_expression
  (qualified_name) @reference.instantiation)
(object_creation_expression
  (name) @reference.instantiation)

;; Member access (->property)
(member_access_expression
  name: (name) @reference.field)

;; Type references
(named_type (name) @reference.type)
(named_type (qualified_name) @reference.type)
