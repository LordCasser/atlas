;; Bash imports query
;; Captures: word arguments to commands as best-effort include-like imports

;; Command with a word argument (may be source/include)
(command (word) @import.module)
