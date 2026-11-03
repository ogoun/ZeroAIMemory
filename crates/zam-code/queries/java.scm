; Дополнения ZeroAIMemory к tags.scm tree-sitter-java (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).

(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.class
(annotation_type_declaration name: (identifier) @name) @definition.interface
(constructor_declaration name: (identifier) @name) @definition.method
(compact_constructor_declaration name: (identifier) @name) @definition.method
(annotation_type_element_declaration name: (identifier) @name) @definition.method

(import_declaration (scoped_identifier name: (identifier) @name)) @reference.import

; типы (имена классов Java — identifier, поэтому определения сюда не попадают)
(type_identifier) @name @reference.type

(type_list (generic_type (type_identifier) @name)) @reference.implementation
(superclass (generic_type (type_identifier) @name)) @reference.implementation
(object_creation_expression
  type: (generic_type (type_identifier) @name)) @reference.call
