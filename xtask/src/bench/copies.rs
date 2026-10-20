//! Размножение корпуса eval до N заметок (бенчмарк §17).
//!
//! Копия — компактная заметка из одного-двух чанков: заголовок исходной заметки с номером
//! копии и окно абзацев исходника не короче [`FRAGMENT_CHARS`] символов. Окно сдвигается от
//! копии к копии, поэтому тексты копий одного исходника различаются; путь копии
//! (`bench/cNN/NNNNN-<имя>`) входит в `context_prefix`, поэтому кэш `embeddings` их не
//! склеивает: каждая копия проходит инференс, как новая заметка.
//!
//! Компактность нужна, чтобы 10 000 заметок помещались на диск и индексировались на CPU за
//! разумное время (≈ 1 чанк на копию против ≈ 5 у исходных заметок).

use crate::eval::corpus::Corpus;

/// Нижняя граница длины окна, символов. Чанкер режет окно по своим порогам (300–500
/// токенов): по замеру на этом корпусе листы копий — в среднем ~200 токенов bge-m3, как и
/// листы исходных заметок, а не 400 из §17 — поэтому скорость индексации в отчёте
/// пересчитывается по токенам.
pub const FRAGMENT_CHARS: usize = 1600;
/// Копий в одном каталоге (`bench/cNN`).
const PER_FOLDER: usize = 500;
/// Сдвиг окна на абзацы между раундами копий одного исходника.
const STEP: usize = 3;

/// Заметка для `POST /nodes`.
#[derive(Debug, Clone)]
pub struct NoteSpec {
    pub path: String,
    pub text: String,
}

/// Тело без frontmatter: абзацы (блоки между пустыми строками; блок кода — целиком) и
/// заголовок первого уровня.
fn split(text: &str) -> (Option<String>, Vec<String>) {
    let mut body = text;
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---\n")
    {
        body = &rest[end + 5..];
    }
    let mut title = None;
    let mut paras = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && title.is_none() && cur.is_empty() && line.starts_with("# ") {
            title = Some(line[2..].trim().to_string());
            continue;
        }
        if !fence && line.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(cur.join("\n"));
                cur.clear();
            }
            continue;
        }
        cur.push(line);
    }
    if !cur.is_empty() {
        paras.push(cur.join("\n"));
    }
    (title, paras)
}

/// Окно абзацев с `start` (по кругу) не короче [`FRAGMENT_CHARS`].
fn window(paras: &[String], start: usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut chars = 0;
    for k in 0..paras.len() {
        let p = &paras[(start + k) % paras.len()];
        out.push(p);
        chars += p.chars().count();
        if chars >= FRAGMENT_CHARS {
            break;
        }
    }
    out.join("\n\n")
}

/// `total - <заметок корпуса>` копий (пусто, если корпус уже не меньше `total`).
pub fn make(corpus: &Corpus, total: usize) -> Vec<NoteSpec> {
    let base: Vec<_> = corpus.notes.values().collect();
    let n = base.len();
    let count = total.saturating_sub(n);
    let mut out = Vec::with_capacity(count);
    if n == 0 {
        return out;
    }
    let parsed: Vec<_> = base.iter().map(|b| split(&b.text)).collect();
    for k in 0..count {
        let i = k % n;
        let round = k / n + 1;
        let (title, paras) = &parsed[i];
        let name = base[i].path.rsplit('/').next().unwrap_or("note");
        let title = title.clone().unwrap_or_else(|| name.to_string());
        let body = if paras.is_empty() {
            base[i].text.clone()
        } else {
            window(paras, round * STEP)
        };
        out.push(NoteSpec {
            path: format!("bench/c{:02}/{k:05}-{name}", k / PER_FOLDER),
            text: format!("# {title} ({k})\n\n{body}\n"),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_code_blocks_and_skips_frontmatter() {
        let t = "---\ntags: [a]\n---\n# Заголовок\n\nпервый\nабзац\n\n```\nкод\n\nещё код\n```\n\n## B\n\nтретий\n";
        let (title, p) = split(t);
        assert_eq!(title.as_deref(), Some("Заголовок"));
        assert_eq!(
            p,
            vec![
                "первый\nабзац".to_string(),
                "```\nкод\n\nещё код\n```".to_string(),
                "## B".to_string(),
                "третий".to_string(),
            ]
        );
    }

    #[test]
    fn window_wraps_and_reaches_min_length() {
        let paras: Vec<String> = (0..4).map(|i| format!("{i}").repeat(700)).collect();
        let w = window(&paras, 3);
        assert!(w.starts_with('3'));
        assert!(w.chars().count() >= FRAGMENT_CHARS);
        // окно длиннее всех абзацев — все по одному разу
        let short = vec!["a".to_string(), "b".to_string()];
        assert_eq!(window(&short, 1), "b\n\na");
    }
}
