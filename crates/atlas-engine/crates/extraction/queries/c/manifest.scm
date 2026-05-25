;; C manifest definitions — top-level symbols only
;; Wraps every pattern in (translation_unit ...) to restrict to file scope.
;; No nested (function-body) symbols are captured.

(translation_unit
  (function_definition
    (function_declarator (identifier) @definition.function)))

(translation_unit
  (function_definition
    (pointer_declarator
      (function_declarator (identifier) @definition.function))))

(translation_unit
  (struct_specifier
    (type_identifier) @definition.class
    (field_declaration_list)))

(translation_unit
  (enum_specifier (type_identifier) @definition.enum))

(translation_unit
  (type_definition (type_identifier) @definition.type_alias))

(translation_unit
  (preproc_def (identifier) @definition.macro))

(translation_unit
  (declaration (identifier) @definition.variable))
