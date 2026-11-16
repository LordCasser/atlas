;; C++ scopes query
;; Captures: translation unit, function, class, namespace, block, control flow

(translation_unit) @scope.file

(function_definition) @scope.function

;; Lambda parameters and captures inhabit their own scope, including the
;; declarator before the body block. They must not leak into the enclosing block.
(lambda_expression) @scope.function

(class_specifier (field_declaration_list)) @scope.class

(struct_specifier (field_declaration_list)) @scope.class

(enum_specifier (enumerator_list)) @scope.enum

(namespace_definition) @scope.namespace

(compound_statement) @scope.block

(if_statement) @scope.conditional

(for_statement) @scope.loop

(while_statement) @scope.loop
