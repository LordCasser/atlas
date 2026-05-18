;; Dataflow captures: parameters, returns, assignments, field access

;; Function/method/arrow parameters (no field labels for tree-sitter compat)
(function_declaration
  (formal_parameters (required_parameter (identifier) @dataflow.parameter)))
(method_definition
  (formal_parameters (required_parameter (identifier) @dataflow.parameter)))
(arrow_function
  (formal_parameters (required_parameter (identifier) @dataflow.parameter)))

;; Catch-all: any required_parameter's identifier
(required_parameter (identifier) @dataflow.parameter)

;; Return statements
(return_statement (identifier)? @dataflow.return)

;; Variable assignments
(variable_declarator (identifier) @dataflow.assign)

;; Field writes
(assignment_expression (member_expression (property_identifier) @dataflow.field_write))

;; Field reads
(member_expression (property_identifier) @dataflow.field_read)
