; Дополнения ZeroAIMemory к tags.scm tree-sitter-rust (MIT, deploy/licenses/TREE-SITTER-QUERIES-MIT.txt).
; Только ссылки (@reference.*): определения распознаёт обход дерева (src/extract/lang.rs).

; вызов по пути: Store::open() — call `open` и тип `Store`
(call_expression
  function: (scoped_identifier
    path: (identifier) @name)) @reference.type
(call_expression
  function: (scoped_identifier
    name: (identifier) @name)) @reference.call
(call_expression
  function: (generic_function
    function: (identifier) @name)) @reference.call
(call_expression
  function: (generic_function
    function: (scoped_identifier name: (identifier) @name))) @reference.call

; impl fmt::Display for T, impl From<X> for E
(impl_item
  trait: (scoped_type_identifier name: (type_identifier) @name)) @reference.implementation
(impl_item
  trait: (generic_type type: (type_identifier) @name)) @reference.implementation
(impl_item
  trait: (generic_type type: (scoped_type_identifier name: (type_identifier) @name))) @reference.implementation

; типы в сигнатурах и телах (имя определения отбрасывает экстрактор)
(type_identifier) @name @reference.type

; use x::Db; use y::{Conn, z::Pool, w as W};
(use_declaration argument: (identifier) @name) @reference.import
(use_declaration argument: (scoped_identifier name: (identifier) @name)) @reference.import
(use_list (identifier) @name) @reference.import
(use_list (scoped_identifier name: (identifier) @name)) @reference.import
(use_as_clause path: (identifier) @name) @reference.import
(use_as_clause path: (scoped_identifier name: (identifier) @name)) @reference.import
