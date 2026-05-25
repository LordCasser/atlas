;; Rust manifest definitions — top-level (source_file) symbols only
;; No let_declaration, no field_declaration (nested), no enum_variant.

(source_file
  (function_item name: (identifier) @definition.function))

(source_file
  (struct_item name: (type_identifier) @definition.class))

(source_file
  (enum_item name: (type_identifier) @definition.enum))

(source_file
  (trait_item name: (type_identifier) @definition.trait))

(source_file
  (mod_item name: (identifier) @definition.module))

(source_file
  (const_item name: (identifier) @definition.constant))

(source_file
  (static_item name: (identifier) @definition.constant))

(source_file
  (type_item name: (type_identifier) @definition.type_alias))

(source_file
  (macro_definition name: (identifier) @definition.macro))
