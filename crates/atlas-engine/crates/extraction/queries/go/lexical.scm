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
      (identifier) @lexical.local)))
