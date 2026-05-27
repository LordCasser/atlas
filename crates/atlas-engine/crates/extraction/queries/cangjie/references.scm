;; Cangjie references query
;; Captures: call, field access, type reference, instantiation

;; Function/method calls: postfixExpression + callSuffix
;; Method call: obj.method(args) — capture the method name (2nd atomicVariable in fieldAccess)
(postfixExpression (fieldAccess (_) (atomicVariable) @reference.call) (callSuffix))
;; Simple call: func(args)
(postfixExpression (atomicVariable) @reference.call (callSuffix))

;; Field access: obj.field
(fieldAccess (atomicVariable) @reference.field)

;; NOTE: typeAnnotation node type is not yet present in the Cangjie grammar.
;; Type reference capture via scoped_identifier is deferred until the grammar
;; is updated. See: https://gitcode.com/Cangjie-SIG/tree-sitter-cangjie
