;; C++ definitions query
;; Captures: function, method, class, struct, namespace, enum, template, variable

;; Function definitions
(function_definition (identifier) @definition.function)

;; Class declarations
(class_specifier (type_identifier) @definition.class)

;; Struct declarations (treated as class in Atlas)
(struct_specifier (type_identifier) @definition.class)

;; Namespace declarations
(namespace_definition name: (_) @definition.namespace)

;; Enum declarations
(enum_specifier (type_identifier) @definition.enum)

;; Method definitions (outside class body)
(function_definition (field_identifier) @definition.method)

;; Variable declarations
(declaration (identifier) @definition.variable)

;; Preprocessor macro definitions
(preproc_def (identifier) @definition.macro)

;; Template declarations
(template_declaration
  (function_definition (identifier) @definition.function))

(template_declaration
  (class_specifier (type_identifier) @definition.class))
