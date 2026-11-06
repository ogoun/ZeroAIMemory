//! Явное фокусное чтение `POST /read {items: [{id, focus}]}` (M6 §6, решение владельца 6).
//!
//! Фокус — только по явному `focus` агента; без него `/read` прежний (L2 целиком). Выдача цели:
//! - **строка оглавления** `Оглавление: A · *B · … · …ещё N заголовков` — заголовки H1–H3 заметки
//!   (у документа — части по `locator_text`); `*` — у записей, чей текст выдан; не больше
//!   [`TOC_MAX_ENTRIES`] записей и [`TOC_MAX_TOKENS`] токенов; отмеченные дальше предела — окном
//!   вокруг них (`…ещё N выше · …`);
//! - **выданные куски** — лучшие листы по `focus` (не больше `read.focus_leaves`) в порядке
//!   документа. У заметки кусок — **байты текста ревизии** от начала до конца листа (листы
//!   находятся в тексте по порядку; перекрытие соседних и строки заголовков между ними — одним
//!   куском; кусок начинается со строк заголовков перед ним). У документа — чанки с `[подпись]`;
//! - **пропуски** `[пропущено: <адрес> (<подпись>) · ~N t]` — один на промежуток между выданными
//!   кусками (и по краям), всего не больше `focus_leaves + 1`. Адрес ведёт ровно к пропущенному:
//!   у заметки `range:S-E` — байты `[S, E)` текста той же ревизии (`/read {id, range}` отдаёт
//!   их), подпись — первая и последняя секция H1–H3, чей собственный текст пересекает промежуток,
//!   и число таких секций (`начало` — текст до первого заголовка); у документа — `locator` первой
//!   и последней части (`page:3 — page:7`; чтение по `locator` отдаёт единицы целиком), подпись —
//!   `locator_text` справочно. `N` — токены пропущенного счётчиком сервера ([`count_tokens`]).
//!   У заметки куски и пропуски вместе — весь текст ревизии без пересечений (`focus_parts` в
//!   json — адреса по порядку).
//!
//! **Балл листа** — RRF (k = 60) двух порядков: FTS-лексика (число слов `focus`, найденных в
//! тексте и пути заголовков листа, [`zam_store::focus_leaves`]) и dense-косинус листа (эмбеддинг
//! запроса и векторы листов из кэша `embeddings` SQLite — живой Qdrant не нужен, только
//! загруженная модель), без реранкера. Лексика без совпадений — первые листы. **Бюджет**
//! (`max_tokens` элемента или бюджет пачки): листы отбираются по баллу, пока куски и резерв
//! пометок влезают (первый — всегда); строка оглавления — на остаток.
//!
//! **Срок** `read.focus_timeout_ms` — один на весь вызов `/read` ([`FocusCtx`]): эмбеддинги
//! различных текстов `focus` запускаются в начале вызова (параллельно чтению целей), каждая цель
//! ждёт эмбеддинг и векторы листов не дольше остатка срока. Не успели, модели нет или векторов
//! листов нет в кэше — только лексика и `focus lexical`. Эмбеддинг запроса, уже готовый,
//! переиспользуется и после сбоя чтения векторов другой цели.
//!
//! Целиком без фокуса: листов не больше `focus_leaves` (`focus skipped: short`); листов нет,
//! нарезка другой ревизии или лист не нашёлся в тексте (секрет в листе: нарезка — по
//! маскированному тексту) — `focus skipped: not indexed`.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_docs::Locator;
use zam_markdown::parse;
use zam_store::{Db, FocusLeaf, StoreError, count_tokens, focus_leaves, get_embeddings};

use super::doc_locator::Sheets;
use super::search::md_inline;
use crate::app::AppState;
use crate::config::ReadSection;

/// Предупреждение: цель короче `read.focus_min_tokens` (или листов не больше `focus_leaves`),
/// запись журнала или фрагмент `a:` — выдана целиком, `focus` проигнорирован.
pub const FOCUS_SKIPPED: &str = "focus skipped: short";
/// Предупреждение: листов нет, нарезка отстала от ревизии или не сопоставилась с текстом —
/// выдана целиком.
pub const FOCUS_NOT_INDEXED: &str = "focus skipped: not indexed";
/// Предупреждение: фокус только по FTS-лексике листов (срок, нет модели, нет векторов).
pub const FOCUS_LEXICAL: &str = "focus lexical";
/// Предел записей строки оглавления.
pub const TOC_MAX_ENTRIES: usize = 40;
/// Предел токенов строки оглавления.
pub const TOC_MAX_TOKENS: usize = 150;
/// Меньше остатка бюджета — строки оглавления нет.
const TOC_MIN_TOKENS: usize = 15;
/// Предел символов заголовка в оглавлении.
const TOC_TITLE_CHARS: usize = 60;
/// Резерв токенов на одну пометку пропуска при отборе листов.
const MARKER_RESERVE: usize = 40;
/// Резерв токенов на строки заголовков перед куском при отборе листов.
const SEGMENT_HEAD_RESERVE: usize = 15;
/// Константа RRF слияния лексического и векторного порядков.
const RRF_K: f64 = 60.0;
/// Подпись текста до первого заголовка.
const PREAMBLE: &str = "начало";
/// Адрес части документа без `locator` (её даёт только оглавление документа).
const NO_LOCATOR: &str = "оглавление";

/// Цель фокуса.
pub enum FocusTarget<'a> {
    /// Узел: полный текст ревизии `rev` (секции и байтовые адреса — по нему).
    Note {
        db: &'a Db,
        node: Id,
        text: &'a str,
        rev: i64,
    },
    /// Документ вложения `att` (листы — его чанки, адреса — `locator` через `sheets`).
    Doc {
        db: &'a Db,
        att: Id,
        sheets: &'a Sheets,
    },
}

/// Часть выдачи по порядку: адрес (`range:S-E` или `locator`), выдана ли, токены.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusPart {
    pub address: String,
    pub shown: bool,
    pub tokens: usize,
}

impl FocusPart {
    pub fn json(&self) -> Value {
        json!({"part": self.address, "shown": self.shown, "tokens": self.tokens})
    }
}

/// Итог фокуса цели.
#[derive(Debug)]
pub struct FocusOut {
    /// Оглавление, куски и пропуски.
    pub text: String,
    /// `chunk_id` выданных листов (отметка `session_seen`, `part = chunk_id`).
    pub leaves: Vec<String>,
    /// `focus lexical` или пусто.
    pub warnings: Vec<String>,
    pub parts: Vec<FocusPart>,
}

/// Фокус цели или причина выдать её целиком.
#[derive(Debug)]
pub enum FocusResult {
    Focused(FocusOut),
    /// Листов не больше `focus_leaves` — [`FOCUS_SKIPPED`].
    Short,
    /// [`FOCUS_NOT_INDEXED`].
    NotIndexed,
}

/// Эмбеддинг текста `focus`: идёт или готов (`None` — не удался).
enum Query {
    Pending(tokio::task::JoinHandle<Option<Vec<f32>>>),
    Ready(Option<Vec<f32>>),
}

/// Фокус одного вызова `/read`: настройки, общий срок и эмбеддинги текстов `focus`, запущенные в
/// начале вызова.
pub struct FocusCtx<'a> {
    st: &'a AppState,
    cfg: &'a ReadSection,
    deadline: Instant,
    queries: HashMap<String, Query>,
}

impl<'a> FocusCtx<'a> {
    /// Срок — от этого момента; эмбеддинги различных `focus` запускаются сразу (модели нет —
    /// не запускаются).
    pub fn new<'f>(st: &'a AppState, focuses: impl IntoIterator<Item = &'f str>) -> FocusCtx<'a> {
        let cfg = &st.cfg.read;
        let deadline = Instant::now() + Duration::from_millis(cfg.focus_timeout_ms);
        let mut queries = HashMap::new();
        if let Some(emb) = st.index.embedder.clone() {
            for f in focuses {
                if queries.contains_key(f) {
                    continue;
                }
                let (emb, text) = (emb.clone(), f.to_string());
                #[cfg(feature = "test-hooks")]
                let delay = st.focus_delay_ms.load(std::sync::atomic::Ordering::SeqCst);
                let h = tokio::spawn(async move {
                    #[cfg(feature = "test-hooks")]
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    match tokio::task::spawn_blocking(move || emb.embed_query(&text)).await {
                        Ok(Ok(e)) => Some(e.dense),
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "фокус: эмбеддинг запроса не удался");
                            None
                        }
                        Err(j) => {
                            tracing::error!(panic = j.is_panic(), "фокус: эмбеддинг аварийно");
                            None
                        }
                    }
                });
                queries.insert(f.to_string(), Query::Pending(h));
            }
        }
        FocusCtx {
            st,
            cfg,
            deadline,
            queries,
        }
    }

    /// Порог «короткой» цели, токенов полного текста (`read.focus_min_tokens`).
    pub fn min_tokens(&self) -> usize {
        self.cfg.focus_min_tokens
    }

    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Эмбеддинг запроса не дольше остатка срока (`None` — нет модели, сбой, срок).
    async fn query(&mut self, focus: &str) -> Option<Vec<f32>> {
        let left = self.left();
        let q = self.queries.get_mut(focus)?;
        if let Query::Pending(h) = q {
            let v = match tokio::time::timeout(left, &mut *h).await {
                Ok(Ok(v)) => v,
                Ok(Err(_)) => None,
                // срок общий: у следующих целей его не больше
                Err(_) => {
                    h.abort();
                    None
                }
            };
            *q = Query::Ready(v);
        }
        match q {
            Query::Ready(v) => v.clone(),
            Query::Pending(_) => None,
        }
    }
}

impl Drop for FocusCtx<'_> {
    fn drop(&mut self) {
        for q in self.queries.values() {
            if let Query::Pending(h) = q {
                h.abort();
            }
        }
    }
}

/// Фокус цели по тексту `focus` в пределах `limit` токенов текста элемента.
pub async fn focus_read(
    fx: &mut FocusCtx<'_>,
    target: &FocusTarget<'_>,
    focus: &str,
    limit: usize,
) -> Result<FocusResult, StoreError> {
    let (db, tref) = match target {
        FocusTarget::Note { db, node, .. } => (
            *db,
            TargetRef {
                kind: TargetKind::Node,
                id: *node,
            },
        ),
        FocusTarget::Doc { db, att, .. } => (
            *db,
            TargetRef {
                kind: TargetKind::Attachment,
                id: *att,
            },
        ),
    };
    let leaves = focus_leaves(db, tref, zam_store::query_needles(focus)).await?;
    if leaves.is_empty() {
        return Ok(FocusResult::NotIndexed);
    }
    if let FocusTarget::Note { rev, .. } = target
        && leaves.iter().any(|l| l.rev != *rev)
    {
        return Ok(FocusResult::NotIndexed);
    }
    let n = fx.cfg.focus_leaves;
    if leaves.len() <= n {
        return Ok(FocusResult::Short);
    }
    let ranges = match target {
        FocusTarget::Note { text, .. } => match locate(text, &leaves) {
            Some(r) => Some(r),
            None => return Ok(FocusResult::NotIndexed),
        },
        FocusTarget::Doc { .. } => None,
    };
    let mut warnings = Vec::new();
    let cos = dense_scores(fx, db, focus, &leaves).await;
    if cos.is_none() {
        warnings.push(FOCUS_LEXICAL.to_string());
    }
    let ranked = rank(&leaves, cos.as_ref());
    let out = match (target, ranges) {
        (FocusTarget::Note { text, .. }, Some(r)) => {
            match note_focus(
                text,
                &leaves,
                &r,
                &ranked,
                n,
                limit,
                fx.min_tokens(),
                warnings,
            ) {
                Some(o) => o,
                None => return Ok(FocusResult::Short),
            }
        }
        (FocusTarget::Doc { sheets, .. }, _) => {
            doc_focus(sheets, &leaves, &ranked, n, limit, warnings)
        }
        (FocusTarget::Note { .. }, None) => return Ok(FocusResult::NotIndexed),
    };
    Ok(FocusResult::Focused(out))
}

/// Косинусы листов к запросу (`None` — лексика: нет модели или векторов, срок вышел).
async fn dense_scores(
    fx: &mut FocusCtx<'_>,
    db: &Db,
    focus: &str,
    leaves: &[FocusLeaf],
) -> Option<HashMap<usize, f32>> {
    let model = fx.st.index.model_id()?.to_string();
    let q = fx.query(focus).await?;
    let mut keys: Vec<String> = leaves
        .iter()
        .map(|l| l.embed_key.clone())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort();
    keys.dedup();
    let embs = match tokio::time::timeout(fx.left(), get_embeddings(db, &model, &keys)).await {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "фокус: векторы листов не прочитаны — лексика");
            return None;
        }
        Err(_) => return None,
    };
    let cos: HashMap<usize, f32> = leaves
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let e = embs.get(&l.embed_key)?;
            Some((i, q.iter().zip(&e.dense).map(|(a, b)| a * b).sum::<f32>()))
        })
        .collect();
    // ни одного вектора в кэше (цель не проиндексирована) — лексика
    (!cos.is_empty()).then_some(cos)
}

/// Листы с баллом по убыванию: RRF лексики и косинуса; без совпадений и векторов — все по порядку
/// документа.
fn rank(leaves: &[FocusLeaf], cos: Option<&HashMap<usize, f32>>) -> Vec<usize> {
    let mut score: HashMap<usize, f64> = HashMap::new();
    let mut lex: Vec<usize> = (0..leaves.len()).filter(|&i| leaves[i].hits > 0).collect();
    lex.sort_by(|&a, &b| leaves[b].hits.cmp(&leaves[a].hits).then(a.cmp(&b)));
    for (r, &i) in lex.iter().enumerate() {
        *score.entry(i).or_default() += 1.0 / (RRF_K + r as f64 + 1.0);
    }
    if let Some(cos) = cos {
        let mut d: Vec<(usize, f32)> = cos.iter().map(|(&i, &c)| (i, c)).collect();
        d.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        for (r, (i, _)) in d.iter().enumerate() {
            *score.entry(*i).or_default() += 1.0 / (RRF_K + r as f64 + 1.0);
        }
    }
    let mut v: Vec<(usize, f64)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    if v.is_empty() {
        // лексика без совпадений и без векторов — первые листы
        return (0..leaves.len()).collect();
    }
    v.into_iter().map(|(i, _)| i).collect()
}

// ---- заметка: байтовые адреса ----

/// Следующая граница символа после `i`.
fn next_char(text: &str, i: usize) -> usize {
    i + text[i..].chars().next().map_or(1, char::len_utf8)
}

/// Байты листов в тексте `text` по порядку (поиск с начала предыдущего листа: соседние
/// перекрываются). Лист — `src[range]` или повторённый заголовок блока + `src[range]` (тогда ищется
/// без первых строк). Не нашёлся — `None`.
fn locate(text: &str, leaves: &[FocusLeaf]) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::with_capacity(leaves.len());
    let mut from = 0usize;
    for l in leaves {
        let t = l.text.as_str();
        let mut cands: Vec<&str> = vec![t, t.trim()];
        let mut rest = t;
        for _ in 0..3 {
            match rest.split_once('\n') {
                Some((_, r)) => {
                    rest = r;
                    cands.push(rest.trim());
                }
                None => break,
            }
        }
        let (pos, len) = cands
            .into_iter()
            .filter(|c| !c.is_empty())
            .find_map(|c| text.get(from..)?.find(c).map(|p| (from + p, c.len())))?;
        out.push((pos, pos + len));
        from = next_char(text, pos).min(text.len());
    }
    Some(out)
}

/// Строка пустая или заголовок ATX.
fn heading_or_blank(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

/// В `text[a..b]` только пустые строки и строки заголовков.
fn only_headings(text: &str, a: usize, b: usize) -> bool {
    text[a..b].lines().all(heading_or_blank)
}

/// Куски заметки из выбранных листов: объединение перекрытий и промежутков из одних заголовков,
/// начало — со строк заголовков перед куском; промежутки из пробелов — к соседу.
fn segments(text: &str, ranges: &[(usize, usize)], chosen: &[usize]) -> Vec<(usize, usize)> {
    let mut r: Vec<(usize, usize)> = chosen.iter().map(|&i| ranges[i]).collect();
    r.sort_unstable();
    let mut segs: Vec<(usize, usize)> = Vec::new();
    for (s, e) in r {
        match segs.last_mut() {
            Some(last) if s <= last.1 || only_headings(text, last.1, s) => {
                last.1 = last.1.max(e);
            }
            _ => segs.push((s, e)),
        }
    }
    // начало куска — со строк заголовков и пустых строк перед ним (не заходя в предыдущий)
    let mut prev_end = 0usize;
    for seg in &mut segs {
        let mut s = seg.0;
        while s > prev_end && (s == 0 || text.as_bytes()[s - 1] == b'\n') {
            let line_start = text[..s - 1].rfind('\n').map_or(0, |i| i + 1).max(prev_end);
            if line_start >= s || !heading_or_blank(&text[line_start..s]) {
                break;
            }
            s = line_start;
        }
        // пустые строки перед первым заголовком куска — остаются в промежутке
        while s < seg.0 && matches!(text.as_bytes()[s], b'\n' | b'\r') {
            s += 1;
        }
        seg.0 = s;
        prev_end = seg.1;
    }
    // промежутки из пробелов — к куску
    let n = segs.len();
    for i in 0..n {
        let next = if i + 1 < n { segs[i + 1].0 } else { text.len() };
        if text[segs[i].1..next].trim().is_empty() {
            segs[i].1 = next;
        }
    }
    if let Some(first) = segs.first_mut()
        && text[..first.0].trim().is_empty()
    {
        first.0 = 0;
    }
    // слить соприкоснувшиеся
    let mut out: Vec<(usize, usize)> = Vec::new();
    for s in segs {
        match out.last_mut() {
            Some(l) if s.0 <= l.1 => l.1 = l.1.max(s.1),
            _ => out.push(s),
        }
    }
    out
}

/// Промежутки между кусками (и по краям) — весь остальной текст.
fn gaps(len: usize, segs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for &(s, e) in segs {
        if s > at {
            out.push((at, s));
        }
        at = e;
    }
    if at < len {
        out.push((at, len));
    }
    out
}

/// Секция H1–H3 заметки: путь, заголовок, собственный текст (до первой подсекции).
struct Sec {
    path: String,
    title: String,
    own: (usize, usize),
}

fn sections(text: &str) -> Vec<Sec> {
    let doc = parse(text);
    let all: Vec<&zam_markdown::Section> = doc.sections.iter().filter(|s| s.level >= 1).collect();
    all.iter()
        .enumerate()
        .map(|(i, s)| {
            let end = all.get(i + 1).map_or(s.range.end, |n| {
                n.range.start.min(s.range.end).max(s.range.start)
            });
            Sec {
                path: s.path.join(" > "),
                title: s.title.clone(),
                own: (s.range.start, end.max(s.range.start)),
            }
        })
        .collect()
}

/// Секции, чей собственный текст непусто пересекает `[a, b)`; текст до первой секции —
/// [`PREAMBLE`].
fn crossed(text: &str, secs: &[Sec], a: usize, b: usize) -> Vec<String> {
    let mut out = Vec::new();
    let first = secs.first().map_or(text.len(), |s| s.own.0);
    if a < first && !text[a..first.min(b)].trim().is_empty() {
        out.push(PREAMBLE.to_string());
    }
    for s in secs {
        let (x, y) = (a.max(s.own.0), b.min(s.own.1));
        if x < y && !text[x..y].trim().is_empty() {
            out.push(s.path.clone());
        }
    }
    out
}

/// Подпись пропуска: первая и последняя секция и их число.
fn gap_label(secs: &[String]) -> String {
    match secs {
        [] => PREAMBLE.to_string(),
        [one] => one.clone(),
        [first, .., last] => format!("{first} — {last}, секций: {}", secs.len()),
    }
}

fn skip_line(address: &str, label: &str, tokens: usize) -> String {
    format!("[пропущено: {address} ({label}) · ~{tokens} t]")
}

/// Стоимость выбора: токены кусков и резерв пометок.
/// Оценка без токенизации: `token_count` листов (счётчик чанкера), резерв на строки заголовков
/// куска и пометки.
fn note_cost(
    text: &str,
    leaves: &[FocusLeaf],
    ranges: &[(usize, usize)],
    chosen: &[usize],
) -> usize {
    let segs = segments(text, ranges, chosen);
    let shown: usize = chosen.iter().map(|&i| leaves[i].token_count).sum();
    shown + segs.len() * SEGMENT_HEAD_RESERVE + gaps(text.len(), &segs).len() * MARKER_RESERVE
}

/// Отбор по баллу в пределах бюджета: первый — всегда, дальше — пока влезает (не больше `n`;
/// проб не больше `3n`).
fn pick(ranked: &[usize], n: usize, limit: usize, cost: impl Fn(&[usize]) -> usize) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::new();
    for &i in ranked.iter().take(n * 3) {
        if chosen.len() >= n {
            break;
        }
        let mut c = chosen.clone();
        c.push(i);
        if chosen.is_empty() || cost(&c) <= limit {
            chosen = c;
        }
    }
    chosen.sort_unstable();
    chosen
}

/// Фокус заметки; весь текст (куски и пропуски) не длиннее `min_tokens` — `None` (целиком).
#[allow(clippy::too_many_arguments)]
fn note_focus(
    text: &str,
    leaves: &[FocusLeaf],
    ranges: &[(usize, usize)],
    ranked: &[usize],
    n: usize,
    limit: usize,
    min_tokens: usize,
    warnings: Vec<String>,
) -> Option<FocusOut> {
    let chosen = pick(ranked, n, limit, |c| note_cost(text, leaves, ranges, c));
    let segs = segments(text, ranges, &chosen);
    let gs = gaps(text.len(), &segs);
    let secs = sections(text);
    let mut parts: Vec<(usize, usize, bool)> = segs
        .iter()
        .map(|&(s, e)| (s, e, true))
        .chain(gs.iter().map(|&(s, e)| (s, e, false)))
        .collect();
    parts.sort_unstable();
    let mut body = String::new();
    let mut fparts = Vec::with_capacity(parts.len());
    for &(s, e, shown) in &parts {
        let piece = &text[s..e];
        let tokens = count_tokens(piece);
        let address = format!("range:{s}-{e}");
        if shown {
            body.push_str(piece);
        } else {
            let label = gap_label(&crossed(text, &secs, s, e));
            body.push_str(&skip_line(&address, &label, tokens));
        }
        if !body.ends_with('\n') {
            body.push('\n');
        }
        fparts.push(FocusPart {
            address,
            shown,
            tokens,
        });
    }
    // отмечены секции, чей текст выдан
    let marked: HashSet<usize> = secs
        .iter()
        .enumerate()
        .filter(|(_, sec)| {
            segs.iter().any(|&(s, e)| {
                let (x, y) = (s.max(sec.own.0), e.min(sec.own.1));
                x < y && !text[x..y].trim().is_empty()
            })
        })
        .map(|(i, _)| i)
        .collect();
    // полный текст короче порога (токены частей — весь текст) — целиком
    if fparts.iter().map(|p| p.tokens).sum::<usize>() <= min_tokens {
        return None;
    }
    let titles: Vec<String> = secs.iter().map(|s| s.title.clone()).collect();
    let shown: usize = fparts.iter().filter(|p| p.shown).map(|p| p.tokens).sum();
    let room = limit.saturating_sub(shown + count_tokens_markers(&body));
    let out = with_toc(&titles, &marked, "заголовков", room, body);
    Some(FocusOut {
        text: out,
        leaves: chosen.iter().map(|&i| leaves[i].chunk_id.clone()).collect(),
        warnings,
        parts: fparts,
    })
}

/// Токены строк пометок текста (куски уже посчитаны частями).
fn count_tokens_markers(body: &str) -> usize {
    body.lines()
        .filter(|l| l.starts_with("[пропущено: "))
        .map(count_tokens)
        .sum()
}

/// Строка оглавления (если влезает в `room`) и текст.
fn with_toc(
    titles: &[String],
    marked: &HashSet<usize>,
    noun: &str,
    room: usize,
    body: String,
) -> String {
    let max = room.min(TOC_MAX_TOKENS);
    match (max >= TOC_MIN_TOKENS)
        .then(|| toc_line(titles, marked, noun, max))
        .flatten()
    {
        Some(line) => format!("{line}\n{body}"),
        None => body,
    }
}

// ---- документ: адреса `locator` ----

/// Подпись части документа для оглавления: у листа таблицы — без диапазона (части — листы
/// книги, как в оглавлении `/read` без `locator`).
fn part_label(sheets: &Sheets, l: &Locator) -> String {
    let label = sheets.shown(l).1;
    match l {
        Locator::Sheet { .. } | Locator::SheetNo { .. } => label
            .rsplit_once(' ')
            .map_or(label.clone(), |(a, _)| a.to_string()),
        _ => label,
    }
}

/// Адрес и подпись чанка документа (`page:3`, `стр. 3`); без `locator` — [`NO_LOCATOR`].
fn chunk_address(sheets: &Sheets, l: &FocusLeaf) -> (String, String) {
    match l.locator.as_deref().and_then(|r| Locator::parse(r).ok()) {
        Some(loc) => sheets.shown(&loc),
        None => (NO_LOCATOR.to_string(), NO_LOCATOR.to_string()),
    }
}

/// `a — b` или `a`.
fn span(first: &str, last: &str) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first} — {last}")
    }
}

fn doc_focus(
    sheets: &Sheets,
    leaves: &[FocusLeaf],
    ranked: &[usize],
    n: usize,
    limit: usize,
    warnings: Vec<String>,
) -> FocusOut {
    let tokens: Vec<usize> = leaves.iter().map(|l| count_tokens(l.text.trim())).collect();
    let addr: Vec<(String, String)> = leaves.iter().map(|l| chunk_address(sheets, l)).collect();
    let runs = |c: &[usize]| -> usize {
        let set: HashSet<usize> = c.iter().copied().collect();
        (0..leaves.len())
            .filter(|&i| !set.contains(&i) && (i == 0 || set.contains(&(i - 1))))
            .count()
    };
    let chosen = pick(ranked, n, limit, |c| {
        c.iter().map(|&i| tokens[i] + 8).sum::<usize>() + runs(c) * MARKER_RESERVE
    });
    let set: HashSet<usize> = chosen.iter().copied().collect();
    // оглавление: части по порядку, отмечены части выданных чанков
    let mut titles: Vec<String> = Vec::new();
    let mut by_part: HashMap<String, usize> = HashMap::new();
    let mut marked: HashSet<usize> = HashSet::new();
    for (i, l) in leaves.iter().enumerate() {
        if let Some(loc) = l.locator.as_deref().and_then(|r| Locator::parse(r).ok()) {
            let p = part_label(sheets, &loc);
            let k = *by_part.entry(p.clone()).or_insert_with(|| {
                titles.push(p);
                titles.len() - 1
            });
            if set.contains(&i) {
                marked.insert(k);
            }
        }
    }
    let mut body = String::new();
    let mut fparts = Vec::new();
    let mut i = 0;
    let mut last_label: Option<&str> = None;
    while i < leaves.len() {
        let shown = set.contains(&i);
        let mut j = i;
        while j + 1 < leaves.len() && set.contains(&(j + 1)) == shown {
            j += 1;
        }
        let t: usize = tokens[i..=j].iter().sum();
        let address = span(&addr[i].0, &addr[j].0);
        if shown {
            for k in i..=j {
                if last_label != Some(addr[k].1.as_str()) {
                    body.push_str(&format!("[{}]\n", addr[k].1));
                }
                last_label = Some(addr[k].1.as_str());
                body.push_str(leaves[k].text.trim());
                body.push('\n');
            }
        } else {
            body.push_str(&skip_line(&address, &span(&addr[i].1, &addr[j].1), t));
            body.push('\n');
            last_label = None;
        }
        fparts.push(FocusPart {
            address,
            shown,
            tokens: t,
        });
        i = j + 1;
    }
    let room = limit.saturating_sub(count_tokens(&body));
    let out = with_toc(&titles, &marked, "частей", room, body);
    FocusOut {
        text: out,
        leaves: chosen.iter().map(|&i| leaves[i].chunk_id.clone()).collect(),
        warnings,
        parts: fparts,
    }
}

// ---- оглавление ----

/// Заголовок оглавления: одна строка, не длиннее [`TOC_TITLE_CHARS`] символов.
fn toc_title(t: &str) -> String {
    let t = md_inline(t);
    if t.chars().count() <= TOC_TITLE_CHARS {
        return t;
    }
    let mut s: String = t.chars().take(TOC_TITLE_CHARS).collect();
    s.push('…');
    s
}

/// Строка оглавления: записи по порядку, отмеченные — `*`; не больше [`TOC_MAX_ENTRIES`]
/// записей и `max` токенов. Окно — с начала, а если первая отмеченная за пределом — с неё (на
/// две записи раньше): `…ещё N выше · … · …ещё M <noun>`. Записей нет — `None`.
fn toc_line(titles: &[String], marked: &HashSet<usize>, noun: &str, max: usize) -> Option<String> {
    if titles.is_empty() {
        return None;
    }
    let items: Vec<String> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let t = toc_title(t);
            if marked.contains(&i) {
                format!("*{t}")
            } else {
                t
            }
        })
        .collect();
    let line = |from: usize, k: usize| {
        let mut parts: Vec<String> = Vec::new();
        if from > 0 {
            parts.push(format!("…ещё {from} выше"));
        }
        parts.extend(items[from..from + k].iter().cloned());
        if from + k < items.len() {
            parts.push(format!("…ещё {} {noun}", items.len() - from - k));
        }
        format!("Оглавление: {}", parts.join(" · "))
    };
    // оценка суммой токенов записей (каждая считается один раз), затем точная проверка строки
    let cost: Vec<usize> = items
        .iter()
        .map(|t| count_tokens(&format!(" · {t}")))
        .collect();
    let head = count_tokens("Оглавление: ")
        + count_tokens(&format!(" · …ещё {} выше", items.len()))
        + count_tokens(&format!(" · …ещё {} {noun}", items.len()));
    let fit = |from: usize| {
        let mut k = 0;
        let mut sum = head;
        while from + k < items.len() && k < TOC_MAX_ENTRIES && sum + cost[from + k] <= max {
            sum += cost[from + k];
            k += 1;
        }
        while k > 0 && count_tokens(&line(from, k)) > max {
            k -= 1;
        }
        k
    };
    let mut from = 0;
    let mut k = fit(0);
    if let Some(&m) = marked.iter().min()
        && m >= k
    {
        from = m.saturating_sub(2);
        k = fit(from);
    }
    (k > 0).then(|| line(from, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(i: i64, hits: usize, text: &str) -> FocusLeaf {
        FocusLeaf {
            chunk_id: format!("c{i}"),
            heading_path: String::new(),
            ordinal: i,
            locator: None,
            text: text.into(),
            token_count: 10,
            rev: 1,
            embed_key: String::new(),
            hits,
        }
    }

    /// Лексика: по числу совпадений, при равенстве — раньше; RRF с косинусом; без совпадений —
    /// по порядку документа.
    #[test]
    fn rank_lexical_and_rrf() {
        let l = vec![
            leaf(0, 0, "a"),
            leaf(1, 1, "b"),
            leaf(2, 3, "c"),
            leaf(3, 1, "d"),
        ];
        assert_eq!(rank(&l, None), [2, 1, 3]);
        let none: Vec<FocusLeaf> = (0..4).map(|i| leaf(i, 0, "x")).collect();
        assert_eq!(rank(&none, None), [0, 1, 2, 3]);
        // лист 1 — второй в обоих порядках — обходит лист 0, первый только по косинусу
        let cos: HashMap<usize, f32> = [(0, 0.9), (1, 0.1), (2, 0.2), (3, 0.0)].into();
        assert_eq!(&rank(&l, Some(&cos))[..2], [2, 1]);
        let cos: HashMap<usize, f32> = [(0, 0.1), (1, 0.2), (2, 0.3), (3, 0.9)].into();
        assert_eq!(rank(&none, Some(&cos))[0], 3);
    }

    /// Оглавление — не больше 40 записей и `max` токенов; отмеченная дальше предела — окном.
    #[test]
    fn toc_limits_and_window() {
        let titles: Vec<String> = (0..100).map(|i| format!("Заголовок {i}")).collect();
        let line = toc_line(&titles, &[1].into(), "заголовков", 150).unwrap();
        assert!(count_tokens(&line) <= TOC_MAX_TOKENS, "{line}");
        assert!(line.contains("*Заголовок 1 ·"), "{line}");
        let shown = line.split(" · ").count() - 1;
        assert!(shown <= TOC_MAX_ENTRIES, "{line}");
        assert!(
            line.ends_with(&format!("…ещё {} заголовков", 100 - shown)),
            "{line}"
        );
        let far = toc_line(&titles, &[90].into(), "заголовков", 150).unwrap();
        assert!(
            far.contains("…ещё 88 выше · Заголовок 88 · Заголовок 89 · *Заголовок 90"),
            "{far}"
        );
        assert!(count_tokens(&far) <= 150, "{far}");
        let short = toc_line(&titles[..2], &HashSet::new(), "заголовков", 150).unwrap();
        assert_eq!(short, "Оглавление: Заголовок 0 · Заголовок 1");
        assert!(toc_line(&[], &HashSet::new(), "заголовков", 150).is_none());
    }

    /// Листы находятся в тексте по порядку (с перекрытием и повторённым заголовком блока);
    /// куски и промежутки — весь текст без пересечений; строки заголовков — к куску.
    #[test]
    fn locate_segments_gaps_partition() {
        let text = "# A\n\nпервый абзац. второй.\n\n## B\n\nтретий абзац.\n\n## C\n\nчетвёртый.\n";
        let l = vec![
            leaf(0, 0, "первый абзац. второй."),
            leaf(1, 0, "второй."),
            leaf(2, 0, "```rust\nтретий абзац."),
            leaf(3, 0, "четвёртый."),
        ];
        let r = locate(text, &l).unwrap();
        assert_eq!(&text[r[0].0..r[0].1], "первый абзац. второй.");
        assert!(r[1].0 > r[0].0 && r[1].1 == r[0].1);
        assert_eq!(&text[r[2].0..r[2].1], "третий абзац.");
        let segs = segments(text, &r, &[2]);
        // кусок — со строки «## B»
        assert!(text[segs[0].0..].starts_with("## B"), "{segs:?}");
        let gs = gaps(text.len(), &segs);
        let mut all: Vec<(usize, usize)> = segs.iter().chain(&gs).copied().collect();
        all.sort_unstable();
        assert_eq!(all.first().unwrap().0, 0);
        assert_eq!(all.last().unwrap().1, text.len());
        assert!(all.windows(2).all(|w| w[0].1 == w[1].0), "{all:?}");
        let secs = sections(text);
        assert_eq!(
            crossed(text, &secs, gs[0].0, gs[0].1),
            ["A"],
            "{:?}",
            &text[gs[0].0..gs[0].1]
        );
        assert_eq!(crossed(text, &secs, gs[1].0, gs[1].1), ["A > C"]);
        assert!(locate("другое", &l).is_none());
    }
}
