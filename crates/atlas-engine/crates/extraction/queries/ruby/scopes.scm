;; Ruby scopes query
;; Captures: file, module, class, method, block, loop, conditional

(program) @scope.file

(module) @scope.module

(class) @scope.class

(method) @scope.method

(singleton_method) @scope.method

(block) @scope.block

(do_block) @scope.block

(if) @scope.conditional

(unless) @scope.conditional

(case) @scope.conditional

(case_match) @scope.conditional

(while) @scope.loop

(until) @scope.loop

(for) @scope.loop

(begin) @scope.block
