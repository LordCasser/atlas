;; C++ definitions query
;; Captures: function, method, class, struct, namespace, enum, template, variable

;; A closure body is a callable distinct from the function constructing it.
(lambda_expression) @definition.lambda

;; Preserve names introduced by aliases even before alias expansion is supported.
;; They can hide an outer class in a different file's receiver lookup.
(alias_declaration name: (type_identifier) @definition.type_alias)
(type_definition declarator: (_) @definition.type_alias)
(namespace_alias_definition name: (namespace_identifier) @definition.type_alias)

;; Function definitions (identifier may be nested in declarator chain)
(function_definition (identifier) @definition.function)

(function_definition
  (pointer_declarator
    (function_declarator (identifier) @definition.function)))

(function_definition
  (reference_declarator
    (function_declarator (identifier) @definition.function)))

(function_definition
  (function_declarator (identifier) @definition.function))

;; Qualified definitions, e.g. int Stub::dispatch() { ... }.
;; Capture the declarator, never a qualified return type or parameter type.
;; A written qualifier alone does not establish whether the owner is a class.
;; The definition need not spell a return type: out-of-class constructors do not.
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier) @definition.function)
  body: (_))

(function_definition
  type: (_)
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (qualified_identifier) @definition.function))
  body: (_))

(function_definition
  type: (_)
  declarator: (reference_declarator
    (function_declarator
      declarator: (qualified_identifier) @definition.function))
  body: (_))

;; Method definitions inside class (field_identifier in function_declarator)
;; Operator names are declarators too; retain their bodies and source owners.
(function_declarator declarator: (operator_name) @definition.method)

;; A destructor has its own declaration and body; the enclosing class is not
;; its caller. Explicit calls such as object.~Type() are field expressions,
;; not function declarators, and must not create another definition here.
(function_declarator declarator: (destructor_name) @definition.method)

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

;; Class method declarations (field_declaration with function_declarator)
(field_declaration
  (function_declarator (field_identifier) @definition.method))

;; Class method declarations with reference_declarator wrapper
(field_declaration
  (reference_declarator
    (function_declarator (field_identifier) @definition.method)))

;; Class method declarations with qualified return type
(field_declaration
  (qualified_identifier)
  (function_declarator (field_identifier) @definition.method))

;; Class method declarations with qualified return type and reference_declarator
(field_declaration
  (qualified_identifier)
  (reference_declarator
    (function_declarator (field_identifier) @definition.method)))

;; Class declarations

;; Plain free-function declarations establish cross-file visibility. Restrict
;; these to file/namespace scope; local declarations require separate binding.
(translation_unit
  (declaration
    declarator: (function_declarator declarator: (identifier) @definition.function_declaration)))
(namespace_definition
  body: (declaration_list
    (declaration
      declarator: (function_declarator declarator: (identifier) @definition.function_declaration))))

;; Pointer/reference return types wrap the callable declarator. They still
;; declare a function, unlike a parenthesized pointer-to-function variable.
(translation_unit
  (declaration declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @definition.function_declaration))))
(namespace_definition body: (declaration_list
  (declaration declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @definition.function_declaration)))))
(translation_unit
  (declaration declarator: (reference_declarator
    (function_declarator declarator: (identifier) @definition.function_declaration))))
(namespace_definition body: (declaration_list
  (declaration declarator: (reference_declarator
    (function_declarator declarator: (identifier) @definition.function_declaration)))))

(class_specifier name: [(type_identifier) (qualified_identifier)] @definition.class)

;; Struct declarations (treated as class in Atlas)
(struct_specifier name: [(type_identifier) (qualified_identifier)] @definition.class)

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
  (declaration declarator: (function_declarator
    declarator: (identifier) @definition.function_declaration)))

(template_declaration
  (declaration declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @definition.function_declaration))))

(template_declaration
  (declaration declarator: (reference_declarator
    (function_declarator declarator: (identifier) @definition.function_declaration))))

(template_declaration
  (function_definition (identifier) @definition.function))

(template_declaration
  (class_specifier name: [(type_identifier) (qualified_identifier)] @definition.class))

;; ===== Field declarations (data members, excluding methods) =====
;; A method returning a pointer is a callable, not a data field.
(field_declaration
  (pointer_declarator
    (function_declarator
      (field_identifier) @definition.method)))

;; Function pointer field with parenthesized pointer declarator:
;; int (*handler)(int);
(field_declaration
  (function_declarator
    (parenthesized_declarator
      (pointer_declarator
        (field_identifier) @definition.field))))

;; Direct data declarators, independent of the spelling of their type.
;; Keep the declarator shape explicit so methods are not captured as fields.
(field_declaration
  declarator: (field_identifier) @definition.field)

(field_declaration
  declarator: (pointer_declarator
    declarator: (field_identifier) @definition.field))

(field_declaration
  declarator: (reference_declarator
    (field_identifier) @definition.field))
