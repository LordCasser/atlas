;; C++ definitions query
;; Captures: function, method, class, struct, namespace, enum, template, variable

;; Function definitions (identifier may be nested in declarator chain)
(function_definition (identifier) @definition.function)

(function_definition
  (pointer_declarator
    (function_declarator (identifier) @definition.function)))

(function_definition
  (function_declarator (identifier) @definition.function))

;; Method definitions inside class (field_identifier in function_declarator)
(function_definition
  (function_declarator (field_identifier) @definition.method))

;; Method definitions with reference_declarator wrapper (e.g. const std::string& getName())
(function_definition
  (reference_declarator
    (function_declarator (field_identifier) @definition.method)))

;; Method definitions with qualified return type
(function_definition
  (qualified_identifier)
  (function_declarator (field_identifier) @definition.method))

;; Method definitions with qualified return type and reference_declarator
(function_definition
  (qualified_identifier)
  (reference_declarator
    (function_declarator (field_identifier) @definition.method)))

;; Class declarations
(class_specifier (type_identifier) @definition.class)

;; Struct declarations (treated as class in Atlas)
(struct_specifier (type_identifier) @definition.class)

;; Namespace declarations
(namespace_definition name: (_) @definition.namespace)

;; Enum declarations
(enum_specifier (type_identifier) @definition.enum)

;; Variable declarations
(declaration (identifier) @definition.variable)

;; Preprocessor macro definitions
(preproc_def (identifier) @definition.macro)

;; Template declarations
(template_declaration
  (function_definition (identifier) @definition.function))

(template_declaration
  (class_specifier (type_identifier) @definition.class))
