;; PHP lexical binding captures: parameters, foreach vars, catch vars, static vars
;; Note: assignment LHS ($x = ...) is NOT treated as a binding definition —
;; only explicit declaration points (param, foreach, catch, static) create BindingDefs.

;; --- Function/method parameters ---
(parameter
  name: (variable_name) @lexical.parameter)

;; --- Foreach loop variable (foreach ($a as $v)) ---
(foreach_statement
  value: (variable_name) @lexical.local)

;; --- Catch variable (catch (Exception $e)) ---
(catch_clause
  name: (variable_name) @lexical.catch_variable)

;; --- Static variable declaration (static $x = 1) ---
(static_variable_declaration
  name: (variable_name) @lexical.local)
