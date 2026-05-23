;; PHP dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access, array access, superglobals

;; --- Function/method parameters ---
(simple_parameter
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

;; --- Array access: $arr[$key] ---
;; subscript_expression has no named fields; use positional anchors
(subscript_expression
  .
  (variable_name) @df.receiver
  .
  (_) @df.index)

;; --- Array assignment: $arr[$key] = value ---
(assignment_expression
  left: (subscript_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; --- Superglobal access ($_GET, $_POST, etc.) ---
(variable_name) @df.superglobal
(#match? @df.superglobal "^\\$_")
