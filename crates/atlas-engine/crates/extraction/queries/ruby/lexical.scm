;; Ruby lexical binding captures: parameters, locals, block params, rescue vars

;; --- Method parameters ---
(method_parameters
  (identifier) @lexical.parameter)

;; --- Optional parameters (x = default) ---
(optional_parameter
  name: (identifier) @lexical.parameter)

;; --- Local variable assignments (x = expr) ---
(assignment
  left: (identifier) @lexical.local)

;; --- Block parameters (|x, y|) ---
(block_parameters
  (identifier) @lexical.parameter)

;; --- Rescue variable (rescue => e) ---
(rescue
  (exception_variable
    (identifier) @lexical.catch_variable))

;; --- For-loop variable (for x in ...) ---
(for
  pattern: (identifier) @lexical.local)
