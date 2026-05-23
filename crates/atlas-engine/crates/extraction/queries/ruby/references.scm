;; Ruby references query
;; Captures: method calls, constant references

;; Method calls
(call method: (identifier) @reference.call)

;; Constant references (non-assignment context)
(constant) @reference.type
