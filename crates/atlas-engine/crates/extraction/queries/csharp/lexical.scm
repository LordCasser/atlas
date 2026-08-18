;; C# lexical binding captures: parameters, locals, foreach vars, catch vars

;; --- Method/constructor parameters ---
(parameter
  name: (identifier) @lexical.parameter)

;; --- Local variable declarations (int x = 5, var s) ---
(local_declaration_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @lexical.local)))

;; --- Foreach loop variable (foreach (var x in ...)) ---
(foreach_statement
  (identifier) @lexical.local)

;; --- Catch clause variable (catch (Exception e)) ---
(catch_declaration
  (identifier) @lexical.catch_variable)

;; --- Lambda parameter (x => expr) ---
(lambda_expression
  (identifier) @lexical.parameter)

;; --- Pattern variables (input is Type value / switch arms) ---
(declaration_pattern
  name: (identifier) @lexical.pattern)

(recursive_pattern
  name: (identifier) @lexical.pattern)

(var_pattern
  name: (identifier) @lexical.pattern)
