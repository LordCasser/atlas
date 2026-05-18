;; Java dataflow captures: parameters, returns, assignments

;; Method parameters
(formal_parameter (identifier) @dataflow.parameter)

;; Return statements
(return_statement (identifier)? @dataflow.return)

;; Variable assignments
(variable_declarator (identifier) @dataflow.assign)

;; Assignment expressions
(assignment_expression (identifier) @dataflow.assign)

;; Field write
(assignment_expression
  (field_access (identifier) @dataflow.field_write))

;; Field read
(field_access (identifier) @dataflow.field_read)
