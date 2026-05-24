;; Kotlin dataflow builder captures (conservative, verified node types)
;; tree-sitter-kotlin v0.4.0+ compatible

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

;; --- Variable declarations: val x = expr ---
(variable_declaration
  (simple_identifier) @df.assign_target
  (_) @df.assign_value)

;; --- Return value ---
(jump_expression
  (_) @df.return_value)

;; --- Simple function calls: func(args) ---
(call_expression
  (simple_identifier) @df.call_target)

;; --- Call arguments ---
(value_arguments
  (value_argument
    (_) @df.call_arg))

;; --- Field access: obj.field ---
(navigation_expression
  (simple_identifier) @df.field_name)

;; --- Literals ---
(line_string_literal) @df.literal
(integer_literal) @df.literal
(real_literal) @df.literal
(boolean_literal) @df.literal
(null_literal) @df.literal

;; --- Identifier uses ---
(simple_identifier) @df.identifier_use
