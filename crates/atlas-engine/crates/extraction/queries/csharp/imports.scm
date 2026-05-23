;; C# imports query
;; Captures: using directives (namespaces and static imports)

;; Using namespace directives
(using_directive
  name: (identifier) @import.module)

;; Using qualified namespace (e.g. using System.Collections.Generic)
(using_directive
  (qualified_name) @import.module)
