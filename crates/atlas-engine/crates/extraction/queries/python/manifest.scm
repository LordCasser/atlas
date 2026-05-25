; Python manifest definitions — top-level (module-level) symbols only
; No class-level methods, no nested function definitions.

(module
  (function_definition
    name: (identifier) @definition.function))

(module
  (class_definition
    name: (identifier) @definition.class))

(module
  (decorated_definition
    definition: (function_definition
      name: (identifier) @definition.function)))

(module
  (decorated_definition
    definition: (class_definition
      name: (identifier) @definition.class)))
