;; C scopes query
;; Captures: translation unit, function body, struct body, block

(translation_unit) @scope.file

(function_definition (compound_statement) @scope.function)

(struct_specifier (field_declaration_list) @scope.class)

(compound_statement) @scope.block

(if_statement) @scope.conditional

(for_statement) @scope.loop

(while_statement) @scope.loop
