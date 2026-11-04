; Дополнения ZeroAIMemory к tags.scm tree-sitter-javascript (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).
; Только ссылки (@reference.*): определения распознаёт обход дерева (src/extract/lang.rs).

(class_heritage (identifier) @name) @reference.implementation
(class_heritage (member_expression property: (property_identifier) @name)) @reference.implementation

(import_specifier name: (identifier) @name) @reference.import
(import_clause (identifier) @name) @reference.import
