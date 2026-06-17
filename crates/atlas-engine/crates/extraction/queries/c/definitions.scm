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

;; Enum definitions (require body to exclude plain enum-typed variables)
(enum_specifier
  (type_identifier) @definition.enum
  (enumerator_list))

;; Typedef declarations
(type_definition (type_identifier) @definition.type_alias)

;; Preprocessor macro definitions
(preproc_def (identifier) @definition.macro)

;; Struct field declarations (function pointer or data member)
;; Function pointer field: CURLcode (*do_it)(struct connectdata *, int *);
(field_declaration
  (function_declarator
    (pointer_declarator
      (function_declarator
        (field_identifier) @definition.field))))

;; Function pointer field (simpler case): void (*handler)(int);
(field_declaration
  (pointer_declarator
    (function_declarator
      (field_identifier) @definition.field)))

;; Regular data field: int port;
(field_declaration
  (type_identifier)
  (field_identifier) @definition.field)

;; Pointer-typed data field (single pointer): void *buffer; struct dso *ptr;
;; field_identifier is direct child of pointer_declarator (NOT inside function_declarator)
(field_declaration
  (pointer_declarator
    (field_identifier) @definition.field))

;; Pointer-typed data field (double pointer): char **argv; struct node **head;
(field_declaration
  (pointer_declarator
    (pointer_declarator
      (field_identifier) @definition.field)))

;; Pointer-typed data field (triple pointer): void ***data;
(field_declaration
  (pointer_declarator
    (pointer_declarator
      (pointer_declarator
        (field_identifier) @definition.field))))

;; Variable declarations at file scope
(declaration (identifier) @definition.variable)
