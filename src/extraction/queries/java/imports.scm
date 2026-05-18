;; Java imports query
;; Captures: single-type import, on-demand import, static import

;; Single type import: import foo.bar.Baz;
(import_declaration (scoped_identifier) @import.module)

;; On-demand import: import foo.bar.*;
(import_declaration (scoped_identifier) @import.module)

;; Static import: import static foo.bar.method;
(import_declaration "static" (scoped_identifier) @import.module)
