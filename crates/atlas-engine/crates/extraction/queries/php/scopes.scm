;; PHP scopes query
;; Captures: file, namespace, class, interface, trait, function, method,
;;           block, loop, conditional

(program) @scope.file

(namespace_definition) @scope.namespace

(class_declaration) @scope.class

(interface_declaration) @scope.interface

(trait_declaration) @scope.class

(function_definition) @scope.function

;; Anonymous functions establish an independent local-variable namespace.
;; Arrow functions are intentionally excluded until callable ownership can be
;; represented without attributing their facts to an enclosing named function.
(anonymous_function) @scope.function

(method_declaration) @scope.method

(compound_statement) @scope.block

(if_statement) @scope.conditional

(switch_statement) @scope.conditional

(while_statement) @scope.loop

(do_statement) @scope.loop

(for_statement) @scope.loop

(foreach_statement) @scope.loop

(try_statement) @scope.conditional
