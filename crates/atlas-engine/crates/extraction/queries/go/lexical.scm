;; Go lexical binding captures: parameters, locals, short-var, for-range vars
;;
;; Each capture produces a BindingDef via normalize_go_lexical().

;; --- Function/method parameters ---
(parameter_declaration
  name: (identifier) @lexical.parameter)

;; --- Short variable declarations (x := expr) ---
(short_var_declaration
  left: (expression_list
    (identifier) @lexical.local))

;; --- Var declarations ---
(var_spec
  name: (identifier) @lexical.local)

;; --- For-range loop variables (for i, v := range ...) ---
(for_statement
  (range_clause
    left: (expression_list
      (identifier) @lexical.local)
    ":="))

;; --- Select receive declarations (case value, ok := <-ch) ---
;; A communication clause is an implicit lexical block. `=` receive targets
;; are writes to existing bindings and therefore intentionally do not match.
(select_statement
  (communication_case
    communication: (receive_statement
      left: (expression_list
        (identifier) @lexical.receive_local)
      ":="
      right: (unary_expression "<-"))))

;; A type-switch alias is declared separately in the implicit block of every
;; clause. The adapter turns each captured clause into one synthetic binding
;; declaration at the start of its statement list.
(type_switch_statement
  (type_case) @lexical.type_switch_clause)

(type_switch_statement
  (default_case) @lexical.type_switch_clause)
