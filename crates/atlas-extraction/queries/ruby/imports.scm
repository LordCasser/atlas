;; Ruby imports query
;; Captures: require, require_relative, include, extend, prepend

;; require / require_relative
(call
  method: (identifier) @_req_name
  arguments: (argument_list (string) @import.module)
  (#match? @_req_name "^(require|require_relative)$"))

;; include / extend / prepend
(call
  method: (identifier) @_inc_name
  arguments: (argument_list (constant) @import.module)
  (#match? @_inc_name "^(include|extend|prepend)$"))
