;; Rust scopes query
;; Captures: file, module, function, block, struct, enum, trait, loop, conditional

(source_file) @scope.file

(mod_item) @scope.module

(function_item) @scope.function

(struct_item) @scope.class

(enum_item) @scope.class

(trait_item) @scope.trait

(block) @scope.block

(if_expression) @scope.conditional

(match_expression) @scope.conditional

(for_expression) @scope.loop

(while_expression) @scope.loop

(loop_expression) @scope.loop
