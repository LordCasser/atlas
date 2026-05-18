; Python scopes query
; Captures containment scopes: module, functions, classes, methods, blocks

(module) @scope.file

(function_definition) @scope.function

(lambda) @scope.function

(class_definition) @scope.class

(block) @scope.block

; if/for/while/with/try blocks as child scopes
(if_statement) @scope.block

(for_statement) @scope.block

(while_statement) @scope.block

(with_statement) @scope.block

(try_statement) @scope.block
