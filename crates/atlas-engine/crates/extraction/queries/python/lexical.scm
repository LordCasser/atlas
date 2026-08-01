;; Python lexical binding captures: parameters, locals, loop vars,
;; with/except aliases, import aliases, tuple unpacking

;; --- Function parameters ---
(parameters (identifier) @lexical.parameter)
(default_parameter
  name: (identifier) @lexical.parameter)
(typed_parameter
  (identifier) @lexical.parameter)
(typed_default_parameter
  name: (identifier) @lexical.parameter)
;; *args, **kwargs
(list_splat_pattern
  (identifier) @lexical.parameter)
(dictionary_splat_pattern
  (identifier) @lexical.parameter)

;; --- Assignment targets (treated as local definition) ---
(assignment
  left: (identifier) @lexical.local)

;; --- Loop targets ---
(for_statement
  left: (identifier) @lexical.local)
(for_in_clause
  left: (identifier) @lexical.local)

;; --- With statement alias (with open(f) as x) ---
(with_item
  (identifier) @lexical.local)

;; --- Except alias (except Exception as e) ---
(except_clause
  (identifier) @lexical.catch_variable)

;; --- Import alias (import foo as bar) ---
(aliased_import
  alias: (identifier) @lexical.import_alias)

;; --- Tuple/list unpacking ---
(pattern_list
  (identifier) @lexical.local)
(tuple_pattern
  (identifier) @lexical.local)

;; --- Structural pattern matching captures ---
;; The adapter filters class/value/keyword names from these broad grammar
;; shapes, retaining only capture, `as`, and star/rest binding identifiers.
(dotted_name
  (identifier) @lexical.pattern)
(as_pattern
  (identifier) @lexical.pattern)
(splat_pattern
  (identifier) @lexical.pattern)
