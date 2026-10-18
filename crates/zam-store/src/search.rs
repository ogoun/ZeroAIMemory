//! Полнотекстовый поиск по чанкам (§9.2 ТЗ, канал (b)): FTS5 без моделей.
//!
//! - Канал A — `chunks_fts` (unicode61, `bm25` с весами колонок text 1.0,
//!   heading_path 0.5, context_prefix 0.3), top 50. Сначала строгий запрос
//!   (все слова, префиксно), при пустой выдаче — ослабленный (любое слово).
//! - Канал B — `chunks_fts_tri` (trigram) по фразе запроса, top 20: только если
//!   в запросе есть идентификатор (токен ≥ 3 символов с `_ . / - :` или цифрами)
//!   либо канал A ничего не нашёл.
//! - Слияние weighted RRF (k = 60, веса 1.0 и 0.7), не больше 3 чанков на узел, top_k.
//!
//! Фильтр области (пространства, префикс пути, живые узлы) применяется внутри
//! SQL каждого канала, поэтому top-N канала считается уже по разрешённой области.

use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{Connection, ToSql, params_from_iter};
use zam_core::Id;

use crate::time::ts_at;
use crate::{Db, StoreError};

/// Маркеры подсветки в `snippet` (Private Use Area: в тексте заметок не встречаются,
/// клиент заменяет их на свою разметку).
pub const MARK_START: char = '\u{E000}';
pub const MARK_END: char = '\u{E001}';

/// Константа RRF.
const RRF_K: f64 = 60.0;
/// Вес канала bm25 (unicode61).
const W_BM25: f64 = 1.0;
/// Вес канала trigram.
const W_TRIGRAM: f64 = 0.7;
/// Кандидатов из канала bm25.
const BM25_LIMIT: i64 = 50;
/// Кандидатов из канала trigram.
const TRIGRAM_LIMIT: i64 = 20;
/// Не больше чанков с одного узла (§9.2 п.6).
const PER_NODE_CAP: usize = 3;
/// Предел `top_k`.
const TOP_K_MAX: usize = 100;
/// Слов в запросе FTS: остальные отбрасываются, чтобы стоимость запроса была ограничена.
const MAX_TERMS: usize = 32;
/// Токенов в сниппете канала bm25.
const SNIPPET_TOKENS: i64 = 16;
/// Длина сниппета trigram-хита в символах (без многоточий).
const SNIPPET_CHARS: usize = 160;

/// Запрос полнотекстового поиска.
#[derive(Debug, Clone)]
pub struct FtsQuery {
    /// Текст запроса как прислан: синтаксис FTS5 в нём не действует.
    pub text: String,
    /// Разрешённые пространства; пусто → `BadInput`.
    pub space_ids: Vec<Id>,
    /// Префикс пути: сам узел `prefix` и всё под `prefix/`. Регистр учитывается.
    pub path_prefix: Option<String>,
    /// Размер выдачи, 1..=100.
    pub top_k: usize,
}

/// Найденный чанк.
#[derive(Debug, Clone)]
pub struct FtsHit {
    pub chunk_id: String,
    pub node_id: Id,
    pub space_id: Id,
    pub path: String,
    pub heading_path: String,
    /// Ревизия, по которой построен чанк.
    pub rev: i64,
    /// Слитый RRF-балл (больше — лучше).
    pub score: f64,
    pub updated_at: Timestamp,
    /// Фрагмент с совпадениями между [`MARK_START`] и [`MARK_END`].
    pub snippet: String,
    /// Полный текст чанка (обрезку под бюджет делает сервер).
    pub text: String,
}

/// Слова запроса: всё, что не буква и не цифра (Unicode), — разделитель.
fn terms(query: &str) -> Vec<&str> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_TERMS)
        .collect()
}

/// Слово как префиксный терм FTS5 в кавычках: ключевые слова и операторы теряют смысл.
fn quote_prefix(t: &str) -> String {
    format!("\"{}\"*", t.replace('"', "\"\""))
}

/// Безопасный запрос FTS5: каждое слово — `"слово"*`, слова через пробел (неявный AND).
/// Синтаксис FTS5 во входе (`"`, `*`, `AND`, `-`, `:`, `(`, `NEAR`) не действует.
/// `None` — в запросе нет слов.
pub fn sanitize_fts(query: &str) -> Option<String> {
    join_terms(query, " ")
}

/// Ослабленный вариант [`sanitize_fts`]: слова через `OR`.
fn sanitize_fts_any(query: &str) -> Option<String> {
    join_terms(query, " OR ")
}

fn join_terms(query: &str, sep: &str) -> Option<String> {
    let t = terms(query);
    if t.is_empty() {
        return None;
    }
    Some(
        t.into_iter()
            .map(quote_prefix)
            .collect::<Vec<_>>()
            .join(sep),
    )
}

/// Есть ли в запросе идентификатор или код: токен (через пробелы) длиной ≥ 3
/// символов с `_ . / - :` или цифрами.
fn has_identifier(query: &str) -> bool {
    query.split_whitespace().any(|t| {
        t.chars().count() >= 3
            && t.chars()
                .any(|ch| matches!(ch, '_' | '.' | '/' | '-' | ':') || ch.is_ascii_digit())
    })
}

/// Запрос канала trigram: вся фраза в кавычках. `None` — короче 3 символов
/// (trigram такие подстроки не ищет).
fn trigram_query(query: &str) -> Option<String> {
    let q = query.trim();
    (q.chars().count() >= 3).then(|| format!("\"{}\"", q.replace('"', "\"\"")))
}

/// Кандидат одного канала.
struct Cand {
    chunk_id: String,
    node_id: Id,
    space_id: Id,
    path: String,
    heading_path: String,
    rev: i64,
    updated_at: Timestamp,
    text: String,
    /// Сниппет FTS5 (только канал bm25).
    snippet: Option<String>,
}

/// Канал поиска.
#[derive(Clone, Copy)]
enum Channel {
    Bm25,
    Trigram,
}

/// SQL канала. Параметры: `?1` — MATCH, `?2` — префикс пути или NULL,
/// `?3` и `?4` — маркеры сниппета (только bm25), далее — пространства.
fn channel_sql(ch: Channel, n_spaces: usize) -> String {
    let first_space = match ch {
        Channel::Bm25 => 5,
        Channel::Trigram => 3,
    };
    let spaces = (first_space..first_space + n_spaces)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let (table, snippet, order, limit) = match ch {
        Channel::Bm25 => (
            "chunks_fts",
            format!("snippet(chunks_fts, 0, ?3, ?4, '…', {SNIPPET_TOKENS})"),
            "bm25(chunks_fts, 1.0, 0.5, 0.3)",
            BM25_LIMIT,
        ),
        Channel::Trigram => (
            "chunks_fts_tri",
            "NULL".to_string(),
            "bm25(chunks_fts_tri)",
            TRIGRAM_LIMIT,
        ),
    };
    // префикс пути сравнивается через substr, а не LIKE: LIKE в SQLite
    // нечувствителен к регистру ASCII и требует экранирования `%` и `_`
    format!(
        "SELECT c.id, c.node_id, n.space_id, n.path, c.heading_path, c.rev, n.updated_at,
                c.text, {snippet}
         FROM {table} f
         JOIN chunks c ON c.seq = f.rowid
         JOIN nodes n ON n.id = c.node_id
         WHERE {table} MATCH ?1
           AND n.deleted_at IS NULL
           AND n.space_id IN ({spaces})
           AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/')
         ORDER BY {order}, c.seq
         LIMIT {limit}"
    )
}

/// Выполняет канал; результат упорядочен по релевантности канала.
fn run_channel(
    c: &Connection,
    ch: Channel,
    fts: &str,
    prefix: Option<&str>,
    spaces: &[Id],
) -> Result<Vec<Cand>, StoreError> {
    let (ms, me) = (MARK_START.to_string(), MARK_END.to_string());
    let mut args: Vec<&dyn ToSql> = vec![&fts, &prefix];
    if let Channel::Bm25 = ch {
        args.push(&ms);
        args.push(&me);
    }
    args.extend(spaces.iter().map(|s| s as &dyn ToSql));
    let mut st = c.prepare_cached(&channel_sql(ch, spaces.len()))?;
    let rows = st.query_map(params_from_iter(args), |r| {
        Ok(Cand {
            chunk_id: r.get(0)?,
            node_id: r.get(1)?,
            space_id: r.get(2)?,
            path: r.get(3)?,
            heading_path: r.get(4)?,
            rev: r.get(5)?,
            updated_at: ts_at(r, 6)?,
            text: r.get(7)?,
            snippet: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Полнотекстовый поиск по чанкам живых узлов в пределах `space_ids` и `path_prefix`.
/// Запрос без слов (и без фразы для trigram) — пустая выдача.
/// `top_k` вне 1..=100 или пустой `space_ids` → `BadInput`.
pub async fn search_fts(db: &Db, q: FtsQuery) -> Result<Vec<FtsHit>, StoreError> {
    if !(1..=TOP_K_MAX).contains(&q.top_k) {
        return Err(StoreError::BadInput(format!(
            "top_k должен быть в 1..={TOP_K_MAX}, получено {}",
            q.top_k
        )));
    }
    if q.space_ids.is_empty() {
        return Err(StoreError::BadInput(
            "не задано ни одного пространства".into(),
        ));
    }
    let mut spaces = q.space_ids.clone();
    spaces.sort();
    spaces.dedup();
    let prefix = q
        .path_prefix
        .as_deref()
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string);

    db.read(move |c| {
        let prefix = prefix.as_deref();
        // канал A: строгий запрос, при пустой выдаче — ослабленный
        let mut a = match sanitize_fts(&q.text) {
            Some(fts) => run_channel(c, Channel::Bm25, &fts, prefix, &spaces)?,
            None => Vec::new(),
        };
        if a.is_empty()
            && terms(&q.text).len() > 1
            && let Some(fts) = sanitize_fts_any(&q.text)
        {
            a = run_channel(c, Channel::Bm25, &fts, prefix, &spaces)?;
        }
        // канал B
        let b = match trigram_query(&q.text) {
            Some(fts) if a.is_empty() || has_identifier(&q.text) => {
                run_channel(c, Channel::Trigram, &fts, prefix, &spaces)?
            }
            _ => Vec::new(),
        };
        Ok(merge(a, b, q.top_k, q.text.trim()))
    })
    .await
}

/// Weighted RRF по чанкам, лимит на узел, top_k; недостающие сниппеты — из текста.
fn merge(a: Vec<Cand>, b: Vec<Cand>, top_k: usize, needle: &str) -> Vec<FtsHit> {
    let mut by_chunk: HashMap<String, (Cand, f64)> = HashMap::new();
    for (w, list) in [(W_BM25, a), (W_TRIGRAM, b)] {
        for (i, cand) in list.into_iter().enumerate() {
            let s = w / (RRF_K + (i + 1) as f64);
            match by_chunk.get_mut(&cand.chunk_id) {
                // сниппет канала bm25 (пришедшего первым) сохраняется
                Some((_, score)) => *score += s,
                None => {
                    by_chunk.insert(cand.chunk_id.clone(), (cand, s));
                }
            }
        }
    }
    let mut ranked: Vec<(Cand, f64)> = by_chunk.into_values().collect();
    ranked.sort_by(|(x, sx), (y, sy)| sy.total_cmp(sx).then_with(|| x.chunk_id.cmp(&y.chunk_id)));

    let mut per_node: HashMap<Id, usize> = HashMap::new();
    ranked
        .into_iter()
        .filter(|(cand, _)| {
            let n = per_node.entry(cand.node_id).or_default();
            *n += 1;
            *n <= PER_NODE_CAP
        })
        .take(top_k)
        .map(|(cand, score)| FtsHit {
            snippet: cand
                .snippet
                .unwrap_or_else(|| text_snippet(&cand.text, needle)),
            chunk_id: cand.chunk_id,
            node_id: cand.node_id,
            space_id: cand.space_id,
            path: cand.path,
            heading_path: cand.heading_path,
            rev: cand.rev,
            score,
            updated_at: cand.updated_at,
            text: cand.text,
        })
        .collect()
}

/// Сниппет из текста: до [`SNIPPET_CHARS`] символов вокруг первого вхождения
/// `needle` без учёта регистра, вхождение — между маркерами. Не нашлось —
/// начало текста. Режется только по границам символов.
fn text_snippet(text: &str, needle: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pat: Vec<char> = needle.chars().collect();
    let same = |x: char, y: char| x == y || x.to_lowercase().eq(y.to_lowercase());
    let found = if pat.is_empty() || pat.len() > chars.len() {
        None
    } else {
        (0..=chars.len() - pat.len())
            .find(|&i| pat.iter().zip(&chars[i..]).all(|(&p, &t)| same(p, t)))
    };
    let (start, len) = found.map_or((0, 0), |i| (i, pat.len()));
    let budget = SNIPPET_CHARS.saturating_sub(len);
    let mut from = start.saturating_sub(budget / 2);
    let to = (start + len + (budget - (start - from))).min(chars.len());
    // остаток бюджета у конца текста отдаётся началу
    from = from.min(start.saturating_sub(budget.saturating_sub(to - (start + len))));

    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    s.extend(&chars[from..start]);
    if len > 0 {
        s.push(MARK_START);
        s.extend(&chars[start..start + len]);
        s.push(MARK_END);
    }
    s.extend(&chars[start + len..to]);
    if to < chars.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_detection() {
        for s in [
            "core.autocrlf",
            "a_b",
            "src/main",
            "x-y",
            "a:b",
            "v42",
            "abc 123",
        ] {
            assert!(has_identifier(s), "{s}");
        }
        for s in ["скрипт bash", "a.", "12", "просто слова"] {
            assert!(!has_identifier(s), "{s}");
        }
    }

    #[test]
    fn trigram_query_quotes_and_requires_three_chars() {
        assert_eq!(trigram_query(" a\"b "), Some("\"a\"\"b\"".into()));
        assert_eq!(trigram_query("ab"), None);
        assert_eq!(sanitize_fts_any("a b").as_deref(), Some(r#""a"* OR "b"*"#));
    }

    #[test]
    fn snippet_window() {
        let m = |s: &str| format!("{MARK_START}{s}{MARK_END}");
        assert_eq!(
            text_snippet("Hello World", "WORLD"),
            format!("Hello {}", m("World"))
        );
        assert_eq!(text_snippet("abc", "zzz"), "abc");
        let long = "я".repeat(500);
        let s = text_snippet(&format!("{long}КЛЮЧ{long}"), "ключ");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.contains(&m("КЛЮЧ")));
        assert_eq!(s.chars().count(), SNIPPET_CHARS + 4);
        // у конца текста окно сдвигается влево
        let s = text_snippet(&format!("{long}ключ"), "ключ");
        assert_eq!(s.chars().count(), 1 + SNIPPET_CHARS + 2);
        assert!(s.ends_with(MARK_END));
    }
}
