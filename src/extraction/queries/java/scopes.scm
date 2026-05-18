;; Java scopes query
;; Captures: program, class, method, block, control flow

(program) @scope.file

(class_declaration) @scope.class
(interface_declaration) @scope.interface
(enum_declaration) @scope.enum

(method_declaration) @scope.method
(constructor_declaration) @scope.method

(block) @scope.block

(if_statement) @scope.conditional
(for_statement) @scope.loop
(while_statement) @scope.loop
(do_statement) @scope.loop
(try_statement) @scope.conditional
(catch_clause) @scope.conditional
(switch_expression) @scope.conditional
