;; Cangjie scopes query
;; Captures: file, class, interface, function, main, block

(translationUnit) @scope.file
(classDefinition) @scope.class
(interfaceDefinition) @scope.interface
(functionDefinition) @scope.function
(mainDefinition) @scope.function
(forInExpression) @scope.loop
(matchCase) @scope.conditional
(block) @scope.block
