;; Rust imports query
;; Captures: use declarations, extern crate declarations

;; Use declarations (simple)
(use_declaration
  argument: (identifier) @import.module)

;; Use declarations (scoped path)
(use_declaration
  argument: (scoped_identifier) @import.module)

;; Use declarations (use list)
(use_declaration
  argument: (use_list
    (identifier) @import.module))

;; Extern crate declarations
(extern_crate_declaration
  name: (identifier) @import.module)
