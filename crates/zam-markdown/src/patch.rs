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
    /// После splice повторный разбор не находит затронутую секцию ровно один раз
    /// (например, вставка дублирует путь или превращает начало во frontmatter).
    #[error("после правки секция не находится однозначно: {0}")]
    Broken(String),
}

/// Применяет операцию к исходнику заметки.
///
/// Путь секции сравнивается с заголовками точно (пробелы вокруг `>` не важны);
/// если путь есть у нескольких секций — [`PatchError::Ambiguous`]. После splice
/// новый текст разбирается повторно: если затронутая секция не находится ровно
/// одна — [`PatchError::Broken`].
/// Пустой `content` ничего не вставляет (у `ReplaceSection` — удаляет тело).
pub fn apply_patch(src: &str, op: &PatchOp) -> Result<Applied, PatchError> {
    let doc = parse(src);
    let (range, content, touched, path) = match op {
        PatchOp::ReplaceSection { section, content } => {
            let s = unique_section(&doc, section)?;
            (s.body.clone(), content, Some(s), section.as_str())
        }
        PatchOp::AppendToSection { section, content } => {
            let s = unique_section(&doc, section)?;
            (s.range.end..s.range.end, content, Some(s), section.as_str())
        }
        PatchOp::InsertAfterHeading { section, content } => {
            let s = unique_section(&doc, section)?;
            (
                s.body.start..s.body.start,
                content,
                Some(s),
                section.as_str(),
            )
        }
        PatchOp::Append { content } => (src.len()..src.len(), content, None, ""),
        PatchOp::Prepend { content } => (doc.body_start..doc.body_start, content, None, ""),
    };

    let insert = prepare(src, &range, content);
    let mut text = String::with_capacity(src.len() - range.len() + insert.len());
    text.push_str(&src[..range.start]);
    text.push_str(&insert);
    text.push_str(&src[range.end..]);

    let touched_section = touched.map(|s| s.path.join(" > "));
    if let Some(canon) = &touched_section
        && sections_by_path(&parse(&text), canon).count() != 1
    {
        return Err(PatchError::Broken(path.to_string()));
    }
    Ok(Applied {
        text,
        changed: range.start..range.start + insert.len(),
        touched_section,
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

/// Текст для вставки вместо `range`: переводы строк документа, перевод строки
/// перед вставкой (если она не в начале строки) и в конце.
///
/// Чтобы вставленный абзац не стал ленивым продолжением соседнего (CommonMark §4.8,
/// §5.1), добавляется пустая строка: перед вставкой — если она начинается обычным
/// текстом, а предыдущая строка документа — абзац, пункт списка, цитата или строка
/// таблицы; после — если вставка кончается такой строкой, а следующая строка
/// документа — обычный текст. Всё добавленное входит во вставку.
fn prepare(src: &str, range: &Range<usize>, content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let at = range.start;
    let nl = newline_of(src);
    let lf = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(lf.len() + lf.len() / 8 + 8);
    let bom = if src.starts_with(BOM) { BOM.len() } else { 0 };
    let before = &src[bom..at.max(bom)];
    if !before.is_empty() && !before.ends_with(['\n', '\r']) {
        out.push_str(nl);
    }
    let first = lf.split('\n').next().unwrap_or("");
    if continues_paragraph(prev_line(before)) && is_plain_text(first) {
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
    let last = lf.strip_suffix('\n').unwrap_or(&lf);
    let last = last.rsplit('\n').next().unwrap_or("");
    let next = src[range.end..].split(['\n', '\r']).next().unwrap_or("");
    if continues_paragraph(last) && is_plain_text(next) {
        out.push_str(nl);
    }
    out
}

/// Последняя строка `before` без перевода строки (пусто, если её нет).
fn prev_line(before: &str) -> &str {
    let b = before
        .strip_suffix("\r\n")
        .or_else(|| before.strip_suffix(['\n', '\r']))
        .unwrap_or(before);
    b.rsplit(['\n', '\r']).next().unwrap_or("")
}

/// Отступ строки до 3 пробелов снят (4 и больше — отступный код).
fn unindent(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len() - rest.len() <= 3).then_some(rest)
}

/// Строка заголовка ATX, ограждения кода или подчёркивания setext / разделителя.
fn is_closed_block_line(line: &str) -> bool {
    let Some(t) = unindent(line) else {
        return false;
    };
    let hashes = t.len() - t.trim_start_matches('#').len();
    let atx =
        (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace);
    let underline = !t.trim_end().is_empty()
        && (t.trim_end().bytes().all(|b| b == b'=') || t.trim_end().bytes().all(|b| b == b'-'));
    atx || underline || t.starts_with("```") || t.starts_with("~~~")
}

/// Строка, к которой может лениво присоединиться следующая: непустая и не
/// закрытый однострочный блок (абзац, пункт списка, цитата, строка таблицы).
fn continues_paragraph(line: &str) -> bool {
    !line.trim().is_empty() && !is_closed_block_line(line)
}

/// Обычный текст: непустая строка, которая сама не начинает блок, прерывающий
/// абзац (заголовок, цитата, ограждение, пункт списка). Подчёркивания `===`/`---`
/// считаются текстом: после абзаца они превратили бы его в заголовок setext.
fn is_plain_text(line: &str) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    let Some(t) = unindent(line) else {
        // отступный код не прерывает абзац — строка продолжит его
        return true;
    };
    let hashes = t.len() - t.trim_start_matches('#').len();
    let atx =
        (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace);
    let bullet =
        t.len() >= 2 && matches!(t.as_bytes()[0], b'-' | b'*' | b'+') && t.as_bytes()[1] == b' ';
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let ordered = (1..=9).contains(&digits)
        && t[digits..].starts_with(['.', ')'])
        && t[digits + 1..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
    !(atx
        || bullet
        || ordered
        || t.starts_with('>')
        || t.starts_with("```")
        || t.starts_with("~~~"))
}

/// Перевод строки документа: CRLF, если первый перевод строки — `\r\n`, иначе LF.
fn newline_of(src: &str) -> &'static str {
    match src.find('\n') {
        Some(i) if src[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}
