;; PHP lexical binding captures: parameters, foreach vars, catch vars, static vars
;; Note: assignment LHS ($x = ...) is NOT treated as a binding definition —
;; only explicit declaration points (param, foreach, catch, static) create BindingDefs.

;; --- Function/method parameters ---
(simple_parameter
  name: (variable_name) @lexical.parameter)

;; --- Foreach key/value variables (foreach ($items as $key => $value)) ---
;; Anchor after `as` so the collection expression is never misclassified as a
;; declaration. Direct variables, references, and one-level destructuring are
;; covered; nested destructuring remains a capability boundary.
(foreach_statement
  "as"
  (variable_name) @lexical.local)

(foreach_statement
  "as"
  (by_ref
    (variable_name) @lexical.local))

(foreach_statement
  "as"
  (pair
    (variable_name) @lexical.local))

(foreach_statement
  "as"
  (pair
    (by_ref
      (variable_name) @lexical.local)))

(foreach_statement
  "as"
  (list_literal
    (variable_name) @lexical.local))

(foreach_statement
  "as"
  (list_literal
    (by_ref
      (variable_name) @lexical.local)))

(foreach_statement
  "as"
  (pair
    (list_literal
      (variable_name) @lexical.local)))

;; --- Catch variable (catch (Exception $e)) ---
(catch_clause
  name: (variable_name) @lexical.catch_variable)

;; --- Static variable declaration (static $x = 1) ---
(static_variable_declaration
  name: (variable_name) @lexical.local)
