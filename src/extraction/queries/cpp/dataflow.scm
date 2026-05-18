;; C++ dataflow query
;; Captures: parameters, return, assignments

;; Function parameters
(parameter_declaration (identifier) @dataflow.parameter)

(primitive_type) @dataflow.type

;; Return statements
(return_statement (identifier) @dataflow.return)

;; Variable assignments
(init_declarator (identifier) @dataflow.assign)
