;; PHP lexical binding captures: parameters, locals, foreach vars, catch vars

;; --- Function/method parameters ---
(parameter
  name: (variable_name) @lexical.parameter)

;; --- Local variable assignments ($x = expr) ---
(assignment_expression
  left: (variable_name) @lexical.local)

;; --- Foreach loop variable (foreach ($a as $v)) ---
(foreach_statement
  value: (variable_name) @lexical.local)

;; --- Catch variable (catch (Exception $e)) ---
(catch_clause
  name: (variable_name) @lexical.catch_variable)

;; --- Static variable declaration (static $x = 1) ---
(static_variable_declaration
  name: (variable_name) @lexical.local)
