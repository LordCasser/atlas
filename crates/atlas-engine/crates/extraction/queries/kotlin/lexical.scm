;; Kotlin lexical binding captures: parameters, locals, for-loop vars, catch vars

;; --- Function parameters ---
(function_declaration
  (function_value_parameters
    (parameter
      (simple_identifier) @lexical.parameter)))

;; --- Extension function receiver (fun String.isValid()): create "this" binding ---
;; NOTE: type_reference is not available in the pinned tree-sitter-kotlin revision
;; (rev f66d290). Re-enable after upgrading to a grammar version that supports it.
;; (function_declaration
;;   receiver: (type_reference) @lexical.receiver
;;   name: (simple_identifier) @_ext_name
;;   (function_value_parameters))

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
