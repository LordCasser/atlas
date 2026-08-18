;; C# scopes query
;; Captures: file, namespace, class, struct, interface, method, block, loop, conditional

(compilation_unit) @scope.file

(namespace_declaration) @scope.namespace

(class_declaration) @scope.class

(struct_declaration) @scope.class

(interface_declaration) @scope.interface

(method_declaration) @scope.method

(constructor_declaration) @scope.method

;; Destructor
(destructor_declaration) @scope.method

(block) @scope.block

(if_statement) @scope.conditional

(switch_statement) @scope.conditional

;; Pattern variables are scoped per switch section/arm. Capturing only the
;; enclosing switch would conflate same-named declarations in sibling arms.
(switch_section) @scope.conditional

(switch_expression_arm) @scope.conditional

(conditional_expression) @scope.conditional

(for_statement) @scope.loop

(foreach_statement) @scope.loop

(while_statement) @scope.loop

(do_statement) @scope.loop

(try_statement) @scope.conditional
