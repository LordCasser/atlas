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
  (variable_declarator
    name: (identifier) @lexical.local))
(variable_declaration
  (variable_declarator
    name: (identifier) @lexical.local))

;; `let`/`const` declaration destructuring. Captures stay broad so nested
;; patterns are covered; the adapter keeps only binding leaves under the
;; `name` field of a lexical_declaration variable_declarator.
(array_pattern (identifier) @lexical.declaration_variable)
(pair_pattern value: (identifier) @lexical.declaration_variable)
(rest_pattern (identifier) @lexical.declaration_variable)
(assignment_pattern left: (identifier) @lexical.declaration_variable)
(shorthand_property_identifier_pattern) @lexical.declaration_variable

;; --- for...of / for...in iteration variables ---
;; The leaf captures are intentionally broad because destructuring patterns
;; may nest. The adapter keeps only supported leaves inside the `left` field of
;; a for_in_statement and creates bindings only for let/const declarations.
(for_in_statement
  left: (identifier) @lexical.for_variable)
(array_pattern (identifier) @lexical.for_variable)
(pair_pattern value: (identifier) @lexical.for_variable)
(rest_pattern (identifier) @lexical.for_variable)
(assignment_pattern left: (identifier) @lexical.for_variable)
(shorthand_property_identifier_pattern) @lexical.for_variable

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
