;; Ruby dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access

;; --- Method parameters ---
(method_parameters
  (identifier) @df.parameter)
(optional_parameter
  name: (identifier) @df.parameter)

;; --- Assignments: x = expr ---
(assignment
  left: (identifier) @df.assign_target
  right: (_) @df.assign_value)

;; --- Return statements (Ruby: return is optional, captured when explicit) ---
(return
  (_) @df.return_value)

;; --- Call targets: method(args) ---
(call
  method: (identifier) @df.call_target)

;; --- Method calls: obj.method(args) ---
(call
  receiver: (_)
  method: (identifier) @df.call_target)

;; --- Call arguments ---
(argument_list
  (_) @df.call_arg)

;; --- Field access: obj.field ---
(call
  receiver: (_) @df.receiver
  method: (identifier) @df.field_name)

;; --- Literals ---
(string) @df.literal
(integer) @df.literal
(float) @df.literal
(true) @df.literal
(false) @df.literal
(nil) @df.literal
(symbol) @df.literal
