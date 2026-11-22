//! Код в тексте Markdown без разбора comrak: блоки в ограждениях (```` ``` ````, `~~~`, в том
//! числе в цитатах) и инлайн-код `` `…` ``. Определение кода — одно на проект,
//! [`zam_secrets::code`] (круг правок 2 задачи 5 M7.1, решение координатора: его же берёт правило
//! флагов сканера); здесь — сегментация предложений ([`crate::split_sentences`]: внутри кода
//! границ нет) и маска прозы ([`prose_mask`], M6.6 §6.1: имена и псевдонимы терминов в коде не
//! ищутся). Блоки с отступом в 4 пробела кодом не считаются.

use zam_secrets::code::code_spans_of;
pub(crate) use zam_secrets::code::{Line, code_spans, fence_of, fenced_lines, lines_of};

/// Проза `text` без кода (M6.6 §6.1): блоки в ограждениях (```` ``` ````, `~~~`, вместе со
/// строками ограждений; незакрытый — до конца текста) и инлайн-код `` `…` `` заменены пробелами
/// той же длины в байтах, а переводы строк (`\n`, `\r`) внутри кода остаются на месте. Длина в
/// байтах, смещения и число строк сохраняются: позиция в маске — позиция в исходнике. Границы
/// кода — те же, что у [`crate::split_sentences`].
pub fn prose_mask(text: &str) -> String {
    mask(text, false)
}

/// Как [`prose_mask`], но `prefix` — лишь начало более длинного текста (окно маски головы
/// промпта, повторное ревью задачи 5): инлайн-код, не закрытый до конца `prefix` и не прерванный
/// пустой строкой, может закрыться дальше — он код до конца `prefix`. Незакрытый блок в
/// ограждении и так идёт до конца.
pub fn prose_mask_prefix(prefix: &str) -> String {
    mask(prefix, true)
}

fn mask(text: &str, open_tail: bool) -> String {
    let lines = lines_of(text);
    let fenced = fenced_lines(text, &lines);
    let mut spans = code_spans_of(text, &lines, &fenced, open_tail);
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for r in spans {
        // границы участков — на ASCII-знаках (обратная кавычка, перевод строки) или на краях
        // строк: срезы не режут UTF-8; пересечение участков — продолжение с конца прежнего
        let start = r.start.max(pos);
        if start >= r.end {
            continue;
        }
        out.push_str(&text[pos..start]);
        out.extend(text.as_bytes()[start..r.end].iter().map(|&b| match b {
            b'\n' | b'\r' => b as char,
            _ => ' ',
        }));
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(n: usize) -> String {
        " ".repeat(n)
    }

    /// M6.6 §6.1: код — пробелы той же длины в байтах, переводы строк в ограждениях — на месте,
    /// проза — как была; незакрытая обратная кавычка и кавычка через пустую строку — не код.
    #[test]
    fn prose_mask_keeps_offsets() {
        let text =
            "a `ёж` b ``c`d`` e\n```rs\nlet ё = 1;\n```\nf ~~~ g\n~~~\nh\n~~~\nk `open\n\nl` m";
        let expect = format!(
            "a {} b {} e\n{}\n{}\n{}\nf ~~~ g\n{}\n{}\n{}\nk `open\n\nl` m",
            sp("`ёж`".len()),
            sp("``c`d``".len()),
            sp("```rs".len()),
            sp("let ё = 1;".len()),
            sp(3),
            sp(3),
            sp(1),
            sp(3),
        );
        let m = prose_mask(text);
        assert_eq!(m, expect);
        assert_eq!(m.len(), text.len());
        assert_eq!(m.lines().count(), text.lines().count());
        // CRLF: оба байта перевода строки остаются
        assert_eq!(
            prose_mask("a\r\n```\r\nx\r\n```\r\nb"),
            "a\r\n   \r\n \r\n   \r\nb"
        );
        // незакрытое ограждение — код до конца текста
        assert_eq!(prose_mask("a\n```\nzam"), "a\n   \n   ");
        // инлайн-код через одиночный перевод строки — код, перевод строки на месте
        assert_eq!(prose_mask("x `a\nb` y"), "x   \n   y");
        // инлайн-код, закрытый внутри блока, — пересечение участков
        assert_eq!(prose_mask("a `b\n```\nc`\n```"), "a   \n   \n  \n   ");
        assert_eq!(prose_mask(""), "");
        assert_eq!(prose_mask("проза без кода"), "проза без кода");
    }

    /// Ревью задачи 5, M2 (CommonMark): строка информации после ограждения обратными кавычками
    /// не содержит обратных кавычек — «```x``` …» в начале строки — инлайн-код, а не блок до
    /// конца текста; у `~~~` — может.
    #[test]
    fn backtick_fence_info_has_no_backticks() {
        assert_eq!(fence_of("```x```"), None);
        assert_eq!(fence_of("```zam code map``` и дальше"), None);
        assert_eq!(fence_of("```rust"), Some((b'`', 3)));
        assert_eq!(fence_of("````"), Some((b'`', 4)));
        assert_eq!(fence_of("~~~ x`y"), Some((b'~', 3)));
        assert_eq!(
            prose_mask("```x``` hooks\nтекст"),
            format!("{} hooks\nтекст", sp(7))
        );
    }

    /// Повторное ревью задачи 5: начало более длинного текста — инлайн-код, не закрытый до
    /// конца и не прерванный пустой строкой, — код до конца; прочее — как у [`prose_mask`].
    #[test]
    fn prose_mask_prefix_keeps_open_code() {
        assert_eq!(prose_mask_prefix("a `b c"), format!("a {}", sp(4)));
        assert_eq!(
            prose_mask_prefix("a `b` c `d"),
            format!("a {} c {}", sp(3), sp(2))
        );
        assert_eq!(prose_mask_prefix("a `b\n\nc"), "a `b\n\nc");
        assert_eq!(prose_mask_prefix("a ``b` c"), format!("a {}", sp(6)));
        assert_eq!(prose_mask_prefix("a\n```\nb"), "a\n   \n ");
        assert_eq!(prose_mask_prefix("проза"), "проза");
    }
}
