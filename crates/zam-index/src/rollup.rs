//! Свёртка L0 (§8.3 ТЗ): чистые функции без ввода-вывода; данные и эмбеддинги даёт воркер.
//!
//! - **Заметка** (`l0_source = 'auto'`): предложение, ближайшее по косинусу к центроиду
//!   эмбеддингов листовых чанков, не длиннее [`L0_MAX_TOKENS`]. Кандидаты — предложения
//!   ([`zam_markdown::split_sentences`]) из [`TOP_CHUNKS`] чанков, ближайших к центроиду,
//!   не больше [`MAX_CANDIDATES`]: так инференс на заметку ограничен восемью короткими
//!   текстами.
//! - **Папка**: строки `<имя>: <L0>` детей по свежести, не больше [`FOLDER_MAX_LINES`].

use zam_markdown::split_sentences;

/// Предел L0 заметки, токенов модели.
pub const L0_MAX_TOKENS: usize = 50;
/// Предел строк L0 папки.
pub const FOLDER_MAX_LINES: usize = 50;
/// Чанков-источников кандидатов.
pub const TOP_CHUNKS: usize = 3;
/// Предложений-кандидатов из одного чанка.
const PER_CHUNK: usize = 4;
/// Всего кандидатов (эмбеддинги считаются для каждого).
pub const MAX_CANDIDATES: usize = 8;
/// Короче — не сводка (обрывок, маркер, число).
const MIN_SENTENCE_CHARS: usize = 12;

/// Косинус; нулевой вектор — 0.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na <= f32::MIN_POSITIVE || nb <= f32::MIN_POSITIVE {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Среднее векторов одной размерности; пусто или разные размерности — `None`.
pub fn centroid(vs: &[&[f32]]) -> Option<Vec<f32>> {
    let dim = vs.first()?.len();
    if dim == 0 || vs.iter().any(|v| v.len() != dim) {
        return None;
    }
    let mut c = vec![0.0f32; dim];
    for v in vs {
        for (s, x) in c.iter_mut().zip(*v) {
            *s += x;
        }
    }
    let n = vs.len() as f32;
    c.iter_mut().for_each(|x| *x /= n);
    Some(c)
}

/// Предложение без разметки Markdown в начале и с одним пробелом между словами;
/// код, таблицы и обрывки — `None`.
pub fn clean_sentence(s: &str) -> Option<String> {
    let t = s.trim();
    if t.starts_with("```") || t.starts_with("~~~") || t.starts_with('|') {
        return None;
    }
    let mut t = t;
    loop {
        let before = t;
        t = t.trim_start_matches(['#', '>', '-', '*', '+']).trim_start();
        // «1. » / «2) » — пункт нумерованного списка
        let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
            t = t[digits + 2..].trim_start();
        }
        if t == before {
            break;
        }
    }
    let out = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let letters = out.chars().filter(|c| c.is_alphabetic()).count();
    let words = out
        .split(' ')
        .filter(|w| w.chars().any(char::is_alphabetic))
        .count();
    (out.chars().count() >= MIN_SENTENCE_CHARS && letters * 2 >= out.chars().count() && words >= 2)
        .then_some(out)
}

/// Кандидаты в L0: предложения из [`TOP_CHUNKS`] листов, ближайших к `center`
/// (по [`PER_CHUNK`] из каждого, в порядке текста), без повторов (перекрытие чанков),
/// не больше [`MAX_CANDIDATES`].
pub fn candidate_sentences(chunks: &[(&str, &[f32])], center: &[f32]) -> Vec<String> {
    let mut order: Vec<usize> = (0..chunks.len()).collect();
    order.sort_by(|&a, &b| {
        cosine(chunks[b].1, center)
            .total_cmp(&cosine(chunks[a].1, center))
            .then(a.cmp(&b))
    });
    let mut out: Vec<String> = Vec::new();
    for &i in order.iter().take(TOP_CHUNKS) {
        let text = chunks[i].0;
        let mut taken = 0;
        for r in split_sentences(text) {
            if taken == PER_CHUNK || out.len() == MAX_CANDIDATES {
                break;
            }
            if let Some(s) = clean_sentence(&text[r])
                && !out.contains(&s)
            {
                out.push(s);
                taken += 1;
            }
        }
    }
    out
}

/// Индекс кандидата, ближайшего к `center`; при равенстве — более ранний.
pub fn best_candidate(center: &[f32], cands: &[&[f32]]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, v) in cands.iter().enumerate() {
        let s = cosine(v, center);
        if best.is_none_or(|(_, b)| s > b) {
            best = Some((i, s));
        }
    }
    best.map(|(i, _)| i)
}

/// Не больше `max` токенов по `count`: отбрасываются слова с конца. Одно слово длиннее
/// предела режется по символам.
pub fn truncate_tokens(s: &str, max: usize, count: &dyn Fn(&str) -> usize) -> String {
    if count(s) <= max {
        return s.to_string();
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    // наибольший префикс слов в пределе (монотонно по длине — бинарный поиск)
    let (mut lo, mut hi) = (0usize, words.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if count(&words[..mid].join(" ")) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo > 0 {
        return words[..lo].join(" ");
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if count(&out) > max {
            out.pop();
            break;
        }
    }
    out
}

/// Ребёнок папки для свёртки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderChild {
    pub name: String,
    pub folder: bool,
    pub l0: Option<String>,
    /// Unix-время, секунды: свежие — первыми.
    pub updated_at: i64,
}

/// L0 папки: `<имя>: <L0>` (папка — `<имя>/`) по свежести, затем по имени, не больше
/// [`FOLDER_MAX_LINES`] строк; переводы строк внутри L0 заменяются пробелами.
/// Нет детей — `None`.
pub fn folder_l0(children: &[FolderChild]) -> Option<String> {
    let mut v: Vec<&FolderChild> = children.iter().collect();
    v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.name.cmp(&b.name)));
    let lines: Vec<String> = v
        .into_iter()
        .take(FOLDER_MAX_LINES)
        .map(|c| {
            let name = if c.folder {
                format!("{}/", c.name)
            } else {
                c.name.clone()
            };
            match c
                .l0
                .as_deref()
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|l| !l.is_empty())
            {
                Some(l) => format!("{name}: {l}"),
                None => name,
            }
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn cosine_and_centroid() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        let c = centroid(&[&[1.0, 0.0], &[0.0, 1.0]]).unwrap();
        assert_eq!(c, vec![0.5, 0.5]);
        assert!(centroid(&[]).is_none());
        assert!(centroid(&[&[1.0], &[1.0, 2.0]]).is_none());
    }

    #[test]
    fn clean_strips_markdown_and_rejects_code() {
        assert_eq!(
            clean_sentence("## Сборка проекта\n  идёт  долго.").as_deref(),
            Some("Сборка проекта идёт долго.")
        );
        assert_eq!(
            clean_sentence("- 1. пункт списка с текстом").as_deref(),
            Some("пункт списка с текстом")
        );
        assert_eq!(clean_sentence("```rust"), None);
        assert_eq!(clean_sentence("| a | b |"), None);
        assert_eq!(clean_sentence("Да."), None);
        assert_eq!(clean_sentence("12345 67890 1234"), None);
    }

    #[test]
    fn candidates_come_from_chunks_nearest_to_centroid() {
        let far: &[f32] = &[0.0, 1.0];
        let near: &[f32] = &[1.0, 0.0];
        let chunks = [
            ("Далёкий текст про погоду. Ещё одна фраза о дожде.", far),
            (
                "Сборка идёт командой cargo build. Тесты запускаются отдельно.",
                near,
            ),
        ];
        let c = candidate_sentences(&chunks, &[1.0, 0.1]);
        assert_eq!(c[0], "Сборка идёт командой cargo build.");
        assert!(c.len() <= MAX_CANDIDATES);
        // повтор из перекрытия соседних чанков не дублируется
        let dup = [(chunks[1].0, near), (chunks[1].0, near)];
        assert_eq!(candidate_sentences(&dup, near).len(), 2);
    }

    #[test]
    fn best_candidate_is_nearest() {
        let c = [1.0f32, 0.0];
        assert_eq!(best_candidate(&c, &[&[0.0, 1.0], &[0.9, 0.1]]), Some(1));
        assert_eq!(best_candidate(&c, &[]), None);
    }

    #[test]
    fn truncate_respects_token_budget() {
        let s = "one two three four five";
        assert_eq!(truncate_tokens(s, 10, &words), s);
        assert_eq!(truncate_tokens(s, 3, &words), "one two three");
        let chars = |s: &str| s.chars().count();
        assert_eq!(truncate_tokens("абвгдежз", 3, &chars), "абв");
    }

    #[test]
    fn folder_lines_by_recency_and_capped() {
        let ch = |name: &str, folder: bool, l0: Option<&str>, t: i64| FolderChild {
            name: name.into(),
            folder,
            l0: l0.map(str::to_string),
            updated_at: t,
        };
        let v = vec![
            ch("old", false, Some("старое"), 1),
            ch("sub", true, Some("a: x\nb: y"), 5),
            ch("new", false, None, 9),
        ];
        assert_eq!(folder_l0(&v).unwrap(), "new\nsub/: a: x b: y\nold: старое");
        assert_eq!(folder_l0(&[]), None);
        let many: Vec<FolderChild> = (0..80)
            .map(|i| ch(&format!("n{i:02}"), false, None, i))
            .collect();
        let l = folder_l0(&many).unwrap();
        assert_eq!(l.lines().count(), FOLDER_MAX_LINES);
        assert!(l.starts_with("n79"));
    }
}
