;; C# dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Method/constructor parameters ---
(parameter
  name: (identifier) @df.parameter)

;; --- Assignments: target = value ---
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Local variable declarations with initializer ---
(local_declaration_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @df.assign_target
      equals_value_clause: (equals_value_clause
        (_) @df.assign_value))))

;; --- Return statements ---
(return_statement
  (_) @df.return_value)

;; --- Call targets: method(args) ---
(invocation_expression
  function: (identifier) @df.call_target)

;; --- Method calls: obj.method(args) ---
(invocation_expression
  function: (member_access_expression
    name: (identifier) @df.call_target))

;; --- Call arguments ---
(argument_list
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(member_access_expression
  expression: (_) @df.receiver
  name: (identifier) @df.field_name)

;; --- Literals ---
(string_literal) @df.literal
(integer_literal) @df.literal
(real_literal) @df.literal
(boolean_literal) @df.literal
(null_literal) @df.literal
(character_literal) @df.literal
