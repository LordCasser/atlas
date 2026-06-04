;; Kotlin manifest definitions — top-level (file scope) symbols only
;; Wraps every pattern in (source_file ...) to restrict to file scope.
;; Excludes local variable declarations (variable_declaration without
;; property wrapper) — those are captured by the full definitions query.

(source_file
  (class_declaration (type_identifier) @definition.class))

(source_file
  (object_declaration (type_identifier) @definition.class))

(source_file
  (function_declaration (simple_identifier) @definition.function))

(source_file
  (property_declaration
    (variable_declaration
      (simple_identifier) @definition.property)))

(source_file
  (package_header
    (identifier) @definition.package))
