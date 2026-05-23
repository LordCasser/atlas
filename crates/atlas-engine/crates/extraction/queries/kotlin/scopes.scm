;; Kotlin scopes query (tree-sitter-kotlin v0.4.0)
;; Captures: file, class, function, control body, loop, conditional
;; Note: no generic 'block' node — uses function_body and control_structure_body

(source_file) @scope.file

(class_declaration) @scope.class

(object_declaration) @scope.class

(function_body) @scope.block

(if_expression) @scope.conditional

(when_expression) @scope.conditional

(for_statement) @scope.loop

(while_statement) @scope.loop

(try_expression) @scope.conditional
