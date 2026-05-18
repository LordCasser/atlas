;; C++ scopes query
;; Captures: translation unit, function, class, namespace, block, control flow

(translation_unit) @scope.file

(function_definition (compound_statement) @scope.function)

(class_specifier (field_declaration_list) @scope.class)

(namespace_definition) @scope.namespace

(compound_statement) @scope.block

(if_statement) @scope.conditional

(for_statement) @scope.loop

(while_statement) @scope.loop
