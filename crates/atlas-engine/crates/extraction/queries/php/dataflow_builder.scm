;; PHP dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Function/method parameters ---
(parameter
  name: (variable_name) @df.parameter)

;; --- Assignments: $x = expr ---
(assignment_expression
  left: (variable_name) @df.assign_target
  right: (_) @df.assign_value)

;; --- Return statements ---
(return_statement
  (_) @df.return_value)

;; --- Call targets: func(args) ---
(function_call_expression
  function: (name) @df.call_target)

;; --- Method calls: $obj->method(args) ---
(member_call_expression
  name: (name) @df.call_target)

;; --- Call arguments ---
(arguments
  (_) @df.call_arg)

;; --- Field access: $obj->field ---
(member_access_expression
  object: (_) @df.receiver
  name: (name) @df.field_name)

;; --- Literals ---
(encapsed_string) @df.literal
(string) @df.literal
(integer) @df.literal
(float) @df.literal
(boolean) @df.literal
(null) @df.literal
