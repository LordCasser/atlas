;; C definitions query
;; Captures: function, struct, enum, typedef, macro, variable

;; Function definitions (identifier is nested in function_declarator, which may be in pointer_declarator)
(function_definition
  (function_declarator (identifier) @definition.function))

(function_definition
  (pointer_declarator
    (function_declarator (identifier) @definition.function)))

;; Struct definitions (require body to exclude forward declarations and type references)
;; Without the body check, every `struct Foo *ptr` and `struct Foo;` would be
;; falsely captured as a definition, inflating the class symbol count by ~5.5x.
(struct_specifier
  (type_identifier) @definition.class
  (field_declaration_list))

;; Enum declarations
(enum_specifier (type_identifier) @definition.enum)

;; Typedef declarations
(type_definition (type_identifier) @definition.type_alias)

;; Preprocessor macro definitions
(preproc_def (identifier) @definition.macro)

;; Variable declarations at file scope
(declaration (identifier) @definition.variable)
