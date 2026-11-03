; Дополнения ZeroAIMemory к tags.scm tree-sitter-python (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).

(import_from_statement name: (dotted_name (identifier) @name .)) @reference.import
(import_from_statement
  name: (aliased_import name: (dotted_name (identifier) @name .))) @reference.import
(import_statement name: (dotted_name (identifier) @name .)) @reference.import
(import_statement
  name: (aliased_import name: (dotted_name (identifier) @name .))) @reference.import

(type (identifier) @name) @reference.type
(generic_type (identifier) @name) @reference.type

(class_definition
  superclasses: (argument_list (identifier) @name)) @reference.implementation
(class_definition
  superclasses: (argument_list (attribute attribute: (identifier) @name))) @reference.implementation

(decorator (identifier) @name) @reference.call
(decorator (call function: (identifier) @name)) @reference.call
