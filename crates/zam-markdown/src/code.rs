//! Код в тексте Markdown без разбора comrak: блоки в ограждениях (```` ``` ````, `~~~`, в том
//! числе в цитатах) и инлайн-код `` `…` ``. Определение кода — одно на проект,
//! [`zam_secrets::code`] (круг правок 2 задачи 5 M7.1, решение координатора: его же берёт правило
//! флагов сканера); здесь — сегментация предложений ([`crate::split_sentences`]: внутри кода
//! границ нет) и маска прозы ([`prose_mask`], M6.6 §6.1: имена и псевдонимы терминов в коде не
//! ищутся). Блоки с отступом в 4 пробела кодом не считаются.
//!
//! Исключение — [`has_code_block`] (M7.3 §6.7 п. 3, кандидат в Rules): «есть ли в тексте блок
//! кода» решает разбор comrak по правилам CommonMark (огороженный блок и блок отступом — только
//! после пустой строки и вне списка), а не построчное определение выше: вложенный список и
//! продолжение строки с четырьмя пробелами — не блок.

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};
use zam_secrets::code::code_spans_of;
pub(crate) use zam_secrets::code::{Line, code_spans, fence_of, fenced_lines, lines_of};

use crate::options::comrak_options;

/// Есть ли в Markdown `src` блок кода — огороженный (```` ``` ````, `~~~`) или отступом
/// (CommonMark: 4 пробела после пустой строки, вне элемента списка) на любой глубине (цитата,
/// элемент списка); разбор — comrak с [`comrak_options`] (frontmatter — не блок). Инлайн-код
/// `` `…` `` — не блок. [`crate::Doc`] из [`crate::parse`] блоков кода не отдаёт.
pub fn has_code_block(src: &str) -> bool {
    let arena = Arena::new();
    let root = parse_document(&arena, src, &comrak_options());
    root.descendants()
        .any(|n| matches!(n.data().value, NodeValue::CodeBlock(_)))
}

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
    mask_by(text, open_tail, fenced_lines)
}

/// [`mask`] со строками блоков по `fenced` (сверка с определением кода круга 2 — тесты).
pub(crate) fn mask_by(
    text: &str,
    open_tail: bool,
    fenced: fn(&str, &[Line]) -> Vec<bool>,
) -> String {
    let lines = lines_of(text);
    let fenced = fenced(text, &lines);
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

    /// M7.3 §6.7 п. 3: блок кода — огороженный (и в цитате, в элементе списка) и отступом после
    /// пустой строки; вложенный список с четырьмя пробелами, продолжение строки отступом и
    /// однострочный код в обратных кавычках — не блок; frontmatter — не блок.
    #[test]
    fn has_code_block_cases() {
        for (src, want) in [
            ("Правило.\n\n```sh\nrm -rf /\n```\n", true),
            ("~~~\nкод\n~~~", true),
            ("```\nнезакрытый", true),
            ("Текст.\n\n    let x = 1;\n", true),
            ("> цитата\n>\n> ```\n> код\n> ```\n", true),
            ("- пункт\n\n  ```\n  код\n  ```\n", true),
            ("- пункт\n    - вложенный пункт\n    - ещё\n", false),
            ("1. шаг\n    1. подшаг\n", false),
            ("Абзац, который\n    продолжается с отступом.\n", false),
            ("Запускай `cargo test` перед коммитом.", false),
            ("Используй ``a `b` c`` как есть.", false),
            ("---\ntags: [a]\n---\nПравило без кода.", false),
            ("", false),
        ] {
            assert_eq!(has_code_block(src), want, "{src:?}");
        }
    }

    /// Круг правок 5 задачи 5 M7.1: маска прозы (имена терминов в коде не ищутся) и сегментация
    /// предложений против определения кода круга 2 на текстах со списками — числа журнала.
    /// `cargo test -p zam-markdown --release --lib -- --ignored round2_comparison --nocapture`.
    #[test]
    #[ignore]
    fn round2_comparison_on_lists() {
        use zam_secrets::code::fenced_lines_round2;
        const ATOMS: &[&str] = &[
            "- ",
            "  - ",
            "    - ",
            "1. ",
            "   1. ",
            "10) ",
            "-   ",
            "-\t",
            "* ",
            "  ",
            "   ",
            "    ",
            "      ",
            "\t",
            "```",
            "```sh",
            "~~~",
            "> ",
            "Термин. ",
            "Слово x. ",
            "\n",
            "\n",
            "\n",
            "\n\n",
            "`",
        ];
        let mut x: u64 = 0x5EED_0005_0000_0001;
        let mut pick = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let n = 200_000;
        let (mut texts, mut mask_diff, mut more, mut less, mut seg_diff) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        for _ in 0..n {
            let k = pick(30);
            let mut t = String::new();
            for _ in 0..k {
                t.push_str(ATOMS[pick(ATOMS.len())]);
            }
            if !["- ", "1. ", "10) ", "* "].iter().any(|m| t.contains(m)) {
                continue;
            }
            texts += 1;
            let now = mask_by(&t, false, fenced_lines);
            let old = mask_by(&t, false, fenced_lines_round2);
            if now != old {
                mask_diff += 1;
                // байты, ставшие кодом (замаскированы теперь, но не в круге 2) и ставшие прозой
                let triples = || now.bytes().zip(old.bytes()).zip(t.bytes());
                let to_code = triples().filter(|((a, b), c)| a != c && b == c).count();
                let to_prose = triples().filter(|((a, b), c)| a == c && b != c).count();
                more += u64::from(to_code > 0);
                less += u64::from(to_prose > 0);
            }
            let s_now = crate::sentences::split_sentences_by(&t, fenced_lines);
            let s_old = crate::sentences::split_sentences_by(&t, fenced_lines_round2);
            seg_diff += u64::from(s_now != s_old);
        }
        println!(
            "тексты со списками {texts}: маска прозы отличается от круга 2 в {mask_diff} (кода больше \
             в {more}, меньше в {less}); сегментация предложений отличается в {seg_diff}"
        );
    }
}
