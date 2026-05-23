;; Lexical binding captures: parameters, locals, import aliases, catch variables, destructuring
;;
;; Each capture produces a BindingDef via normalize_lexical().

;; --- Function parameters ---
(function_declaration
  (formal_parameters (required_parameter (identifier) @lexical.parameter)))
(method_definition
  (formal_parameters (required_parameter (identifier) @lexical.parameter)))
(arrow_function
  (formal_parameters (required_parameter (identifier) @lexical.parameter)))
;; Catch-all required_parameter
(required_parameter (identifier) @lexical.parameter)
;; Optional parameters
(optional_parameter (identifier) @lexical.parameter)

;; --- Local variable declarations (let/const/var) ---
(lexical_declaration
  (variable_declarator (identifier) @lexical.local))
(variable_declaration
  (variable_declarator (identifier) @lexical.local))

;; --- Import aliases ---
(import_specifier (identifier) @lexical.import_alias)
;; Default import: `import foo from 'bar'` — the import_clause wraps the identifier
(import_clause (identifier) @lexical.import_alias)
;; Namespace import: `import * as foo from 'bar'`
(namespace_import (identifier) @lexical.import_alias)

;; --- Catch variable bindings ---
(catch_clause (identifier) @lexical.catch_variable)

;; --- Lambda/arrow parameters (already covered by arrow_function above) ---
;; Class property shorthand binds to field
(public_field_definition (identifier) @lexical.field)
