//! Правила языков: грамматика, запрос тегов, вид символа по узлу, контейнеры `qual`, публичность.
//!
//! Запрос языка — встроенный `TAGS_QUERY` грамматики (TypeScript/TSX — ещё и JavaScript: грамматика
//! TS наследует JS) плюс дополнения `queries/<язык>.scm`; у C# — только свой запрос.

use std::sync::OnceLock;

use tree_sitter::{Language, Node, Query};

use crate::proto::{Lang, RefKind, SymbolKind};

/// Скомпилированная грамматика и запрос языка. Компиляция запроса — единицы–десятки миллисекунд
/// (release: Rust 15, TypeScript 31, C# 50 мс; только определения — 3, 23, 11 мс; замер журнала
/// M5.6), поэтому один раз на процесс.
pub(super) struct Spec {
    pub language: Language,
    pub query: Query,
}

/// Полный запрос (`extract`) или только определения (`symbol_hashes` для хуков: быстрее
/// компилируется, ссылки не нужны).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Full,
    Defs,
}

type Slot = OnceLock<Result<Spec, String>>;
static FULL: [Slot; 8] = [const { OnceLock::new() }; 8];
static DEFS: [Slot; 8] = [const { OnceLock::new() }; 8];

/// Грамматика и запрос языка; ошибка компиляции запроса — строка (тест ловит её заранее).
pub(super) fn spec(lang: Lang, mode: Mode) -> Result<&'static Spec, &'static str> {
    let slot = match mode {
        Mode::Full => &FULL[lang as usize],
        Mode::Defs => &DEFS[lang as usize],
    };
    slot.get_or_init(|| {
        let language = language(lang);
        let source = match mode {
            Mode::Full => query_source(lang),
            Mode::Defs => definitions_only(&query_source(lang)),
        };
        Query::new(&language, &source)
            .map(|query| Spec { language, query })
            .map_err(|e| format!("{lang}: {e}"))
    })
    .as_ref()
    .map_err(String::as_str)
}

fn language(lang: Lang) -> Language {
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

fn query_source(lang: Lang) -> String {
    let parts: &[&str] = match lang {
        Lang::Rust => &[
            tree_sitter_rust::TAGS_QUERY,
            include_str!("../../queries/rust.scm"),
        ],
        Lang::TypeScript | Lang::Tsx => &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
            include_str!("../../queries/typescript.scm"),
        ],
        Lang::JavaScript => &[
            tree_sitter_javascript::TAGS_QUERY,
            include_str!("../../queries/javascript.scm"),
        ],
        Lang::Python => &[
            tree_sitter_python::TAGS_QUERY,
            include_str!("../../queries/python.scm"),
        ],
        Lang::Go => &[
            tree_sitter_go::TAGS_QUERY,
            include_str!("../../queries/go.scm"),
        ],
        Lang::Java => &[
            tree_sitter_java::TAGS_QUERY,
            include_str!("../../queries/java.scm"),
        ],
        Lang::CSharp => &[include_str!("../../queries/csharp.scm")],
    };
    parts
        .iter()
        .map(|p| strip_doc_captures(p))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Убирает из встроенных запросов захват документирующих комментариев (`(comment)* @doc .` и
/// предикаты `#strip!`/`#select-adjacent!`/`#set-adjacent!` над `@doc`): экстрактор их не берёт,
/// а квантификатор перед якорем дорог при компиляции (TypeScript: 44 → 31 мс). Те же определения
/// без комментария совпадают как и раньше — проверяют эталоны.
fn strip_doc_captures(query: &str) -> String {
    query
        .lines()
        .filter(|l| {
            let t = l.trim();
            !(t == "(comment)* @doc"
                || t == "."
                || t.starts_with("(#strip! @doc")
                || t.starts_with("(#select-adjacent! @doc")
                || t.starts_with("(#set-adjacent! @doc"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Только шаблоны с `@definition.*`: верхнеуровневые S-выражения запроса (скобки с учётом строк
/// и комментариев `;`), шаблон заканчивается переводом строки на нулевой глубине.
fn definitions_only(query: &str) -> String {
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
            if cur.contains("@definition.") {
                out.push_str(&cur);
            }
            cur.clear();
        }
    }
    out
}

/// Вид символа по виду узла `@definition.*` (`cap` — суффикс захвата, запасной путь); `None` —
/// узел не символ.
pub(super) fn def_kind(lang: Lang, node: Node, cap: &str) -> Option<SymbolKind> {
    use SymbolKind as K;
    let kind = match (lang, node.kind()) {
        (Lang::Rust, "struct_item" | "union_item") => K::Struct,
        (Lang::Rust, "enum_item") => K::Enum,
        (Lang::Rust, "type_item" | "associated_type") => K::Type,
        (Lang::Rust, "trait_item") => K::Trait,
        (Lang::Rust, "mod_item") => K::Module,
        (Lang::Rust, "macro_definition") => K::Macro,
        (Lang::Rust, "function_item") => K::Function,
        (Lang::Rust, "function_signature_item") => K::Method,
        (Lang::Rust, "const_item" | "static_item") => K::Const,
        (Lang::Go, "type_spec") => match node.child_by_field_name("type").map(|t| t.kind()) {
            Some("struct_type") => K::Struct,
            Some("interface_type") => K::Interface,
            _ => K::Type,
        },
        (Lang::CSharp, "record_declaration") => {
            let mut c = node.walk();
            let is_struct = node.children(&mut c).any(|ch| ch.kind() == "struct");
            if is_struct { K::Struct } else { K::Class }
        }
        (Lang::CSharp, "delegate_declaration") => K::Type,
        (_, "class_declaration" | "class" | "abstract_class_declaration" | "class_definition") => {
            K::Class
        }
        (_, "record_declaration") => K::Class,
        (_, "struct_declaration") => K::Struct,
        (_, "interface_declaration" | "annotation_type_declaration") => K::Interface,
        (_, "enum_declaration") => K::Enum,
        (_, "type_alias_declaration" | "type_alias") => K::Type,
        (
            _,
            "internal_module"
            | "module"
            | "namespace_declaration"
            | "file_scoped_namespace_declaration",
        ) => K::Module,
        (
            _,
            "method_definition"
            | "method_signature"
            | "abstract_method_signature"
            | "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "method_elem"
            | "field_definition"
            | "public_field_definition",
        ) => K::Method,
        (
            _,
            "function_declaration"
            | "function_expression"
            | "generator_function"
            | "generator_function_declaration"
            | "function_signature"
            | "variable_declarator"
            | "assignment_expression"
            | "pair"
            | "function_definition",
        ) => K::Function,
        (Lang::Python, "assignment") | (_, "const_spec" | "export_statement") => K::Const,
        _ => match cap {
            "function" => K::Function,
            "method" => K::Method,
            "class" => K::Class,
            "interface" => K::Interface,
            "module" => K::Module,
            "macro" => K::Macro,
            "constant" => K::Const,
            "type" => K::Type,
            "struct" => K::Struct,
            "enum" => K::Enum,
            _ => return None,
        },
    };
    Some(kind)
}

/// Контейнер `qual`: имя (части через `::`) и признак «внутри — методы» (класс, `impl`, `trait`;
/// не модуль и не пространство имён). `None` — узел не контейнер.
pub(super) fn container(lang: Lang, node: Node, src: &[u8]) -> Option<(Vec<String>, bool)> {
    let class_like = match (lang, node.kind()) {
        (Lang::Rust, "impl_item") => {
            let ty = node.child_by_field_name("type")?;
            return rust_type_path(ty, src).map(|p| (p, true));
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

/// Публичность (§3): Rust — `visibility_modifier`; TS/JS — предок `export_statement`; Go —
/// заглавная буква; Java/C# — модификатор `public`; Python — `None`.
pub(super) fn exported(lang: Lang, node: Node, name: &str, src: &[u8]) -> Option<bool> {
    let mut c = node.walk();
    Some(match lang {
        Lang::Python => return None,
        Lang::Go => name.chars().next().is_some_and(char::is_uppercase),
        Lang::Rust => node
            .children(&mut c)
            .any(|ch| ch.kind() == "visibility_modifier"),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            let mut cur = Some(node);
            let mut found = false;
            while let Some(n) = cur {
                if n.kind() == "export_statement" {
                    found = true;
                    break;
                }
                cur = n.parent();
            }
            found
        }
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

/// Путь типа `impl` Rust без обобщений: `Pair<A, B>` → `Pair`, `net::Client` → `net::Client`.
fn rust_type_path(node: Node, src: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" => {
            let parts = name_parts(node, src);
            (!parts.is_empty()).then_some(parts)
        }
        "generic_type" | "reference_type" => rust_type_path(node.child_by_field_name("type")?, src),
        _ => None,
    }
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
