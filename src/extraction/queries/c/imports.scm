;; C imports query
;; Captures: #include directives (system vs local)

;; System includes: #include <stdio.h>
(preproc_include (system_lib_string) @import.module)

;; Local includes: #include "foo.h"
(preproc_include (string_literal) @import.include)
