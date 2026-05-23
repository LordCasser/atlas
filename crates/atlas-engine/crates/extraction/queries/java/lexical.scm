;; Java lexical binding captures: parameters, locals, for-each vars, catch vars
;;
;; Each capture produces a BindingDef via normalize_java_lexical().

;; --- Method/constructor parameters ---
(formal_parameter
  name: (identifier) @lexical.parameter)

;; --- Local variable declarations (int x = 5, String s) ---
(local_variable_declaration
  declarator: (variable_declarator
    name: (identifier) @lexical.local))

;; --- Enhanced for-loop variable (for (Type var : collection)) ---
(enhanced_for_statement
  name: (identifier) @lexical.local)

;; --- Catch clause variable (catch (Exception e)) ---
(catch_clause
  (catch_formal_parameter
    name: (identifier) @lexical.catch_variable))

;; --- Lambda parameter (x -> expr) ---
(lambda_expression
  (identifier) @lexical.parameter)
