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

(select_statement) @scope.conditional
