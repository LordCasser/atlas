;; C# manifest definitions — top-level (file scope) symbols only
;; Wraps every pattern in (compilation_unit ...) to restrict to file scope.
;; Excludes method/constructor/property/field/enum_member/local — those are
;; nested inside types and captured by the full definitions query.

(compilation_unit
  (class_declaration name: (identifier) @definition.class))

(compilation_unit
  (struct_declaration name: (identifier) @definition.class))

(compilation_unit
  (interface_declaration name: (identifier) @definition.interface))

(compilation_unit
  (enum_declaration name: (identifier) @definition.enum))

(compilation_unit
  (delegate_declaration name: (identifier) @definition.function))

(compilation_unit
  (namespace_declaration name: (identifier) @definition.namespace))

(compilation_unit
  (file_scoped_namespace_declaration name: (identifier) @definition.namespace))
