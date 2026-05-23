;; C# references query
;; Captures: method calls, object creation, field access, type references

;; Method invocations
(invocation_expression
  function: (identifier) @reference.call)

;; Member access method calls (e.g. obj.Method())
(invocation_expression
  function: (member_access_expression
    name: (identifier) @reference.call))

;; Object creation (new expressions)
(object_creation_expression
  type: (identifier) @reference.instantiation)

;; Qualified object creation (e.g. new Namespace.Class())
(object_creation_expression
  type: (qualified_name
    name: (identifier) @reference.instantiation))

;; Member access / field access
(member_access_expression
  name: (identifier) @reference.field)

;; Type references (variable types, parameter types, return types, etc.)
(identifier) @reference.type
