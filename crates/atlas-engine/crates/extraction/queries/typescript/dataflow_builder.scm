;; Dataflow builder captures: parameters, assignments, returns, call targets,
;; call args, member access, literals
;;
;; These captures feed into DataFlowBuilder which creates DataNodes and DataFlowEdges.
;; Unlike the old dataflow.scm (which produces SymbolId-based RawEdges),
;; this produces DataNode-based DataFlowEdges.

;; --- Function parameters ---
;; Required parameter: function f(x: T)
(required_parameter
  (identifier) @df.parameter)

;; Optional parameter: function f(x?: T)
(optional_parameter
  (identifier) @df.parameter)

;; --- Assignments: target = value ---
;; Captures both left-hand side and right-hand side of assignments
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; Variable declarations with initializer: const name = expr
(variable_declarator
  name: (identifier) @df.assign_target
  value: (_) @df.assign_value)

;; --- Return statements ---
;; return expr
(return_statement (_) @df.return_value)

;; --- Call targets ---
;; Direct call: func(args) — captures the function being called
(call_expression
  function: (identifier) @df.call_target)

;; Method call: obj.method(args) — captures the method name
(call_expression
  function: (member_expression
    property: (property_identifier) @df.call_target))

;; --- Call arguments ---
;; Each argument expression at a call site
(arguments (_) @df.call_arg)

;; --- Member access chains ---
;; obj.field → captured for field load dataflow
(member_expression
  object: (_) @df.receiver
  property: (property_identifier) @df.field_name)

;; --- Literals ---
(string) @df.literal
(number) @df.literal
(true) @df.literal
(false) @df.literal
(null) @df.literal
(undefined) @df.literal

;; --- Await expressions (async dataflow) ---
(await_expression (_) @df.await_value)

;; --- Identifier uses (variable references) ---
;; Captured broadly; normalize filters out declarations/properties/types/callee-targets.
(identifier) @df.identifier_use

;; --- Destructuring binding ---
(object_pattern
  (shorthand_property_identifier_pattern) @df.assign_target)
(pair_pattern
  key: (_)
  value: (identifier) @df.assign_target)
(array_pattern
  (identifier) @df.assign_target)

;; --- Property assignment (obj.field = value) ---
(assignment_expression
  left: (member_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; --- Subscript assignment (arr[i] = value) ---
(assignment_expression
  left: (subscript_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; --- new expression ---
(new_expression
  constructor: (_) @df.call_target
  arguments: (arguments (_) @df.call_arg))
