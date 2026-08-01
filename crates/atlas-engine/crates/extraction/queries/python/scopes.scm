; Python lexical namespaces. Ordinary statement blocks do not introduce a
; scope; comprehensions do have an isolated implicit namespace in Python 3.

(module) @scope.file

(function_definition) @scope.function

(lambda) @scope.function

(class_definition) @scope.class

(list_comprehension) @scope.comprehension
(dictionary_comprehension) @scope.comprehension
(set_comprehension) @scope.comprehension
(generator_expression) @scope.comprehension
