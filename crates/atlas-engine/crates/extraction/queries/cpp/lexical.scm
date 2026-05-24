;; C++ lexical binding captures: parameters, locals, for-loop vars, catch vars

;; --- Function parameters ---
(parameter_declaration
  declarator: (identifier) @lexical.parameter)

;; --- Local variable declarations ---
(declaration
  declarator: (init_declarator
    declarator: (identifier) @lexical.local))

;; --- For-loop initializer (for (int i = 0; ...)) ---
(for_statement
  initializer: (declaration
    declarator: (init_declarator
      declarator: (identifier) @lexical.local)))

;; --- Range-based for (for (auto x : ...)) ---
(for_statement
  initializer: (declaration
    declarator: (structured_binding_declarator
      (identifier) @lexical.local)))

;; --- Catch variable (catch (exception& e)) ---
(catch_clause
  (parameter_declaration
    declarator: (identifier) @lexical.catch_variable))

;; --- Lambda parameter ---
(lambda_expression
  (parameter_declaration
    declarator: (identifier) @lexical.parameter))
