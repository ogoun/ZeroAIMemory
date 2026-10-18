//! Фазз-цель: `apply_patch` не паникует, байты вне `changed` совпадают с исходными,
//! `section_bytes` возвращает подстроку исходника.
//!
//! Вход: первый байт — операция, второй — номер секции, остальное — документ и
//! `content`, разделённые первым `\0`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use zam_markdown::{PatchOp, apply_patch, parse, section_bytes};

fuzz_target!(|data: &[u8]| {
    let [kind, pick, rest @ ..] = data else {
        return;
    };
    let Ok(rest) = std::str::from_utf8(rest) else {
        return;
    };
    let (src, content) = rest.split_once('\0').unwrap_or((rest, "x"));
    let content = content.to_string();
    let doc = parse(src);
    let paths: Vec<String> = doc
        .sections
        .iter()
        .filter(|s| s.level > 0)
        .map(|s| s.path.join(" > "))
        .collect();
    let section = if paths.is_empty() {
        String::from("нет")
    } else {
        paths[usize::from(*pick) % paths.len()].clone()
    };
    if let Some(bytes) = section_bytes(src, &section) {
        let start = bytes.as_ptr() as usize - src.as_ptr() as usize;
        assert!(start + bytes.len() <= src.len());
    }
    let op = match kind % 5 {
        0 => PatchOp::ReplaceSection { section, content },
        1 => PatchOp::AppendToSection { section, content },
        2 => PatchOp::InsertAfterHeading { section, content },
        3 => PatchOp::Append { content },
        _ => PatchOp::Prepend { content },
    };
    let Ok(a) = apply_patch(src, &op) else {
        return;
    };
    let c = a.changed.clone();
    assert!(c.start <= c.end && c.end <= a.text.len());
    let tail = a.text.len() - c.end;
    assert!(c.start + tail <= src.len());
    assert_eq!(&a.text[..c.start], &src[..c.start]);
    assert_eq!(&a.text[c.end..], &src[src.len() - tail..]);
});
