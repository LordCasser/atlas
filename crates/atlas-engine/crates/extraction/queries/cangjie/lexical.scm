;; Cangjie lexical binding captures: parameters, locals
;;
;; Each capture produces a BindingDef via normalize_cangjie_lexical().

;; --- Function parameters ---
(parameter
  paraName: (identifier) @lexical.parameter)

;; --- Local variable declarations (let x = expr) ---
(variableDeclaration
  (variableName
    (varBindingPattern) @lexical.local))

;; --- Match pattern bindings ---
;; Broad grammar capture filtered by the adapter: enum constructor names and
;; identifiers outside the pattern portion of a matchCase are not bindings.
(varBindingPattern) @lexical.pattern
