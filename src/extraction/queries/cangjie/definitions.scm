;; Cangjie definitions query
;; Uses aliased node names: className, interfaceName, enumName, funcName, variableName

(classDefinition (className) @definition.class)

(interfaceDefinition (interfaceName) @definition.interface)

(enumDefinition) @definition.enum

(functionDefinition (funcName) @definition.function)

(variableDeclaration (variableName) @definition.variable)
