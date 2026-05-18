;; Cangjie dataflow query
;; Captures: parameters, return, assignments, field writes

;; Function parameters (variableName is aliased from identifier in parameter)
(functionDefinition (parameterList (parameter (variableName) @dataflow.parameter)))

;; Return expressions
(return (expression)? @dataflow.return)

;; Variable assignments
(variableDeclaration (variableName) @dataflow.assign)

;; Field writes: obj.field = value
(assignment (fieldAccess (atomicVariable) @dataflow.field_write))
