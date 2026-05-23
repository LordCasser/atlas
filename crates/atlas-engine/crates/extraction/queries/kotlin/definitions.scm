;; Kotlin definitions query (tree-sitter-kotlin v0.4.0)
;; Captures: class, object, function, property, variable, package
;; Note: enum/interface use class_declaration with different body types

;; Class declarations (includes regular classes, enums, interfaces)
(class_declaration (type_identifier) @definition.class)

;; Object declarations (singletons, companion objects)
(object_declaration (type_identifier) @definition.class)

;; Function declarations
(function_declaration (simple_identifier) @definition.function)

;; Property declarations
(property_declaration
  (variable_declaration
    (simple_identifier) @definition.property))

;; Variable declarations (local)
(variable_declaration
  (simple_identifier) @definition.variable)

;; Package header
(package_header
  (identifier) @definition.package)
