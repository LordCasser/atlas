;; PHP definitions query
;; Captures: class, interface, trait, enum, function, method, property,
;;           constant, namespace

;; Class declarations
(class_declaration name: (name) @definition.class)

;; Interface declarations
(interface_declaration name: (name) @definition.interface)

;; Trait declarations
(trait_declaration name: (name) @definition.trait)

;; Enum declarations
(enum_declaration name: (name) @definition.enum)

;; Function definitions
(function_definition name: (name) @definition.function)

;; Method declarations
(method_declaration name: (name) @definition.method)

;; Property declarations
(property_declaration
  (property_element
    (variable_name) @definition.property))

;; Class constants
(const_declaration
  (const_element (name) @definition.constant))

;; Namespace definitions
(namespace_definition name: (namespace_name) @definition.namespace)
