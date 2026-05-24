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

;; Destructuring let: let (a, b) = expr
(let_declaration
  pattern: (tuple_pattern
    (identifier) @df.assign_target)
  value: (_) @df.assign_value)

(let_declaration
  pattern: (tuple_struct_pattern
    (identifier) @df.assign_target)
  value: (_) @df.assign_value)

;; --- Assignments: x = expr ---
(assignment_expression
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Return expressions ---
(return_expression
  (_) @df.return_value)

;; --- Tail return (last expression in block, no semicolon) ---
;; Captures the last named child before "}" in a block body.
;; The normalize function filters out non-expression nodes
;; (e.g. let_declaration) that might match as the last child.
(block
  (_) @df.tail_return
  .
  "}")

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

;; --- Match arm bindings: match value { Pattern(x) => ... } ---
;; Simple binding: x => ...
(match_arm
  pattern: (match_pattern
    (identifier) @df.assign_target))

;; mut binding: mut x => ...
(match_arm
  pattern: (match_pattern
    (mut_pattern
      (identifier) @df.assign_target)))

;; ref binding: ref x => ...
(match_arm
  pattern: (match_pattern
    (ref_pattern
      (identifier) @df.assign_target)))
