;; C definitions query
;; Captures: function, struct, enum, typedef, macro, variable

;; Function definitions
(function_definition (identifier) @definition.function)

;; Struct declarations
(struct_specifier (type_identifier) @definition.class)

;; Enum declarations
(enum_specifier (type_identifier) @definition.enum)

;; Typedef declarations
(type_definition (type_identifier) @definition.type_alias)

;; Preprocessor macro definitions
(preproc_def (identifier) @definition.macro)

;; Variable declarations at file scope
(declaration (identifier) @definition.variable)
