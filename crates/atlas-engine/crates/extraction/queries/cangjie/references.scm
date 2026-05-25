;; Cangjie references query
;; Captures: call, field access, type reference, instantiation

;; Function/method calls: postfixExpression + callSuffix
(postfixExpression (fieldAccess (atomicVariable) @reference.call) (callSuffix))
(postfixExpression (atomicVariable) @reference.call (callSuffix))

;; Field access: obj.field
(fieldAccess (atomicVariable) @reference.field)

;; NOTE: typeAnnotation node type is not yet present in the Cangjie grammar.
;; Type reference capture via scoped_identifier is deferred until the grammar
;; is updated. See: https://gitcode.com/Cangjie-SIG/tree-sitter-cangjie
