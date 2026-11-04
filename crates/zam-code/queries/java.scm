; Дополнения ZeroAIMemory к tags.scm tree-sitter-java (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).
; Только ссылки (@reference.*): определения распознаёт обход дерева (src/extract/lang.rs).

(import_declaration (scoped_identifier name: (identifier) @name)) @reference.import

; типы (имена классов Java — identifier, поэтому определения сюда не попадают)
(type_identifier) @name @reference.type

(type_list (generic_type (type_identifier) @name)) @reference.implementation
(superclass (generic_type (type_identifier) @name)) @reference.implementation
(object_creation_expression
  type: (generic_type (type_identifier) @name)) @reference.call
