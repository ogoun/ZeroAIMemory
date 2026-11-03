//! Типы протокола карты кода (спецификация M5.6 §3–§6): общие для клиента `zam` и сервера.
//!
//! Строковые значения перечислений — дословно из спецификации: они уходят в JSON запросов
//! `/code/*` и в столбцы базы (`code_files.parse_state`, `code_symbols.kind`, …).

use serde::{Deserialize, Serialize};

/// Предел длины сигнатуры, символов (§3): обрезка по символу с `…`.
pub const SIG_MAX_CHARS: usize = 160;
/// Предел длины имени символа или ссылки, символов (§13).
pub const NAME_MAX_CHARS: usize = 256;
/// Предел длины квалифицированного имени, символов (§13).
pub const QUAL_MAX_CHARS: usize = 512;
/// Файлов в одном `PUT /code/files` (§5).
pub const UPLOAD_FILES_MAX: usize = 500;
/// Хэш удалённого файла: 64 нуля, как `zam-cli` `refs::MISSING_HASH` (очередь хуков M4.5).
pub const MISSING_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Перечисление со строковым видом из спецификации: `as_str`, `parse`, `ALL`.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $s)] $v),+
        }

        impl $name {
            /// Все значения в порядке объявления.
            pub const ALL: &'static [$name] = &[$($name::$v),+];

            /// Строковый вид (JSON, столбцы базы).
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$v => $s),+ }
            }

            /// Разбор строкового вида; регистр значим.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$v),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum! {
    /// Язык файла (таблица §3; JSX разбирается грамматикой JavaScript).
    Lang {
        Rust => "rust",
        TypeScript => "typescript",
        Tsx => "tsx",
        JavaScript => "javascript",
        Python => "python",
        Go => "go",
        Java => "java",
        CSharp => "csharp",
    }
}

str_enum! {
    /// Вид символа (§3).
    SymbolKind {
        Function => "function",
        Method => "method",
        Class => "class",
        Struct => "struct",
        Enum => "enum",
        Interface => "interface",
        Trait => "trait",
        Type => "type",
        Const => "const",
        Module => "module",
        Macro => "macro",
    }
}

str_enum! {
    /// Вид ссылки (§3).
    RefKind {
        Call => "call",
        Type => "type",
        Import => "import",
        Impl => "impl",
        Other => "other",
    }
}

str_enum! {
    /// Состояние разбора файла (§6).
    ParseState {
        Ok => "ok",
        /// Разобран, но символы или ссылки обрезаны пределом (`skip_reason = limit`).
        Partial => "partial",
        Skipped => "skipped",
    }
}

str_enum! {
    /// Причина пропуска или неполного разбора (§6, §12).
    SkipReason {
        TooLarge => "too_large",
        Minified => "minified",
        Binary => "binary",
        Timeout => "timeout",
        ParserCrash => "parser_crash",
        Limit => "limit",
        /// Язык не собран или выключен `code.languages` (ставит клиент).
        Lang => "lang",
    }
}

/// Расширение → язык (таблица §3), без учёта регистра; `None` — язык не поддерживается.
pub fn lang_of(path: &str) -> Option<Lang> {
    let file = path.rsplit(['/', '\\']).next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => Lang::Rust,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "py" | "pyi" => Lang::Python,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "cs" => Lang::CSharp,
        _ => return None,
    })
}

/// Символ файла (§3). `line`, `end_line` — с 1; `line` — строка имени (не атрибута/декоратора).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub qual: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    pub body_hash: String,
    #[serde(default)]
    pub exported: Option<bool>,
}

/// Агрегированная ссылка файла: `(name, kind)` → число и первая строка (§3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefAgg {
    pub name: String,
    pub kind: RefKind,
    pub count: u32,
    pub first_line: u32,
}

/// Файл в `PUT /code/files` (§4): только метаданные, содержимое не уходит.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileUpload {
    pub path: String,
    pub lang: Lang,
    /// blake3 содержимого, 64 hex.
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_oid: Option<String>,
    pub size: u64,
    pub parse_state: ParseState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<SkipReason>,
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub refs: Vec<RefAgg>,
}

/// Элемент манифеста `POST /code/sync` (§5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestItem {
    pub path: String,
    pub hash: String,
}

/// Хэш тела символа для правила устарелости (§8: `/files/changed`, `/code/impact`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHash {
    pub qual: String,
    pub body_hash: String,
}

/// Сигнатура (§3): первая непустая строка без хвостовых пробелов, не длиннее
/// [`SIG_MAX_CHARS`] символов (обрезка по символу с `…`); пустой текст — `None`.
pub fn sig_line(def_text: &str) -> Option<String> {
    let line = def_text
        .lines()
        .map(str::trim_end)
        .find(|l| !l.trim_start().is_empty())?;
    if line.chars().count() <= SIG_MAX_CHARS {
        return Some(line.to_string());
    }
    let mut out: String = line.chars().take(SIG_MAX_CHARS - 1).collect();
    out.push('…');
    Some(out)
}

/// Хэш тела (§3): строки обрезаны по краям, пустые убраны, склеены `\n`; blake3, 16 hex.
/// Переотступ, перевод строк CRLF/LF и пустые строки хэш не меняют.
pub fn body_hash(def_text: &str) -> String {
    let mut h = blake3::Hasher::new();
    let mut first = true;
    for line in def_text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !first {
            h.update(b"\n");
        }
        h.update(line.as_bytes());
        first = false;
    }
    h.finalize().to_hex()[..16].to_string()
}
