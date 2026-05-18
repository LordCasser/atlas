; TypeScript/JavaScript imports query

; Module source for all import statements
(import_statement
  source: (string) @import.module)

; Named imports
(import_specifier
  name: (identifier) @import.name)

; Aliased import
(import_specifier
  alias: (identifier) @import.alias)

; Namespace import: capture the identifier as a wildcard child
(namespace_import
  (identifier) @import.namespace)
