;; Ruby dataflow builder captures: parameters, assignments, returns,
;; call targets, call args, field access, implicit return,
;; instance/class/global variables, hash/index access

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
(simple_symbol) @df.literal

;; --- Identifier uses (variable references) ---
(identifier) @df.identifier_use

;; ── Ruby dataflow additions (§2.12) ──────────────────────────

;; Implicit return: last expression in method body
;; Trailing `.` matches only the last child of body_statement.
(body_statement
  (_) @df.implicit_return
  .)

;; Instance variable: @x
(instance_variable) @df.assign_target

;; Class variable: @@x
(class_variable) @df.assign_target

;; Global variable: $x
(global_variable) @df.assign_target

;; Hash/index access: params[:name] (parsed as element_reference, not call)
(element_reference
  object: (_) @df.receiver
  .
  (_) @df.field_name)
