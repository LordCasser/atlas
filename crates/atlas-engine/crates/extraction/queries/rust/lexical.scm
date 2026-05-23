;; Rust lexical binding captures: parameters, locals, for-loop vars, match bindings

;; --- Function parameters ---
(parameters
  (parameter
    pattern: (identifier) @lexical.parameter))

;; --- Self parameter (methods) ---
(self_parameter
  (self) @lexical.parameter)

;; --- Let bindings (let x = expr) ---
(let_declaration
  pattern: (identifier) @lexical.local)

;; --- For-loop variable (for x in ...) ---
(for_expression
  pattern: (identifier) @lexical.local)

;; --- Closure parameters (|x, y| expr) ---
(closure_parameters
  (identifier) @lexical.parameter)

;; --- Match arm bindings ---
(match_pattern
  (identifier) @lexical.local)
