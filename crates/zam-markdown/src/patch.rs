//! PATCH заметки: байтовый splice исходника по секции (ТЗ §5.3).
//!
//! AST обратно не сериализуется: меняется ровно один диапазон байтов исходника,
//! всё вне него остаётся байт в байт. Вставляемый текст приводится к переводам
//! строк документа и всегда заканчивается переводом строки.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::doc::{Section, parse, sections_by_path};

const BOM: &str = "\u{FEFF}";

/// Операция PATCH. В JSON — объект с полем `op` в snake_case, например
/// `{"op":"replace_section","section":"Установка > Linux","content":"…"}`.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// Заменяет тело секции (всё после строк заголовка, включая подсекции);
    /// строки заголовка остаются.
    ReplaceSection { section: String, content: String },
    /// Дописывает в конец всего диапазона секции, после её подсекций;
    /// следующий заголовок не затрагивается.
    AppendToSection { section: String, content: String },
    /// Вставляет сразу после строк заголовка секции.
    InsertAfterHeading { section: String, content: String },
    /// Дописывает в конец документа.
    Append { content: String },
    /// Вставляет в начало тела: после BOM и frontmatter.
    Prepend { content: String },
}

/// Результат PATCH.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Новый текст заметки.
    pub text: String,
    /// Диапазон вставленных байтов в новом тексте (вместе с добавленными
    /// переводами строк). До него и после него — байты исходника без изменений.
    pub changed: Range<usize>,
    /// Канонический путь затронутой секции (заголовки через ` > `);
    /// `None` для `Append` и `Prepend`.
    pub touched_section: Option<String>,
}

/// Ошибка PATCH.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    #[error("секция не найдена: {0}")]
    SectionNotFound(String),
    #[error("путь секции неоднозначен: {0}")]
    Ambiguous(String),
}

/// Применяет операцию к исходнику заметки.
///
/// Путь секции сравнивается с заголовками точно (пробелы вокруг `>` не важны);
/// если путь есть у нескольких секций — [`PatchError::Ambiguous`].
/// Пустой `content` ничего не вставляет (у `ReplaceSection` — удаляет тело).
pub fn apply_patch(src: &str, op: &PatchOp) -> Result<Applied, PatchError> {
    let doc = parse(src);
    let (range, content, touched) = match op {
        PatchOp::ReplaceSection { section, content } => {
            let s = unique_section(&doc, section)?;
            (s.body.clone(), content, Some(s))
        }
        PatchOp::AppendToSection { section, content } => {
            let s = unique_section(&doc, section)?;
            (s.range.end..s.range.end, content, Some(s))
        }
        PatchOp::InsertAfterHeading { section, content } => {
            let s = unique_section(&doc, section)?;
            (s.body.start..s.body.start, content, Some(s))
        }
        PatchOp::Append { content } => (src.len()..src.len(), content, None),
        PatchOp::Prepend { content } => (doc.body_start..doc.body_start, content, None),
    };

    let insert = prepare(src, range.start, content);
    let mut text = String::with_capacity(src.len() - range.len() + insert.len());
    text.push_str(&src[..range.start]);
    text.push_str(&insert);
    text.push_str(&src[range.end..]);
    Ok(Applied {
        text,
        changed: range.start..range.start + insert.len(),
        touched_section: touched.map(|s| s.path.join(" > ")),
    })
}

/// Байты секции `path` в документе — весь её диапазон, с заголовком и подсекциями
/// (для проверки «секция не менялась после base_rev»). `None`, если секции нет
/// или путь неоднозначен.
pub fn section_bytes<'a>(src: &'a str, path: &str) -> Option<&'a str> {
    let doc = parse(src);
    let mut found = sections_by_path(&doc, path);
    let s = found.next()?;
    if found.next().is_some() {
        return None;
    }
    src.get(s.range.clone())
}

fn unique_section<'a>(doc: &'a crate::Doc, path: &str) -> Result<&'a Section, PatchError> {
    let mut found = sections_by_path(doc, path);
    let s = found
        .next()
        .ok_or_else(|| PatchError::SectionNotFound(path.to_string()))?;
    if found.next().is_some() {
        return Err(PatchError::Ambiguous(path.to_string()));
    }
    Ok(s)
}

/// Текст для вставки в позицию `at`: переводы строк документа, перевод строки
/// перед вставкой (если `at` не в начале строки) и в конце.
fn prepare(src: &str, at: usize, content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let nl = newline_of(src);
    let lf = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(lf.len() + lf.len() / 8 + 4);
    let bom = if src.starts_with(BOM) { BOM.len() } else { 0 };
    if at > bom && !src[..at].ends_with(['\n', '\r']) {
        out.push_str(nl);
    }
    if nl == "\n" {
        out.push_str(&lf);
    } else {
        out.push_str(&lf.replace('\n', nl));
    }
    if !lf.ends_with('\n') {
        out.push_str(nl);
    }
    out
}

/// Перевод строки документа: CRLF, если первый перевод строки — `\r\n`, иначе LF.
fn newline_of(src: &str) -> &'static str {
    match src.find('\n') {
        Some(i) if src[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}
