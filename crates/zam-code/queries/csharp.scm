; Запрос тегов ZeroAIMemory для C# (грамматика tree-sitter-c-sharp 0.23.5, MIT). Встроенный
; tags.scm крейта не используется: в нём нет структур, записей, конструкторов, делегатов и импортов.
; Вид символа определяет экстрактор по виду узла @definition.*, ссылки — по @reference.*.

(class_declaration name: (identifier) @name) @definition.class
(struct_declaration name: (identifier) @name) @definition.struct
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.class
(delegate_declaration name: (identifier) @name) @definition.type
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method
(namespace_declaration name: (_) @name) @definition.module
(file_scoped_namespace_declaration name: (_) @name) @definition.module

; вызовы и создание объектов
(invocation_expression function: (identifier) @name) @reference.call
(invocation_expression function: (generic_name (identifier) @name)) @reference.call
(invocation_expression
  function: (member_access_expression name: (identifier) @name)) @reference.call
(invocation_expression
  function: (member_access_expression name: (generic_name (identifier) @name))) @reference.call
(object_creation_expression type: (identifier) @name) @reference.call
(object_creation_expression type: (generic_name (identifier) @name)) @reference.call
(object_creation_expression type: (qualified_name name: (identifier) @name)) @reference.call

; наследование и реализация
(base_list (identifier) @name) @reference.implementation
(base_list (generic_name (identifier) @name)) @reference.implementation
(base_list (qualified_name name: (identifier) @name)) @reference.implementation

; using System.Text; using Foo = Bar.Baz;
(using_directive !name (identifier) @name) @reference.import
(using_directive (qualified_name name: (identifier) @name)) @reference.import

; типы параметров, переменных, полей, возвращаемых значений и аргументов обобщений
(parameter type: (identifier) @name) @reference.type
(parameter type: (generic_name (identifier) @name)) @reference.type
(parameter type: (qualified_name name: (identifier) @name)) @reference.type
(variable_declaration type: (identifier) @name) @reference.type
(variable_declaration type: (generic_name (identifier) @name)) @reference.type
(method_declaration returns: (identifier) @name) @reference.type
(method_declaration returns: (generic_name (identifier) @name)) @reference.type
(property_declaration type: (identifier) @name) @reference.type
(type_argument_list (identifier) @name) @reference.type
