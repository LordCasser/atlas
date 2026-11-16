;; Normalize identifier nodes through their declarator chain. This retains
;; shadowing from unsupported types (arrays, multiple pointers and structured
;; bindings) without capturing identifiers in types, bounds or initializers.
(identifier) @lexical.declarator
