;; C++ manifest definitions — top-level symbols only
;; Wraps every pattern in (translation_unit ...) to restrict to file scope.

(translation_unit
  (function_definition (identifier) @definition.function))

(translation_unit
  (function_definition
    (pointer_declarator
      (function_declarator (identifier) @definition.function))))

(translation_unit
  (function_definition
    (function_declarator (identifier) @definition.function)))

(translation_unit
  (class_specifier (type_identifier) @definition.class))

(translation_unit
  (struct_specifier (type_identifier) @definition.class))

(translation_unit
  (namespace_definition name: (_) @definition.namespace))

(translation_unit
  (enum_specifier (type_identifier) @definition.enum))

(translation_unit
  (declaration (identifier) @definition.variable))

(translation_unit
  (preproc_def (identifier) @definition.macro))

(translation_unit
  (template_declaration
    (function_definition (identifier) @definition.function)))

(translation_unit
  (template_declaration
    (class_specifier (type_identifier) @definition.class)))
