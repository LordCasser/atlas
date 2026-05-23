;; Python dataflow builder captures: parameters, assignments, returns, call targets,
;; call arguments, member access, literals

;; --- Function parameters ---
;; def f(x, y, z):
(parameters (identifier) @df.parameter)

;; Default parameter: def f(x=1)
(default_parameter (identifier) @df.parameter)

;; Typed parameter: def f(x: int)
(typed_parameter (identifier) @df.parameter)

;; Typed default parameter: def f(x: int = 1)
(typed_default_parameter (identifier) @df.parameter)

;; List parameter: def f(*args)
(list_splat_pattern (identifier) @df.parameter)

;; Dict parameter: def f(**kwargs)
(dictionary_splat_pattern (identifier) @df.parameter)

;; --- Assignments ---
;; x = expr
(assignment
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Return statements ---
(return_statement (_) @df.return_value)

;; --- Call targets ---
;; Direct call: func(args)
(call
  function: (identifier) @df.call_target)

;; Method call: obj.method(args)
(call
  function: (attribute
    attribute: (identifier) @df.call_target))

;; --- Call arguments ---
(argument_list (_) @df.call_arg)

;; --- Member access chains ---
;; obj.attr — captured for field load dataflow
(attribute
  object: (_) @df.receiver
  attribute: (identifier) @df.field_name)

;; --- Literals ---
(string) @df.literal
(integer) @df.literal
(float) @df.literal
(true) @df.literal
(false) @df.literal
(none) @df.literal

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use
