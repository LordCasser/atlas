;; Dataflow builder captures: assignments, returns, call args, member access, literals
;;
;; These captures feed into DataFlowBuilder which creates DataNodes and DataFlowEdges.
;; Unlike the old dataflow.scm (which produces SymbolId-based RawEdges),
;; this produces DataNode-based DataFlowEdges.

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
