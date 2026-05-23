;; Go dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access
;;
;; These captures feed into DataFlowBuilder which creates DataNodes and DataFlowEdges.

;; --- Function/method parameters ---
(parameter_declaration
  name: (identifier) @df.parameter)

;; --- Short variable declarations (x := expr) ---
(short_var_declaration
  left: (expression_list
    (identifier) @df.assign_target)
  right: (expression_list
    (_) @df.assign_value))

;; --- Assignment statements (x = expr) ---
(assignment_statement
  left: (expression_list
    (_) @df.assign_target)
  right: (expression_list
    (_) @df.assign_value))

;; --- Var declarations with initializer ---
(var_declaration
  (var_spec
    name: (identifier) @df.assign_target
    value: (expression_list
      (_) @df.assign_value)))

;; --- Return statements ---
(return_statement
  (expression_list
    (_) @df.return_value))

;; --- Call targets: func(args) ---
(call_expression
  function: (identifier) @df.call_target)

;; --- Method calls: obj.method(args) ---
(call_expression
  function: (selector_expression
    field: (field_identifier) @df.call_target))

;; --- Call arguments ---
(argument_list
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(selector_expression
  operand: (_) @df.receiver
  field: (field_identifier) @df.field_name)

;; --- Literals ---
(int_literal) @df.literal
(float_literal) @df.literal
(interpreted_string_literal) @df.literal
(raw_string_literal) @df.literal
(rune_literal) @df.literal
(true) @df.literal
(false) @df.literal
(nil) @df.literal

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use
