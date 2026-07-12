; TypeScript/JavaScript definitions query
; Captures symbol definitions: functions, classes, methods, variables, enums, interfaces, type aliases

(function_declaration
  name: (identifier) @definition.function)

(generator_function_declaration
  name: (identifier) @definition.function)

(method_definition
  name: (property_identifier) @definition.method)

(class_declaration
  name: (type_identifier) @definition.class)

(abstract_class_declaration
  name: (type_identifier) @definition.class)

(interface_declaration
  name: (type_identifier) @definition.interface)

(property_signature
  name: (property_identifier) @definition.property)

(method_signature
  name: (property_identifier) @definition.method)

(abstract_method_signature
  name: (property_identifier) @definition.method)

(enum_declaration
  name: (identifier) @definition.enum)

(enum_assignment
  name: (property_identifier) @definition.enum_member)

(enum_body
  (property_identifier) @definition.enum_member)

(type_alias_declaration
  name: (type_identifier) @definition.type_alias)

(variable_declarator
  name: (identifier) @definition.variable)

; Module-level named exports
(export_statement
  declaration: (function_declaration name: (identifier) @definition.function))

(export_statement
  declaration: (class_declaration name: (type_identifier) @definition.class))

(export_statement
  declaration: (abstract_class_declaration name: (type_identifier) @definition.class))

(export_statement
  declaration: (lexical_declaration
    (variable_declarator name: (identifier) @definition.variable)))
