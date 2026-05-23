;; C dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Function parameters ---
(parameter_declaration
  declarator: (identifier) @df.parameter)

;; --- Assignments: target = value ---
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Local variable declarations with initializer ---
(init_declarator
  declarator: (identifier) @df.assign_target
  value: (_) @df.assign_value)

;; --- Return statements ---
(return_statement
  (_) @df.return_value)

;; --- Call targets: func(args) ---
(call_expression
  function: (identifier) @df.call_target)

;; --- Call arguments ---
(argument_list
  (_) @df.call_arg)

;; --- Field access: obj.field (member_expression) ---
(member_expression
  object: (_) @df.receiver
  property: (identifier) @df.field_name)

;; --- Field access via pointer: ptr->field ---
(pointer_expression
  argument: (_) @df.receiver
  operator: "->"
  (field_identifier) @df.field_name)

;; --- Literals ---
(string_literal) @df.literal
(number_literal) @df.literal
(char_literal) @df.literal
(true) @df.literal
(false) @df.literal
(nullptr) @df.literal

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use
