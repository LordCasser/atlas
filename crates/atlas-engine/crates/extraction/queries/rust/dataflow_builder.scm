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

;; --- Qualified calls: Path::to::func(args) ---
(call_expression
  function: (scoped_identifier) @df.call_target)

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

;; --- Match scrutinees and pattern bindings ---
(match_expression
  value: (_) @df.match_subject)

;; Broad captures are classified by the Rust adapter so nested patterns share
;; the same syntax rules as lexical binding extraction.
(match_pattern
  (identifier) @df.pattern_target)
(_pattern
  (identifier) @df.pattern_target)
(field_pattern
  pattern: (identifier) @df.pattern_target)
(shorthand_field_identifier) @df.pattern_target
