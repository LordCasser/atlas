; Python imports query
; Captures import statements: module paths, imported names, aliases

(import_statement
  name: (dotted_name) @import.module
  .)

(import_statement
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias))

(import_from_statement
  module_name: (dotted_name) @import.module
  name: (dotted_name) @import.name)

(import_from_statement
  module_name: (dotted_name) @import.module
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias))

; Wildcard imports
(import_from_statement
  module_name: (dotted_name) @import.module
  name: (wildcard_import) @import.wildcard)

; Relative imports
(import_from_statement
  module_name: (relative_import) @import.module
  name: (dotted_name) @import.name)
