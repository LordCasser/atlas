;; Ruby definitions query
;; Captures: class, module, method, singleton_method, constant, variable, field

;; Class definitions
(class name: (constant) @definition.class)

;; Module definitions
(module name: (constant) @definition.module)

;; Method definitions
(method name: (identifier) @definition.method)

;; Singleton method definitions (class methods)
(singleton_method name: (identifier) @definition.method)

;; Constant assignments
(assignment left: (constant) @definition.constant)

;; Instance variable assignments
(assignment left: (instance_variable) @definition.variable)

;; Class variable assignments
(assignment left: (class_variable) @definition.variable)

;; Global variable assignments
(assignment left: (global_variable) @definition.variable)

;; attr_reader / attr_writer / attr_accessor calls produce field definitions
(call
  method: (identifier) @_attr_name
  arguments: (argument_list (simple_symbol) @definition.field)
  (#match? @_attr_name "^(attr_reader|attr_writer|attr_accessor)$"))

(call
  method: (identifier) @_attr_name2
  arguments: (argument_list (string) @definition.field)
  (#match? @_attr_name2 "^(attr_reader|attr_writer|attr_accessor)$"))
