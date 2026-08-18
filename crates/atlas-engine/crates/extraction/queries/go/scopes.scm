;; Go scopes query
;; Captures: file, function, method, block, loop, conditional

(source_file) @scope.file

(function_declaration) @scope.function

(method_declaration) @scope.method

(block) @scope.block

(if_statement) @scope.conditional

(for_statement) @scope.loop

(expression_switch_statement) @scope.conditional

(type_switch_statement) @scope.conditional

;; Each type-switch clause is an implicit lexical block in Go. Keep these
;; scopes inside the enclosing switch scope so an alias has one identity per
;; clause, as required by the language specification.
(type_switch_statement
  (type_case) @scope.type_switch_clause)

(type_switch_statement
  (default_case) @scope.type_switch_clause)

(select_statement) @scope.conditional
