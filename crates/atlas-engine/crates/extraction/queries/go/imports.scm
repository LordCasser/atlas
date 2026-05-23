;; Go imports query
;; Captures: import paths from import declarations

;; Single or multi-line import declarations
(import_declaration
  (import_spec
    path: (interpreted_string_literal) @import.module))
