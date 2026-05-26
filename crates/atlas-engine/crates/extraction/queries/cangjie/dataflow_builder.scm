;; Cangjie dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access, literals, identifier uses
;;
;; These captures feed into DataFlowBuilder which creates DataNodes and DataFlowEdges.

;; --- Function parameters ---
(parameter
  paraName: (identifier) @df.parameter)

;; --- Variable declarations with initializer: let x = expr ---
(variableDeclaration
  (variableName
    (varBindingPattern) @df.assign_target)
  initilizer: (_) @df.assign_value)

;; --- Return statements ---
(jumpExpression) @df.return_value

;; --- Call targets: func(args) ---
(postfixExpression
  (atomicVariable
    (varBindingPattern) @df.call_target)
  (callSuffix))

;; --- Call arguments ---
(callSuffix
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(fieldAccess
  (atomicVariable) @df.receiver)

;; --- Literals ---
(stringLiteral) @df.literal
(integerLiteral) @df.literal

;; --- Identifier uses (variable references) ---
(atomicVariable
  (varBindingPattern) @df.identifier_use)
