;; Ruby manifest definitions — top-level (file scope) symbols only
;; Wraps every pattern in (program ...) to restrict to file scope.
;; Excludes local assignments, attr_* DSL calls (inside class bodies),
;; and nested method/constant definitions — those are captured by the
;; full definitions query.

(program
  (class name: (constant) @definition.class))

(program
  (module name: (constant) @definition.module))

(program
  (method name: (identifier) @definition.method))

(program
  (singleton_method name: (identifier) @definition.method))

(program
  (assignment left: (constant) @definition.constant))
