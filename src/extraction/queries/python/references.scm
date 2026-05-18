; Python references query
; Captures reference uses: calls, attribute access, decorators

(call
  function: (identifier) @reference.call)

(call
  function: (attribute
    attribute: (identifier) @reference.call))

; Attribute access (field/method access)
(attribute
  attribute: (identifier) @reference.field)

; Decorator references
(decorator
  (identifier) @reference.decorator)

(decorator
  (attribute
    attribute: (identifier) @reference.decorator))
