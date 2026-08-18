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
;; In tree-sitter-kotlin v0.3.5+, variable_declaration only contains
;; simple_identifier + optional type.  The = expr part lives in
;; property_declaration.  Capture the target from the nested
;; variable_declaration and the value expression from property_declaration.
(property_declaration
  (variable_declaration
    (simple_identifier) @df.assign_target)
  (_) @df.assign_value)

;; --- when subject declaration: when (val x = expr) ---
(when_subject
  (variable_declaration
    (simple_identifier) @df.assign_target)
  (_) @df.assign_value)

;; --- Simple assignment: x = expr ---
;; tree-sitter-kotlin exposes no left/right fields and wraps the target in
;; directly_assignable_expression. Capture both leaves; the Kotlin AST walker
;; supplies their Assign edge.
(assignment
  (directly_assignable_expression
    (simple_identifier) @df.assign_target)
  "="
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
;; The first named child is the receiver (may be simple_identifier or another
;; navigation_expression).  The trailing `.` anchor captures only the LAST
;; simple_identifier (the actual field name), avoiding dual captures where
;; both `obj` and `field` would be created as Field nodes.
(navigation_expression
  .
  (_) @df.receiver)

(navigation_expression
  (simple_identifier) @df.field_name
  .)

;; --- Literals ---
(integer_literal) @df.literal
(real_literal) @df.literal
(boolean_literal) @df.literal
(null_literal) @df.literal
(string_literal) @df.literal

;; --- Identifier uses ---
(simple_identifier) @df.identifier_use
