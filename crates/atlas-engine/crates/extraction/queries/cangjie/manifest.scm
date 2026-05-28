; Cangjie manifest definitions — top-level (file scope) symbols only
; No function-local, class-body nested definitions.

(translationUnit
  (functionDefinition
    name: (funcName) @definition.function))

(translationUnit
  (classDefinition
    name: (className) @definition.class))

(translationUnit
  (interfaceDefinition
    name: (interfaceName) @definition.interface))

(translationUnit
  (enumDefinition) @definition.enum)

(translationUnit
  (variableDeclaration
    name: (variableName) @definition.variable))
