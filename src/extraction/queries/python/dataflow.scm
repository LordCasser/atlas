;; Dataflow captures: parameters, returns, assignments, field access

;; Function parameters (no field labels for tree-sitter compat)
(function_definition
  (parameters (identifier) @dataflow.parameter))
(lambda
  (lambda_parameters (identifier) @dataflow.parameter))

;; Return statements
(return_statement (identifier)? @dataflow.return)

;; Assignments
(assignment (identifier) @dataflow.assign)
(augmented_assignment (identifier) @dataflow.assign)

;; Field writes
(assignment (attribute (identifier) @dataflow.field_write))
(augmented_assignment (attribute (identifier) @dataflow.field_write))

;; Field reads
(attribute (identifier) @dataflow.field_read)
