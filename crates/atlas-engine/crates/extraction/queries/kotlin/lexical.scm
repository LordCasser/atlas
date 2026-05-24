;; Kotlin lexical binding captures: parameters, locals, for-loop vars, catch vars

;; --- Function parameters ---
(function_declaration
  (function_value_parameters
    (parameter
      (simple_identifier) @lexical.parameter)))

;; --- Lambda parameters ---
(lambda_literal
  (lambda_parameters
    (variable_declaration
      (simple_identifier) @lexical.parameter)))

;; --- Local variable declarations (val x = 5, var s) ---
(variable_declaration
  (simple_identifier) @lexical.local)

;; --- For-loop variable (for (x in ...)) ---
(for_statement
  (variable_declaration
    (simple_identifier) @lexical.local))

;; --- Catch variable (catch (e: Exception)) ---
(catch_block
  (simple_identifier) @lexical.catch_variable)
