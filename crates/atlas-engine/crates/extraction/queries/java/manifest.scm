;; Java manifest definitions — top-level (compilation_unit) symbols only
;; No method_declaration, constructor_declaration, field_declaration (nested in class body).

(program
  (class_declaration (identifier) @definition.class))

(program
  (interface_declaration (identifier) @definition.interface))

(program
  (enum_declaration (identifier) @definition.enum))

(program
  (annotation_type_declaration (identifier) @definition.interface))
