;; Cangjie references query
;; Captures: call, field access, type reference, instantiation

;; Function/method calls: postfixExpression + callSuffix
(postfixExpression (fieldAccess (atomicVariable) @reference.call) (callSuffix))
(postfixExpression (atomicVariable) @reference.call (callSuffix))

;; Field access: obj.field
(fieldAccess (atomicVariable) @reference.field)

;; Type reference in declarations (type hints)
(typeAnnotation (scoped_identifier) @reference.type)
