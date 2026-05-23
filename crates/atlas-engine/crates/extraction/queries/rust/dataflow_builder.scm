;; Rust dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Function parameters ---
(parameters
  (parameter
    pattern: (identifier) @df.parameter))
(self_parameter
  (self) @df.parameter)

;; --- Let bindings ---
(let_declaration
  pattern: (identifier) @df.assign_target
  value: (_) @df.assign_value)

;; --- Assignments: x = expr ---
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Return expressions ---
(return_expression
  (_) @df.return_value)

;; --- Call targets: func(args) ---
(call_expression
  function: (identifier) @df.call_target)

;; --- Method calls: obj.method(args) ---
(call_expression
  function: (field_expression
    field: (field_identifier) @df.call_target))

;; --- Call arguments ---
(arguments
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(field_expression
  value: (_) @df.receiver
  field: (field_identifier) @df.field_name)

;; --- Literals ---
(string_literal) @df.literal
(integer_literal) @df.literal
(float_literal) @df.literal
(char_literal) @df.literal
(boolean_literal) @df.literal

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use
