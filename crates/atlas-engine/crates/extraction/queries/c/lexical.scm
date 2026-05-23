;; C lexical binding captures: parameters, locals, for-loop vars

;; --- Function parameters ---
(parameter_declaration
  declarator: (identifier) @lexical.parameter)

;; --- Local variable declarations (int x = 5) ---
(declaration
  declarator: (init_declarator
    declarator: (identifier) @lexical.local))

;; --- For-loop initializer (for (int i = 0; ...)) ---
(for_statement
  initializer: (declaration
    declarator: (init_declarator
      declarator: (identifier) @lexical.local)))
