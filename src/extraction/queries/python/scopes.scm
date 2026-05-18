; Python scopes query
; Captures containment scopes: module, functions, classes, methods, blocks

(module) @scope.file

(function_definition) @scope.function

(lambda) @scope.function

(class_definition) @scope.class

(block) @scope.block

; Conditional scopes
(if_statement) @scope.conditional
(try_statement) @scope.conditional
(with_statement) @scope.conditional

; Loop scopes
(for_statement) @scope.loop
(while_statement) @scope.loop
