//! Эвристики пропуска до разбора (спецификация M5.6 §12): бинарный и минифицированный файл.

use crate::proto::SkipReason;

/// Окно поиска NUL, байт.
const BINARY_WINDOW: usize = 8 * 1024;
/// Строка длиннее этого числа символов — минифицированный файл.
const LINE_MAX_CHARS: usize = 10_000;
/// Средняя длина строки больше этого числа символов — минифицированный файл.
const AVG_LINE_MAX_CHARS: usize = 300;

/// NUL в первых 8 КиБ → `binary`; строка > 10 000 символов или средняя длина строки > 300 →
/// `minified`; иначе `None`. Символы — кодовые точки UTF-8 (байты продолжения не считаются),
/// `\r` и `\n` в длину строки не входят. Один проход, выход на первой длинной строке.
pub fn sniff(bytes: &[u8]) -> Option<SkipReason> {
    if bytes[..bytes.len().min(BINARY_WINDOW)].contains(&0) {
        return Some(SkipReason::Binary);
    }
    let (mut total, mut lines) = (0usize, 0usize);
    for line in bytes.split(|&b| b == b'\n') {
        let n = line
            .iter()
            .filter(|&&b| b & 0xC0 != 0x80 && b != b'\r')
            .count();
        if n > LINE_MAX_CHARS {
            return Some(SkipReason::Minified);
        }
        total += n;
        lines += 1;
    }
    // хвост после последнего `\n` пуст — это не строка
    if bytes.last() == Some(&b'\n') {
        lines -= 1;
    }
    if lines > 0 && total > AVG_LINE_MAX_CHARS * lines {
        return Some(SkipReason::Minified);
    }
    None
}
