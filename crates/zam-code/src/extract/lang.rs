//! Правила языков: грамматика, запрос ссылок, определения по виду узла, контейнеры `qual`,
//! публичность.
//!
//! **Определения** распознаются обходом дерева по таблице видов узлов ([`defs_of`]) — без
//! `Query`: компиляция запроса стоила хуку 14–24 мс (C#, TypeScript), а правила `tags.scm`
//! повторены здесь явно (шаблоны `@definition.*` встроенных запросов — источник правил).
//! **Ссылки** — шаблоны `@reference.*` встроенного `TAGS_QUERY` грамматики (TypeScript/TSX — ещё
//! и JavaScript: грамматика TS наследует JS) плюс `queries/<язык>.scm`; у C# — только свой запрос.

use std::collections::HashSet;
use std::sync::OnceLock;

use tree_sitter::{Language, Node, Query};

use crate::proto::{Lang, RefKind, SymbolKind};

/// Грамматика языка (дёшево: таблица из статической памяти грамматики).
pub(super) fn language(lang: Lang) -> Language {
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
    }
}

static REFS: [OnceLock<Result<Query, String>>; 8] = [const { OnceLock::new() }; 8];

/// Запрос ссылок языка, один раз на процесс; ошибка компиляции — строка (тесты ловят заранее).
pub(super) fn refs_query(lang: Lang) -> Result<&'static Query, &'static str> {
    REFS[lang as usize]
        .get_or_init(|| {
            Query::new(&language(lang), &refs_source(lang)).map_err(|e| format!("{lang}: {e}"))
        })
        .as_ref()
        .map_err(String::as_str)
}

fn refs_source(lang: Lang) -> String {
    let (builtin, own): (&[&str], &str) = match lang {
        Lang::Rust => (
            &[tree_sitter_rust::TAGS_QUERY],
            include_str!("../../queries/rust.scm"),
        ),
        Lang::TypeScript | Lang::Tsx => (
            &[
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY,
            ],
            include_str!("../../queries/typescript.scm"),
        ),
        Lang::JavaScript => (
            &[tree_sitter_javascript::TAGS_QUERY],
            include_str!("../../queries/javascript.scm"),
        ),
        Lang::Python => (
            &[tree_sitter_python::TAGS_QUERY],
            include_str!("../../queries/python.scm"),
        ),
        Lang::Go => (
            &[tree_sitter_go::TAGS_QUERY],
            include_str!("../../queries/go.scm"),
        ),
        Lang::Java => (
            &[tree_sitter_java::TAGS_QUERY],
            include_str!("../../queries/java.scm"),
        ),
        Lang::CSharp => (&[], include_str!("../../queries/csharp.scm")),
    };
    let mut out: String = builtin.iter().map(|q| references_only(q)).collect();
    out.push_str(own);
    out
}

/// Только шаблоны с `@reference.*`: верхнеуровневые S-выражения запроса (скобки с учётом строк
/// и комментариев `;`), шаблон заканчивается переводом строки на нулевой глубине. Шаблоны
/// определений (с захватом документирующих комментариев `(comment)* @doc`, дорогим при
/// компиляции) и шаблоны без захвата вида отбрасываются.
pub(super) fn references_only(query: &str) -> String {
    let mut out = String::new();
    let mut cur = String::new();
    let (mut depth, mut in_str, mut esc, mut in_comment) = (0i32, false, false, false);
    for c in query.chars().chain(std::iter::once('\n')) {
        if in_comment {
            if c != '\n' {
                continue;
            }
            in_comment = false;
        } else if in_str {
            cur.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            ';' => {
                in_comment = true;
                continue;
            }
            '"' => in_str = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        cur.push(c);
        if depth == 0 && c == '\n' {
            if cur.contains("@reference.") && !cur.contains("@definition.") {
                out.push_str(&cur);
            }
            cur.clear();
        }
    }
    out
}

/// Где стоит узел: предки, нужные правилам определений (без `Node::parent()`).
#[derive(Clone, Copy)]
pub(super) struct Place<'a> {
    pub parent: Option<&'a str>,
    pub grand: Option<&'a str>,
    /// Внутри функции или тела класса: литералы объектов здесь символов не дают.
    pub in_body: bool,
}

/// Определения узла: `(узел имени, вид)`; обычно одно, у `const a, b` Go — по имени.
pub(super) fn defs_of<'t>(
    lang: Lang,
    node: Node<'t>,
    at: Place,
    src: &[u8],
) -> Vec<(Node<'t>, SymbolKind)> {
    use SymbolKind as K;
    let name = || node.child_by_field_name("name");
    let one = |kind: SymbolKind| name().map(|n| vec![(n, kind)]).unwrap_or_default();
    match lang {
        Lang::Rust => match node.kind() {
            "struct_item" | "union_item" => one(K::Struct),
            "enum_item" => one(K::Enum),
            "type_item" | "associated_type" => one(K::Type),
            "trait_item" => one(K::Trait),
            "mod_item" => one(K::Module),
            "macro_definition" => one(K::Macro),
            "function_item" => one(K::Function),
            "function_signature_item" => one(K::Method),
            "const_item" | "static_item" => one(K::Const),
            _ => vec![],
        },
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => js_defs(lang, node, at, src),
        Lang::Python => match node.kind() {
            "class_definition" => one(K::Class),
            "function_definition" => one(K::Function),
            // константа модуля: `X = 1` верхнего уровня
            "assignment"
                if at.parent == Some("expression_statement") && at.grand == Some("module") =>
            {
                field_of_kind(node, "left", &["identifier"])
                    .map(|n| vec![(n, K::Const)])
                    .unwrap_or_default()
            }
            _ => vec![],
        },
        Lang::Go => match node.kind() {
            "function_declaration" => one(K::Function),
            "method_declaration" | "method_elem" => one(K::Method),
            "type_alias" => one(K::Type),
            "type_spec" => match node.child_by_field_name("type").map(|t| t.kind()) {
                Some("struct_type") => one(K::Struct),
                Some("interface_type") => one(K::Interface),
                _ => one(K::Type),
            },
            "const_spec"
                if at.parent == Some("const_declaration") && at.grand == Some("source_file") =>
            {
                let mut c = node.walk();
                node.children_by_field_name("name", &mut c)
                    .map(|n| (n, K::Const))
                    .collect()
            }
            _ => vec![],
        },
        Lang::Java => match node.kind() {
            "class_declaration" | "record_declaration" => one(K::Class),
            "interface_declaration" | "annotation_type_declaration" => one(K::Interface),
            "enum_declaration" => one(K::Enum),
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration" => one(K::Method),
            _ => vec![],
        },
        Lang::CSharp => match node.kind() {
            "class_declaration" => one(K::Class),
            "record_declaration" => {
                let mut c = node.walk();
                let is_struct = node.children(&mut c).any(|ch| ch.kind() == "struct");
                one(if is_struct { K::Struct } else { K::Class })
            }
            "struct_declaration" => one(K::Struct),
            "interface_declaration" => one(K::Interface),
            "enum_declaration" => one(K::Enum),
            "delegate_declaration" => one(K::Type),
            "method_declaration" | "constructor_declaration" => one(K::Method),
            "namespace_declaration" | "file_scoped_namespace_declaration" => one(K::Module),
            _ => vec![],
        },
    }
}

/// Правила `tags.scm` JavaScript и TypeScript (TS — надмножество JS).
fn js_defs<'t>(lang: Lang, node: Node<'t>, at: Place, src: &[u8]) -> Vec<(Node<'t>, SymbolKind)> {
    use SymbolKind as K;
    let ts = lang != Lang::JavaScript;
    let is_fn = |n: Option<Node>| {
        n.is_some_and(|n| matches!(n.kind(), "arrow_function" | "function_expression"))
    };
    let pick =
        |n: Option<Node<'t>>, kind: SymbolKind| n.map(|n| vec![(n, kind)]).unwrap_or_default();
    let field = |f: &str, kinds: &[&str]| field_of_kind(node, f, kinds);
    match node.kind() {
        "function_declaration"
        | "generator_function_declaration"
        | "function_expression"
        | "generator_function" => pick(field("name", &["identifier"]), K::Function),
        "class_declaration" | "class" => pick(node.child_by_field_name("name"), K::Class),
        "method_definition" => {
            let n = field("name", &["property_identifier"]);
            let ctor = n.is_some_and(|n| {
                n.kind() == "property_identifier" && is_named(n, src, "constructor")
            });
            // метод литерала объекта — только у литералов верхнего уровня
            if ctor || (at.parent == Some("object") && at.in_body) {
                vec![]
            } else {
                pick(n, K::Method)
            }
        }
        "variable_declarator"
            if matches!(
                at.parent,
                Some("lexical_declaration" | "variable_declaration")
            ) && is_fn(node.child_by_field_name("value")) =>
        {
            pick(field("name", &["identifier"]), K::Function)
        }
        "assignment_expression" if is_fn(node.child_by_field_name("right")) => {
            let left = node.child_by_field_name("left");
            let name = match left.map(|l| l.kind()) {
                Some("identifier") => left,
                Some("member_expression") => {
                    left.and_then(|l| field_of_kind(l, "property", &["property_identifier"]))
                }
                _ => None,
            };
            pick(name, K::Function)
        }
        "pair" if !at.in_body && is_fn(node.child_by_field_name("value")) => {
            pick(field("key", &["property_identifier"]), K::Function)
        }
        "field_definition" if is_fn(node.child_by_field_name("value")) => {
            pick(field("property", &["property_identifier"]), K::Method)
        }
        "export_statement" => {
            // `export x = <значение>` (шаблон `@definition.constant` встроенного запроса JS)
            let Some(v) = node.child_by_field_name("value") else {
                return vec![];
            };
            if v.kind() != "assignment_expression" {
                return vec![];
            }
            let right = v.child_by_field_name("right").map(|r| r.kind());
            let simple = matches!(
                right,
                Some(
                    "number"
                        | "string"
                        | "identifier"
                        | "undefined"
                        | "null"
                        | "new_expression"
                        | "binary_expression"
                        | "call_expression"
                )
            );
            if simple {
                pick(field_of_kind(v, "left", &["identifier"]), K::Const)
            } else {
                vec![]
            }
        }
        "public_field_definition" if ts && is_fn(node.child_by_field_name("value")) => {
            pick(field("name", &["property_identifier"]), K::Method)
        }
        "abstract_class_declaration" if ts => pick(node.child_by_field_name("name"), K::Class),
        "function_signature" if ts => pick(field("name", &["identifier"]), K::Function),
        "method_signature" | "abstract_method_signature" if ts => {
            pick(field("name", &["property_identifier"]), K::Method)
        }
        "interface_declaration" if ts => pick(node.child_by_field_name("name"), K::Interface),
        "type_alias_declaration" if ts => pick(node.child_by_field_name("name"), K::Type),
        "enum_declaration" if ts => pick(node.child_by_field_name("name"), K::Enum),
        "internal_module" if ts => pick(node.child_by_field_name("name"), K::Module),
        "module" if ts => pick(field("name", &["identifier", "string"]), K::Module),
        _ => vec![],
    }
}

fn field_of_kind<'t>(node: Node<'t>, field: &str, kinds: &[&str]) -> Option<Node<'t>> {
    node.child_by_field_name(field)
        .filter(|n| kinds.contains(&n.kind()))
}

fn is_named(n: Node, src: &[u8], s: &str) -> bool {
    src.get(n.start_byte()..n.end_byte()) == Some(s.as_bytes())
}

/// Контейнер `qual`: имя (части через `::`) и признак «внутри — методы» (класс, `impl`, `trait`;
/// не модуль и не пространство имён). `None` — узел не контейнер.
pub(super) fn container(lang: Lang, node: Node, src: &[u8]) -> Option<(Vec<String>, bool)> {
    let class_like = match (lang, node.kind()) {
        (Lang::Rust, "impl_item") => {
            // `impl` — всегда контейнер: тип без обобщений и ссылок, иначе текст типа
            let ty = node.child_by_field_name("type")?;
            return Some((rust_type_path(ty, src), true));
        }
        (Lang::Rust, "trait_item") => true,
        (Lang::Rust, "mod_item") => false,
        (Lang::TypeScript | Lang::Tsx, "internal_module" | "module") => false,
        (
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript,
            "class_declaration" | "class" | "abstract_class_declaration" | "interface_declaration",
        ) => true,
        (Lang::Python, "class_definition") => true,
        (Lang::Go, "type_spec") => true,
        (
            Lang::Java,
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration",
        ) => true,
        (
            Lang::CSharp,
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration",
        ) => true,
        _ => return None,
    };
    let name = node.child_by_field_name("name")?;
    let parts = name_parts(name, src);
    (!parts.is_empty()).then_some((parts, class_like))
}

/// Узлы-функции: определение внутри них — не метод контейнера выше (вложенная функция).
pub(super) fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "closure_expression"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "generator_function"
            | "generator_function_declaration"
            | "method_definition"
            | "function_definition"
            | "lambda"
            | "method_declaration"
            | "constructor_declaration"
            | "lambda_expression"
            | "local_function_statement"
    )
}

/// Граница экспорта TS/JS: потомки этих узлов не наследуют `export` предка (вложенные функции,
/// блоки, тело пространства имён).
pub(super) fn is_export_boundary(kind: &str) -> bool {
    is_function_like(kind)
        || matches!(
            kind,
            "statement_block" | "internal_module" | "module" | "class_static_block"
        )
}

/// Тип получателя метода Go: `func (s *Store[T]) Open` → `Store`.
pub(super) fn go_receiver(node: Node, src: &[u8]) -> Option<String> {
    let recv = node.child_by_field_name("receiver")?;
    let mut stack = vec![recv];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            return Some(text(n, src));
        }
        let mut c = n.walk();
        let children: Vec<Node> = n.named_children(&mut c).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

/// Публичность (§3): Rust — `visibility_modifier`; TS/JS — `export` контекста обхода; Go —
/// заглавная буква; Java/C# — модификатор `public`; Python — `None`.
pub(super) fn exported(
    lang: Lang,
    node: Node,
    name: &str,
    src: &[u8],
    ts_export: bool,
) -> Option<bool> {
    let mut c = node.walk();
    Some(match lang {
        Lang::Python => return None,
        Lang::Go => name.chars().next().is_some_and(char::is_uppercase),
        Lang::Rust => node
            .children(&mut c)
            .any(|ch| ch.kind() == "visibility_modifier"),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => ts_export,
        Lang::Java => node
            .children(&mut c)
            .filter(|ch| ch.kind() == "modifiers")
            .any(|m| {
                let mut mc = m.walk();
                m.children(&mut mc).any(|x| x.kind() == "public")
            }),
        Lang::CSharp => node
            .children(&mut c)
            .any(|ch| ch.kind() == "modifier" && ch.utf8_text(src).is_ok_and(|t| t == "public")),
    })
}

/// Операнд доступа к члену, чьё имя — имя вызова (M6 §12а): выражение, простой идентификатор
/// или цепочка доступа из одних идентификаторов (`os.path`, `System.Console`) — её левый
/// идентификатор (тексты — для правила «квалификатора»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Operand {
    Expr,
    Ident(String),
    Chain(String),
}

/// Поля `(имя члена, операнд)` узла доступа к члену языка (без Java `method_invocation`).
fn access_fields(lang: Lang, kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match (lang, kind) {
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "member_expression") => {
            ("property", "object")
        }
        (Lang::Python, "attribute") => ("attribute", "object"),
        (Lang::Go, "selector_expression") => ("field", "operand"),
        (Lang::Java, "field_access") => ("field", "object"),
        (Lang::CSharp, "member_access_expression") => ("name", "expression"),
        _ => return None,
    })
}

/// Левый идентификатор цепочки доступа из одних идентификаторов (`os.path` → `os`,
/// `java.util.Objects` → `java`); иное выражение (вызов, индекс, `this`) — `None`.
fn chain_left(lang: Lang, mut node: Node, src: &[u8]) -> Option<String> {
    loop {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            return Some(text(node, src));
        }
        let (name_field, obj_field) = access_fields(lang, node.kind())?;
        let name = node.child_by_field_name(name_field)?;
        if !matches!(
            name.kind(),
            "identifier" | "property_identifier" | "field_identifier"
        ) {
            return None;
        }
        node = node.child_by_field_name(obj_field)?;
    }
}

/// Имя Go-пакета по пути импорта: последний сегмент без `.vN` и без префикса `go-`; сегмент
/// мажорной версии `/vN` — предыдущий (`gopkg.in/yaml.v3` → `yaml`, `github.com/x/go-redis/v9` →
/// `redis`).
pub(super) fn go_package_name(path: &str) -> Option<String> {
    let is_ver = |s: &str| {
        s.strip_prefix('v')
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    };
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut last = *segs.last()?;
    if is_ver(last) && segs.len() > 1 {
        last = segs[segs.len() - 2];
    }
    if let Some((base, v)) = last.rsplit_once('.')
        && is_ver(v)
    {
        last = base;
    }
    let last = last.strip_prefix("go-").unwrap_or(last);
    (!last.is_empty()).then(|| last.to_string())
}

/// Имена-квалификаторы, которые вводит узел импорта (M6 §12а, правило п. 2(б)): псевдонимы
/// (`import numpy as np`, `import * as ns`, `import { a as b }`, Go `f "fmt"`, C#
/// `using F = A.B`), имя Go-пакета по пути, `const fs = require('fs')`, левый сегмент
/// импортируемого пути (`import os.path` → `os`, `using System.Text` → `System`, Java
/// `import java.util.Objects` → `java`).
pub(super) fn import_qualifiers(lang: Lang, node: Node, src: &[u8], out: &mut HashSet<String>) {
    fn leftmost<'t>(mut n: Node<'t>, scope: &str) -> Node<'t> {
        while let Some(s) = n.child_by_field_name(scope) {
            n = s;
        }
        n
    }
    let mut add = |n: Option<Node>| {
        if let Some(n) = n {
            out.insert(text(n, src));
        }
    };
    match (lang, node.kind()) {
        (Lang::Python, "aliased_import") => add(node.child_by_field_name("alias")),
        (Lang::Python, "import_statement") => {
            let mut c = node.walk();
            for n in node.children_by_field_name("name", &mut c) {
                if n.kind() == "dotted_name" {
                    add(n.named_child(0));
                }
            }
        }
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "namespace_import") => {
            let mut c = node.walk();
            let last = node
                .named_children(&mut c)
                .filter(|n| n.kind() == "identifier")
                .last();
            add(last);
        }
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "import_specifier") => {
            add(node.child_by_field_name("alias"))
        }
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "variable_declarator") => {
            let is_require = node
                .child_by_field_name("value")
                .filter(|v| v.kind() == "call_expression")
                .and_then(|v| v.child_by_field_name("function"))
                .is_some_and(|f| f.kind() == "identifier" && text(f, src) == "require");
            if is_require {
                add(node
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "identifier"));
            }
        }
        (Lang::Go, "import_spec") => match node.child_by_field_name("name") {
            Some(n) if n.kind() == "package_identifier" => add(Some(n)),
            Some(_) => {}
            None => {
                let path = node.child_by_field_name("path").map(|p| text(p, src));
                if let Some(name) = path
                    .as_deref()
                    .map(|p| p.trim_matches(|c| c == '"' || c == '`'))
                    .and_then(go_package_name)
                {
                    out.insert(name);
                }
            }
        },
        (Lang::Java, "import_declaration") => {
            let mut c = node.walk();
            let path = node
                .named_children(&mut c)
                .find(|n| matches!(n.kind(), "scoped_identifier" | "identifier"));
            add(path.map(|p| leftmost(p, "scope")));
        }
        (Lang::CSharp, "using_directive") => {
            add(node.child_by_field_name("name"));
            let mut c = node.walk();
            let path = node
                .named_children(&mut c)
                .find(|n| matches!(n.kind(), "qualified_name" | "identifier"));
            add(path.map(|p| leftmost(p, "qualifier")));
        }
        _ => {}
    }
}

/// Узел доступа к члену: `(узел имени члена, операнд)`. Rust `field_expression.field`, TS/TSX/JS
/// `member_expression.property`, Python `attribute.attribute`, Go `selector_expression.field`, Java
/// `method_invocation.name` при поле `object`, C# `member_access_expression.name` (у
/// `generic_name` — его идентификатор). Иные узлы — `None`.
pub(super) fn member_access<'t>(
    lang: Lang,
    node: Node<'t>,
    src: &[u8],
) -> Option<(Node<'t>, Operand)> {
    let (name_field, obj_field) = match (lang, node.kind()) {
        (Lang::Rust, "field_expression") => ("field", "value"),
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "member_expression") => {
            ("property", "object")
        }
        (Lang::Python, "attribute") => ("attribute", "object"),
        (Lang::Go, "selector_expression") => ("field", "operand"),
        (Lang::Java, "method_invocation") => ("name", "object"),
        (Lang::CSharp, "member_access_expression") => ("name", "expression"),
        _ => return None,
    };
    let mut name = node.child_by_field_name(name_field)?;
    if name.kind() == "generic_name" {
        name = name.named_child(0).filter(|n| n.kind() == "identifier")?;
    }
    let obj = node.child_by_field_name(obj_field)?;
    // Rust: путь `T::f` — `scoped_identifier`, а `field_expression` — всегда значение-получатель
    let op = if lang == Lang::Rust {
        Operand::Expr
    } else if matches!(obj.kind(), "identifier" | "type_identifier") {
        Operand::Ident(text(obj, src))
    } else {
        chain_left(lang, obj, src).map_or(Operand::Expr, Operand::Chain)
    };
    Some((name, op))
}

/// Вызов члена с операндом `op` — по получателю (M6 §12а): операнд не «квалификатор». Квалификатор —
/// простой идентификатор с заглавной буквы (тип или класс: `Math.max`, `Console.WriteLine`) или имя
/// импорта файла (пакет, модуль, псевдоним: `fmt.Println`, `np.array`), а также цепочка из одних
/// идентификаторов, чей левый — имя импорта (`os.path.join`, `System.Console.WriteLine`).
pub(super) fn is_recv(op: &Operand, imports: &HashSet<String>) -> bool {
    match op {
        Operand::Expr => true,
        Operand::Ident(t) => {
            !(t.chars().next().is_some_and(char::is_uppercase) || imports.contains(t.as_str()))
        }
        Operand::Chain(left) => !imports.contains(left.as_str()),
    }
}

/// Вид ссылки по суффиксу захвата `@reference.*` и виду узла.
pub(super) fn ref_kind(node: Node, cap: &str) -> RefKind {
    match cap {
        "call" | "send" => RefKind::Call,
        "implementation" | "interface" | "impl" => RefKind::Impl,
        "type" => RefKind::Type,
        "import" => RefKind::Import,
        "class" => match node.kind() {
            "new_expression" | "object_creation_expression" => RefKind::Call,
            "superclass" | "class_heritage" | "base_list" => RefKind::Impl,
            _ => RefKind::Type,
        },
        _ => RefKind::Other,
    }
}

/// Имя типа `impl` Rust циклом (без рекурсии: вложенность `&&&…T` не ограничена): `Pair<A, B>` →
/// `Pair`, `net::Client` → `net::Client`, `&mut S` → `S`, `Box<dyn Tr>` → `Box`, `dyn Tr + 'a` →
/// `Tr` (поле `trait`, первая граница не-время-жизни); иные (`[u8]`, `(A, B)`) — текст без
/// пробелов (до 128 символов, иначе `_`).
///
/// Известные ограничения (ревью задачи 9A, M6; тест `rust_impl_dyn_edge_cases_current_behavior`):
/// `dyn Fn(A, B)` — текст `Fn(A,B)` (у `function_type` нет имени-пути); скобки с `,` внутри
/// (`(dyn Fn(A, B) + 'a)`) от кортежа не отличаются — текст целиком; `impl dyn Error` рядом с
/// `trait Error` — методы обоих под `Error::…`, одноимённый второй получает `#2`.
fn rust_type_path(mut node: Node, src: &[u8]) -> Vec<String> {
    loop {
        match node.kind() {
            "type_identifier" | "scoped_type_identifier" => {
                let parts = name_parts(node, src);
                if !parts.is_empty() {
                    return parts;
                }
                break;
            }
            "generic_type" | "reference_type" | "pointer_type" | "higher_ranked_trait_bound" => {
                match node.child_by_field_name("type") {
                    Some(t) => node = t,
                    None => break,
                }
            }
            // `dyn Tr`, `impl Tr` — по полю `trait` (ревью задачи 1, M8)
            "dynamic_type" | "abstract_type" => match node.child_by_field_name("trait") {
                Some(t) => node = t,
                None => break,
            },
            // `(dyn Tr + 'a)` — скобки (у грамматики это `tuple_type` из одного типа без `,`)
            "tuple_type" | "parenthesized_type"
                if node.named_child_count() == 1 && !text(node, src).contains(',') =>
            {
                match node.named_child(0) {
                    Some(t) => node = t,
                    None => break,
                }
            }
            // `dyn Tr + 'a`, `dyn Tr + Send` — первая граница, не время жизни
            "bounded_type" => {
                let mut cur = node.walk();
                let first = node
                    .named_children(&mut cur)
                    .find(|c| c.kind() != "lifetime" && c.kind() != "use_bounds");
                match first {
                    Some(t) => node = t,
                    None => break,
                }
            }
            _ => break,
        }
    }
    let flat: String = text(node, src).split_whitespace().collect();
    let ok = !flat.is_empty()
        && flat.chars().count() <= 128
        && !flat.contains(['/', '#'])
        && !flat.chars().any(char::is_control);
    vec![if ok { flat } else { "_".to_string() }]
}

/// Текст узла (невалидный UTF-8 — с заменой).
pub(super) fn text(node: Node, src: &[u8]) -> String {
    let bytes = src
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default();
    String::from_utf8_lossy(bytes).into_owned()
}

/// Части имени: `App.Core`, `a::b`, `"external"` → `[App, Core]`, `[a, b]`, `[external]`;
/// пробелы убраны.
pub(super) fn name_parts(node: Node, src: &[u8]) -> Vec<String> {
    let raw = text(node, src);
    let trimmed = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    trimmed
        .split(['.', ':'])
        .map(|p| p.split_whitespace().collect::<String>())
        .filter(|p| !p.is_empty())
        .collect()
}
