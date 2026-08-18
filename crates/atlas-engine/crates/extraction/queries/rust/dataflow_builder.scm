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

;; --- Direct-variable read-modify-write expressions ---
(compound_assignment_expr
  left: (identifier) @df.mutation_target
  right: (_) @df.assign_value) @df.mutation_value

(compound_assignment_expr
  left: (identifier) @df.mutation_read)

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

;; Each supported let-condition RHS is an independent value source. The adapter
;; filters this broad capture to match guards and if/while conditions.
(let_condition
  value: (_) @df.let_condition_value)

;; Broad captures are classified by the Rust adapter so nested patterns share
;; the same syntax rules as lexical binding extraction. The second capture on
;; each binding lets the adapter materialize an exact syntactic projection when
;; the path is knowable without type or slice-length inference.
(match_pattern
  (identifier) @df.pattern_target @df.pattern_projection)
(_pattern
  (identifier) @df.pattern_target @df.pattern_projection)
(field_pattern
  pattern: (identifier) @df.pattern_target @df.pattern_projection)
(shorthand_field_identifier) @df.pattern_target @df.pattern_projection
