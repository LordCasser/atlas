;; Bash definitions query
;; Captures: function, variable
;; Note: Bash has no class/interface/struct/import — simplified Symbolic profile

;; Function definitions
(function_definition name: (word) @definition.function)

;; Variable assignments
(variable_assignment name: (variable_name) @definition.variable)

;; Declaration commands (declare, typeset, local)
(declaration_command (variable_name) @definition.variable)
