;; Java dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access
;;
;; These captures feed into DataFlowBuilder which creates DataNodes and DataFlowEdges.

;; --- Method/constructor parameters ---
(formal_parameter
  name: (identifier) @df.parameter)

;; --- Assignments: target = value ---
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Field assignment: obj.field = value ---
(assignment_expression
  left: (field_access) @df.assign_field_target
  right: (_) @df.assign_value)

;; --- Local variable declarations with initializer: int x = expr ---
(local_variable_declaration
  declarator: (variable_declarator
    name: (identifier) @df.assign_target
    value: (_) @df.assign_value))

;; --- Return statements ---
(return_statement
  (_) @df.return_value)

;; --- Call targets: method(args) ---
(method_invocation
  name: (identifier) @df.call_target)

;; --- Object creation: new Foo(args) ---
(object_creation_expression
  type: (_) @df.call_target
  arguments: (argument_list (_) @df.call_arg))

;; --- Call arguments ---
(argument_list
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(field_access
  object: (_) @df.receiver
  field: (identifier) @df.field_name)

;; --- Literals ---
(string_literal) @df.literal
(decimal_integer_literal) @df.literal
(hex_integer_literal) @df.literal
(decimal_floating_point_literal) @df.literal
(true) @df.literal
(false) @df.literal
(null_literal) @df.literal

;; --- Array access: arr[i] ---
(array_access
  array: (_) @df.receiver
  index: (_) @df.index)

;; --- Enhanced for: for (Type name : value) ---
(enhanced_for_statement
  name: (identifier) @df.assign_target
  value: (_) @df.assign_value)

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use
