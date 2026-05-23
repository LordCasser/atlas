;; PHP imports query
;; Captures: use declarations, require/include expressions

;; Use declarations
(use_declaration
  (qualified_name) @import.module)

;; Require / include expressions
(require_expression
  (encapsed_string) @import.module)
(include_expression
  (encapsed_string) @import.module)
