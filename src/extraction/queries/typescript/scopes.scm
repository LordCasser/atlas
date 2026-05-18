; TypeScript/JavaScript scopes query
; Captures containment scopes: file, functions, classes, interfaces, enums, namespaces, blocks

(program) @scope.file

; Function scopes
(function_declaration) @scope.function
(generator_function_declaration) @scope.function
(arrow_function) @scope.function
(function_expression) @scope.function

; Method scopes
(method_definition) @scope.method

; Class/interface/enum scopes
(class_declaration) @scope.class
(class_heritage) @scope.class
(interface_declaration) @scope.interface
(enum_declaration) @scope.enum

; Namespace/module scopes
(module) @scope.namespace

; Block scopes
(statement_block) @scope.block

; Conditional/loop scopes
(if_statement) @scope.conditional
(for_statement) @scope.loop
(for_in_statement) @scope.loop
(while_statement) @scope.loop
(do_statement) @scope.loop
(try_statement) @scope.conditional
(catch_clause) @scope.conditional
(switch_statement) @scope.conditional
