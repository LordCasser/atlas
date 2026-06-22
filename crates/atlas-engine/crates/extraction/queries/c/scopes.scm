;; C scopes query
;; Captures: translation unit, function, struct body, block

(translation_unit) @scope.file

(function_definition) @scope.function

(struct_specifier (field_declaration_list)) @scope.class

(enum_specifier (enumerator_list)) @scope.enum

(compound_statement) @scope.block

(if_statement) @scope.conditional

(for_statement) @scope.loop

(while_statement) @scope.loop

(do_statement) @scope.loop

(switch_statement) @scope.conditional
