;; Kotlin dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Function parameters ---
(function_declaration
  (function_value_parameters
    (parameter
      (simple_identifier) @df.parameter)))

;; --- Lambda parameters ---
(lambda_literal
  (lambda_parameters
    (variable_declaration
      (simple_identifier) @df.parameter)))

;; --- Assignments: target = value ---
(assignment
  left: (simple_identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Variable declarations with initializer: val x = expr ---
(property_declaration
  (variable_declaration
    (simple_identifier) @df.assign_target
    (expression) @df.assign_value))
(variable_declaration
  (simple_identifier) @df.assign_target
  (expression) @df.assign_value)

;; --- Return value from jump_expression ---
(return_expression
  (_) @df.return_value)

;; --- Call targets: func(args) ---
(call_expression
  (simple_identifier) @df.call_target)

;; --- Method calls: obj.method(args) ---
(call_expression
  (navigation_expression
    name: (simple_identifier) @df.call_target))

;; --- Call arguments ---
(call_expression
  (value_arguments
    (value_argument
      (_) @df.call_arg)))

;; --- Field access: obj.field ---
(navigation_expression
  expression: (_) @df.receiver
  name: (simple_identifier) @df.field_name)

;; --- Literals ---
(line_string_literal) @df.literal
(multi_line_string_literal) @df.literal
(integer_literal) @df.literal
(real_literal) @df.literal
(boolean_literal) @df.literal
(null_literal) @df.literal
(character_literal) @df.literal
