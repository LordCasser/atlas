;; PHP lexical binding captures: parameters, assignment-created locals,
;; foreach vars, catch vars, static vars, and explicit anonymous-function captures.

;; --- Function/method parameters ---
(simple_parameter
  name: (variable_name) @lexical.parameter)

;; --- Assignment-created locals ---
(assignment_expression
  left: (variable_name) @lexical.local)

(reference_assignment_expression
  left: (variable_name) @lexical.local)

(augmented_assignment_expression
  left: (variable_name) @lexical.local)

(update_expression
  argument: (variable_name) @lexical.local)

;; --- Array destructuring targets ([] and list()) ---
;; tree-sitter-php represents both forms as list_literal at every nesting
;; level. Key expressions and target variables are flat siblings around `=>`;
;; the PHP adapter rejects key reads after capture normalization.
(list_literal
  (variable_name) @lexical.destructure)

(list_literal
  (by_ref
    (variable_name) @lexical.destructure))

;; --- Explicit anonymous-function captures (use ($value, &$other)) ---
(anonymous_function_use_clause
  (variable_name) @lexical.local)

(anonymous_function_use_clause
  (by_ref
    (variable_name) @lexical.local))

;; --- Foreach key/value variables (foreach ($items as $key => $value)) ---
;; Anchor after `as` so the collection expression is never misclassified as a
;; declaration. Direct variables/references are captured here; all nested
;; []/list() targets are covered by the list_literal rules above.
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

;; --- Catch variable (catch (Exception $e)) ---
(catch_clause
  name: (variable_name) @lexical.catch_variable)

;; --- Static variable declaration (static $x = 1) ---
(static_variable_declaration
  name: (variable_name) @lexical.local)
