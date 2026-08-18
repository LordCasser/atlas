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

;; --- For-in bindings: iterable aggregate -> pattern variables ---
(forInExpression
  . (_)
  . (_) @df.for_iterable)

;; Broad grammar capture filtered by the adapter to keep only binding nodes in
;; the first forInExpression child and reject enum constructor names.
(varBindingPattern) @df.for_target

;; --- Match selector and pattern binding targets ---
;; The anchors select only `match (selector)`, not conditionless match bodies.
(matchExpression
  . (_) @df.match_subject
  . (matchCase))

;; Broad grammar capture filtered by the adapter, including bindings nested in
;; tuple, enum, and type patterns while excluding enum constructor syntax.
(varBindingPattern) @df.pattern_target

;; --- Return statements ---
(jumpExpression) @df.return_value

;; --- Call targets: func(args) ---
(postfixExpression
  (atomicVariable
    (varBindingPattern) @df.call_target)
  (callSuffix))

;; --- Method call targets: obj.method(args) ---
;; postfixExpression(fieldAccess(receiver, methodName), callSuffix)
(postfixExpression
  (fieldAccess
    (_) @df.receiver
    (atomicVariable
      (varBindingPattern) @df.call_target))
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
