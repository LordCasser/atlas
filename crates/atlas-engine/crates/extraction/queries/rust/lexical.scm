;; Rust lexical binding captures: parameters, locals, for-loop vars, match bindings

;; --- Function parameters ---
(parameters
  (parameter
    pattern: (identifier) @lexical.parameter))

;; --- Self parameter (methods) ---
(self_parameter
  (self) @lexical.parameter)

;; --- For-loop variable (for x in ...) ---
(for_expression
  pattern: (identifier) @lexical.local)

;; --- Closure parameters (|x, y| expr) ---
(closure_parameters
  (identifier) @lexical.parameter)
(closure_parameters
  (parameter
    pattern: (identifier) @lexical.parameter))

;; --- Let declaration, match arm, and let-condition pattern bindings ---
;; Broad grammar captures are filtered by the adapter. This reaches nested
;; tuple/struct/ref/@ patterns in parameters, ordinary let/let-else
;; declarations, match arms, match guards, and if/while conditions while
;; rejecting constructor paths and non-canonical alternatives of an or-pattern.
(match_pattern
  (identifier) @lexical.pattern)
(_pattern
  (identifier) @lexical.pattern)
(field_pattern
  pattern: (identifier) @lexical.pattern)
(shorthand_field_identifier) @lexical.pattern
