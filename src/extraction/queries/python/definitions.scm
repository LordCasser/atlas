; Python definitions query
; Captures symbol definitions: functions, classes, methods, variables

(function_definition
  name: (identifier) @definition.function)

(class_definition
  name: (identifier) @definition.class)

; Module-level and class-level variable assignments
(module
  (expression_statement
    (assignment
      left: (identifier) @definition.variable)))

(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @definition.variable))))

; Decorated definitions
(decorated_definition
  definition: (function_definition
    name: (identifier) @definition.function))

(decorated_definition
  definition: (class_definition
    name: (identifier) @definition.class))
