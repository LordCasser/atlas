;; C++ imports query
;; Captures: #include directives (system vs local)

;; System includes: #include <iostream>
(preproc_include (system_lib_string) @import.module)

;; Local includes: #include "foo.h"
(preproc_include (string_literal) @import.include)

;; Using declarations
(using_declaration (identifier) @import.name)
