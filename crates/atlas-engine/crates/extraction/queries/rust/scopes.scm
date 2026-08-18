;; Rust scopes query
;; Captures: file, module, function, block, struct, enum, trait, loop, conditional

(source_file) @scope.file

(mod_item) @scope.module

(function_item) @scope.function

(closure_expression) @scope.function

(struct_item) @scope.class

(enum_item) @scope.class

(trait_item) @scope.trait

(block) @scope.block

(if_expression) @scope.conditional

(match_expression) @scope.conditional

;; Match bindings are visible only in their own guard and arm expression.
(match_arm) @scope.conditional

(for_expression) @scope.loop

(while_expression) @scope.loop

(loop_expression) @scope.loop
