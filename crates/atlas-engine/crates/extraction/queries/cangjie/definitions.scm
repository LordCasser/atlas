;; Cangjie definitions query
;; Uses aliased node names: className, interfaceName, enumName, funcName, variableName
;;
;; main() in Cangjie is parsed as mainDefinition, not functionDefinition —
;; the name is hard-coded to "main" in normalize_cangjie_definition.

(classDefinition (className) @definition.class)

(interfaceDefinition (interfaceName) @definition.interface)

(enumDefinition) @definition.enum

(functionDefinition (funcName) @definition.function)

;; Entry point: main() is a dedicated mainDefinition AST node (not functionDefinition).
;; Captured as @definition.entry so the normalizer extracts its first child token as the name.
(mainDefinition) @definition.entry

(variableDeclaration (variableName) @definition.variable)
