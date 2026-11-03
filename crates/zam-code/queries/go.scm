; Дополнения ZeroAIMemory к tags.scm tree-sitter-go (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).

(type_alias name: (type_identifier) @name) @definition.type
(method_elem name: (field_identifier) @name) @definition.method
(source_file
  (const_declaration
    (const_spec name: (identifier) @name) @definition.constant))

(import_spec
  path: (interpreted_string_literal (interpreted_string_literal_content) @name)) @reference.import
