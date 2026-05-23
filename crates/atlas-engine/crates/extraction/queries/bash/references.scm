;; Bash references query
;; Captures: command calls (as function calls)

;; Command invocations — first word is the command name
(command (word) @reference.call)
