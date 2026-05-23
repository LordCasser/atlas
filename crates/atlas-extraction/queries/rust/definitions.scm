;; Rust definitions query
;; Captures: function, struct, enum, enum_variant, trait, module, variable,
;;           constant, type_alias, macro, field
;; Note: methods inside impl blocks are captured as @definition.function and
;;       reclassified to Method by the adapter's is_inside_impl() check.

;; Functions (top-level and methods inside impl blocks)
(function_item name: (identifier) @definition.function)

;; Struct definitions
(struct_item name: (type_identifier) @definition.class)

;; Enum definitions
(enum_item name: (type_identifier) @definition.enum)

;; Enum variant definitions
(enum_variant name: (identifier) @definition.enum_member)

;; Trait definitions
(trait_item name: (type_identifier) @definition.trait)

;; Module definitions
(mod_item name: (identifier) @definition.module)

;; Variable bindings (let)
(let_declaration
  pattern: (identifier) @definition.variable)

;; Constants
(const_item name: (identifier) @definition.constant)

;; Static items
(static_item name: (identifier) @definition.constant)

;; Type aliases
(type_item name: (type_identifier) @definition.type_alias)

;; Macro definitions
(macro_definition name: (identifier) @definition.macro)

;; Struct field declarations
(field_declaration name: (field_identifier) @definition.field)
