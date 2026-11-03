; Дополнения ZeroAIMemory к tags.scm tree-sitter-go (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).
; Только ссылки (@reference.*): определения распознаёт обход дерева (src/extract/lang.rs).

(import_spec
  path: (interpreted_string_literal (interpreted_string_literal_content) @name)) @reference.import
