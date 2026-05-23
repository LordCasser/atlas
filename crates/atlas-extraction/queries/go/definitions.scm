;; Go definitions query
;; Captures: function, method, struct, interface, type_alias, variable, constant, package

;; Function declarations
(function_declaration name: (identifier) @definition.function)

;; Method declarations (with receiver)
(method_declaration name: (field_identifier) @definition.method)

;; Struct type declarations
(type_declaration
  (type_spec
    name: (type_identifier) @definition.class
    type: (struct_type)))

;; Interface type declarations
(type_declaration
  (type_spec
    name: (type_identifier) @definition.interface
    type: (interface_type)))

;; Other type alias declarations (non-struct, non-interface)
(type_declaration
  (type_spec
    name: (type_identifier) @definition.type_alias
    type: [
      (pointer_type)
      (slice_type)
      (map_type)
      (array_type)
      (channel_type)
      (function_type)
      (qualified_type)
      (type_identifier)
    ]))

;; Variable declarations
(var_spec name: (identifier) @definition.variable)

;; Constant declarations
(const_spec name: (identifier) @definition.constant)

;; Short variable declarations (:=)
(short_var_declaration
  left: (expression_list (identifier) @definition.variable))

;; Package clause
(package_clause (package_identifier) @definition.package)
