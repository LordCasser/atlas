;; Java definitions query
;; Captures: class, interface, enum, method, constructor, field, constant, annotation

;; Class declarations
(class_declaration (identifier) @definition.class)

;; Interface declarations
(interface_declaration (identifier) @definition.interface)

;; Enum declarations
(enum_declaration (identifier) @definition.enum)

;; Method declarations
(method_declaration (identifier) @definition.method)

;; Constructor declarations
(constructor_declaration (identifier) @definition.method)

;; Field declarations (within class body)
(field_declaration (variable_declarator (identifier) @definition.field))

;; Local variable declarations
(local_variable_declaration (variable_declarator (identifier) @definition.variable))

;; Annotation type declarations
(annotation_type_declaration (identifier) @definition.interface)
