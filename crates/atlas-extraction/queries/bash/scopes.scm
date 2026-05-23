;; Bash scopes query
;; Captures: file and function only (Bash has no block scoping)

(program) @scope.file

(function_definition) @scope.function
