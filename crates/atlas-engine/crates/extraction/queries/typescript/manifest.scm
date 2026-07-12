; TypeScript/JavaScript manifest definitions — top-level symbols only
; Wraps every pattern in (program ...) to restrict to file scope.
; No nested (method_definition, variable_declarator inside functions) are captured.

(program
  (function_declaration
    name: (identifier) @definition.function))

(program
  (generator_function_declaration
    name: (identifier) @definition.function))

(program
  (class_declaration
    name: (type_identifier) @definition.class))

(program
  (abstract_class_declaration
    name: (type_identifier) @definition.class))

(program
  (interface_declaration
    name: (type_identifier) @definition.interface))

(program
  (enum_declaration
    name: (identifier) @definition.enum))

(program
  (type_alias_declaration
    name: (type_identifier) @definition.type_alias))

(program
  (export_statement
    (function_declaration
      name: (identifier) @definition.function)))

(program
  (export_statement
    (class_declaration
      name: (type_identifier) @definition.class)))

(program
  (export_statement
    (abstract_class_declaration
      name: (type_identifier) @definition.class)))

;; Top-level variable declarations (const/let/var at module scope)
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @definition.variable)))
