;; C# definitions query
;; Captures: class, struct, interface, enum, enum_member, method, constructor,
;;           property, field, variable, namespace, delegate

;; Class declarations
(class_declaration name: (identifier) @definition.class)

;; Struct declarations
(struct_declaration name: (identifier) @definition.class)

;; Interface declarations
(interface_declaration name: (identifier) @definition.interface)

;; Enum declarations
(enum_declaration name: (identifier) @definition.enum)

;; Enum member declarations
(enum_member_declaration name: (identifier) @definition.enum_member)

;; Method declarations (including operators)
(method_declaration name: (identifier) @definition.method)

;; Constructor declarations
(constructor_declaration name: (identifier) @definition.constructor)

;; Property declarations
(property_declaration name: (identifier) @definition.property)

;; Field declarations
(field_declaration
  (variable_declaration
    (variable_declarator name: (identifier) @definition.field)))

;; Local variable declarations
(local_declaration_statement
  (variable_declaration
    (variable_declarator name: (identifier) @definition.variable)))

;; Namespace declarations
(namespace_declaration name: (identifier) @definition.namespace)

;; Delegate declarations (treated as function symbols)
(delegate_declaration name: (identifier) @definition.function)
