//! Общие генераторы proptest для тестов `zam-markdown`.

#![allow(dead_code)]

use proptest::prelude::*;

/// Строка Markdown: заголовки, setext, списки, цитаты, ограждения, ссылки.
pub fn md_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("# Заголовок".to_string()),
        Just("## Второй".to_string()),
        Just("### Третий #".to_string()),
        Just("#### Четвёртый".to_string()),
        Just("#без пробела".to_string()),
        Just("текст абзаца".to_string()),
        Just("Setext".to_string()),
        Just("===".to_string()),
        Just("---".to_string()),
        Just("".to_string()),
        Just("- пункт".to_string()),
        Just("- # заголовок в списке".to_string()),
        Just("  продолжение".to_string()),
        Just("1. нумерованный".to_string()),
        Just("> цитата".to_string()),
        Just("> ## в цитате".to_string()),
        Just("```".to_string()),
        Just("~~~".to_string()),
        Just("[[ссылка#якорь|текст]]".to_string()),
        Just("см. [ссылку](путь/файл.md#якорь) и ![кот](img/кот.png)".to_string()),
        Just("> цитата со [ссылкой](a.md \"заголовок\")".to_string()),
        Just("- пункт [в списке](b.md)".to_string()),
        Just("[разорванная".to_string()),
        Just("ссылка](c.md)".to_string()),
        Just("> [в цитате".to_string()),
        Just("> через строку](d.md)".to_string()),
        Just("<https://example.com/x> и https://bare.example.com".to_string()),
        Just("[![вложенная](i.png)](e.md)".to_string()),
        "[a-zа-я ]{0,12}",
    ]
}

/// Документ из случайных строк с LF или CRLF.
pub fn md_doc() -> impl Strategy<Value = String> {
    (prop::collection::vec(md_line(), 0..24), any::<bool>()).prop_map(|(lines, crlf)| {
        let nl = if crlf { "\r\n" } else { "\n" };
        lines.join(nl)
    })
}
