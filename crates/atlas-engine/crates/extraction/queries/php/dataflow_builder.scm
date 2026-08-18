;; PHP dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access, array access, superglobals

;; --- Function/method parameters ---
(simple_parameter
  name: (variable_name) @df.parameter)

;; --- Assignments: $x = expr ---
(assignment_expression
  left: (variable_name) @df.assign_target
  right: (_) @df.assign_value)

;; --- Direct-variable read-modify-write expressions ---
;; Capture the whole expression as the produced value so containment edges
;; retain both the previous target value and an augmented-assignment RHS.
(augmented_assignment_expression
  left: (variable_name) @df.mutation_target) @df.mutation_value

(augmented_assignment_expression
  left: (variable_name) @df.mutation_read)

(update_expression
  argument: (variable_name) @df.mutation_target) @df.mutation_value

(update_expression
  argument: (variable_name) @df.mutation_read)

;; --- Array destructuring ([] and list()) ---
(list_literal
  (variable_name) @df.destructure_target)

(list_literal
  (by_ref
    (variable_name) @df.destructure_target))

(assignment_expression
  left: (list_literal)
  right: (_) @df.destructure_value)

;; --- Foreach collection and direct key/value targets ---
;; The collection is the named child before the literal `as` token. Nested
;; []/list() targets are already captured by df.destructure_target.
(foreach_statement
  (_) @df.foreach_value
  "as")

(foreach_statement
  "as"
  (variable_name) @df.foreach_target)

(foreach_statement
  "as"
  (by_ref
    (variable_name) @df.foreach_target))

(foreach_statement
  "as"
  (pair
    (variable_name) @df.foreach_target))

(foreach_statement
  "as"
  (pair
    (by_ref
      (variable_name) @df.foreach_target)))

;; --- Return statements ---
(return_statement
  (_) @df.return_value)

;; --- Call targets: func(args) ---
(function_call_expression
  function: (name) @df.call_target)

;; --- Method calls: $obj->method(args) ---
(member_call_expression
  name: (name) @df.call_target)

;; --- Dynamic method calls: $obj->$method(args) ---
(member_call_expression
  name: (variable_name) @df.call_target)

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
  (_) @df.field_name)

;; --- Array assignment: $arr[$key] = value ---
(assignment_expression
  left: (subscript_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; --- Superglobal access ($_GET, $_POST, etc.) ---
(variable_name) @df.superglobal
(#match? @df.superglobal "^\\$_")

;; --- Variable uses (reading a variable value) ---
;; Captures all variable_name nodes not already handled by specific patterns
;; above (parameter, assign_target, superglobal). The normalize function filters
;; out declaration contexts (left side of =, parameter declarations, etc.).
(variable_name) @df.identifier_use
