;; Go manifest definitions — top-level (source_file) symbols only
;; No method_declaration (always nested in type), no short_var_declaration.

(source_file
  (function_declaration name: (identifier) @definition.function))

(source_file
  (type_declaration
    (type_spec
      name: (type_identifier) @definition.class
      type: (struct_type))))

(source_file
  (type_declaration
    (type_spec
      name: (type_identifier) @definition.interface
      type: (interface_type))))

(source_file
  (type_declaration
    (type_spec
      name: (type_identifier) @definition.type_alias
      type: [(pointer_type) (slice_type) (map_type) (array_type)
             (channel_type) (function_type) (qualified_type)
             (type_identifier)])))
