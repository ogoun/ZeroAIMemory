; Дополнения ZeroAIMemory к tags.scm tree-sitter-javascript и tree-sitter-typescript
; (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt); общие для TypeScript и TSX.
; Только ссылки (@reference.*): определения распознаёт обход дерева (src/extract/lang.rs).

; типы в аннотациях (имя определения отбрасывает экстрактор)
(type_identifier) @name @reference.type

(extends_clause value: (identifier) @name) @reference.implementation
(extends_clause value: (member_expression property: (property_identifier) @name)) @reference.implementation
(implements_clause (type_identifier) @name) @reference.implementation
(implements_clause (generic_type name: (type_identifier) @name)) @reference.implementation
(extends_type_clause type: (type_identifier) @name) @reference.implementation
(extends_type_clause type: (generic_type name: (type_identifier) @name)) @reference.implementation

(import_specifier name: (identifier) @name) @reference.import
(import_clause (identifier) @name) @reference.import
