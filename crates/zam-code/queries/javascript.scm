; Дополнения ZeroAIMemory к tags.scm tree-sitter-javascript (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).

(field_definition
  property: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.method

(class_heritage (identifier) @name) @reference.implementation
(class_heritage (member_expression property: (property_identifier) @name)) @reference.implementation

(import_specifier name: (identifier) @name) @reference.import
(import_clause (identifier) @name) @reference.import
