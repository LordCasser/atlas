; Cangjie manifest definitions — top-level (file scope) symbols only
; No function-local, class-body nested definitions.
;
; main() in Cangjie is parsed as mainDefinition, a dedicated AST node
; (not functionDefinition).  The name is hard-coded in the normalizer.

(translationUnit
  (mainDefinition) @definition.entry)

(translationUnit
  (functionDefinition
    (funcName) @definition.function))

(translationUnit
  (classDefinition
    (className) @definition.class))

(translationUnit
  (interfaceDefinition
    (interfaceName) @definition.interface))

(translationUnit
  (enumDefinition) @definition.enum)

(translationUnit
  (variableDeclaration
    (variableName) @definition.variable))
