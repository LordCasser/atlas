;; PHP manifest definitions — top-level (file scope) symbols only
;; Wraps every pattern in (program ...) to restrict to file scope.
;; Excludes method/property/const declarations — those are nested inside
;; classes/interfaces and captured by the full definitions query.

(program
  (class_declaration name: (name) @definition.class))

(program
  (interface_declaration name: (name) @definition.interface))

(program
  (trait_declaration name: (name) @definition.trait))

(program
  (enum_declaration name: (name) @definition.enum))

(program
  (function_definition name: (name) @definition.function))

(program
  (namespace_definition name: (namespace_name) @definition.namespace))
