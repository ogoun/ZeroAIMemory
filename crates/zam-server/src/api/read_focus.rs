//! Явное фокусное чтение `POST /read {items: [{id, focus}]}` (M6 §6, решение владельца 6).
//!
//! Фокус — только по явному `focus` агента; без него `/read` прежний (L2 целиком). Выдача цели:
//! - **строка оглавления** `Оглавление: A · *B · … · …ещё N заголовков` — заголовки H1–H3 заметки
//!   (у документа — части по `locator_text`), выбранные листы отмечают свой заголовок `*`; не
//!   больше [`TOC_MAX_ENTRIES`] записей и [`TOC_MAX_TOKENS`] токенов строки;
//! - **листы** с лучшими баллами по `focus` (не больше `read.focus_leaves`) в порядке документа,
//!   перед листом — `[<путь заголовков>]` (у документа — `[<подпись части>]`) при смене;
//! - **пропуски** `[пропущено: <путь> · ~N t]` на месте невыданных листов — по секции H1–H3
//!   (путь годен для `section` дочитывания, §3.4), у документа — подряд идущие части одной
//!   пометкой `<первая> — <последняя>` (подписи годны для `locator`); `N` — токены листов
//!   (счётчик чанкера).
//!
//! **Балл листа** — RRF (k = 60) двух порядков: FTS-лексика (число слов `focus`, найденных в
//! листе, [`zam_store::focus_leaves`]) и dense-косинус листа (эмбеддинг запроса и векторы листов
//! из кэша `embeddings`), без реранкера. Лексика без единого совпадения — первые листы. Лист без
//! вектора в кэше (не проиндексирован) получает только лексический ранг.
//!
//! **Срок** `read.focus_timeout_ms` — на эмбеддинг запроса и чтение векторов вместе (I10). Вышел,
//! модели нет или векторный канал недоступен (`degraded`, [`IndexRuntime::vector_available`]) —
//! только лексика и `focus lexical` в `warnings`. Запрос одного текста `focus` эмбеддится один раз
//! на вызов `/read` ([`FocusCtx`]); не уложившийся текст у следующих целей вызова — сразу лексика.
//!
//! [`IndexRuntime::vector_available`]: crate::index_state::IndexRuntime::vector_available
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use zam_core::{Id, TargetKind, TargetRef};
use zam_docs::Locator;
use zam_markdown::parse;
use zam_store::{Db, FocusLeaf, StoreError, count_tokens, focus_leaves, get_embeddings};

use super::doc_locator::Sheets;
use super::search::md_inline;
use crate::app::AppState;
use crate::config::ReadSection;

/// Предупреждение: цель короче `read.focus_min_tokens`, запись журнала или фрагмент `a:` —
/// выдана целиком, `focus` проигнорирован.
pub const FOCUS_SKIPPED: &str = "focus skipped: short";
/// Предупреждение: фокус только по FTS-лексике листов (срок, нет модели, поиск `degraded`).
pub const FOCUS_LEXICAL: &str = "focus lexical";
/// Предел записей строки оглавления.
pub const TOC_MAX_ENTRIES: usize = 40;
/// Предел токенов строки оглавления.
pub const TOC_MAX_TOKENS: usize = 150;
/// Предел символов заголовка в оглавлении.
const TOC_TITLE_CHARS: usize = 60;
/// Константа RRF слияния лексического и векторного порядков.
const RRF_K: f64 = 60.0;
/// Подпись преамбулы заметки (листы до первого заголовка) в пометке пропуска.
const PREAMBLE: &str = "начало";

/// Цель фокуса.
pub enum FocusTarget<'a> {
    /// Узел: полный текст ревизии `rev` (заголовки оглавления и секции пометок — из него).
    Note {
        db: &'a Db,
        node: Id,
        text: &'a str,
        rev: i64,
    },
    /// Документ вложения `att` (листы — его чанки, подписи — через `sheets`).
    Doc {
        db: &'a Db,
        att: Id,
        sheets: &'a Sheets,
    },
}

/// Итог фокуса цели.
#[derive(Debug)]
pub struct FocusOut {
    /// Оглавление, листы и пропуски.
    pub text: String,
    /// `chunk_id` выданных листов (отметка `session_seen`, `part = chunk_id`).
    pub leaves: Vec<String>,
    /// `focus lexical` или пусто.
    pub warnings: Vec<String>,
}

/// Состояние фокуса одного вызова `/read`: настройки, доступность векторов и эмбеддинги
/// запросов по тексту `focus` (`None` — не уложился в срок или не удался).
pub struct FocusCtx<'a> {
    st: &'a AppState,
    cfg: &'a ReadSection,
    dense: bool,
    queries: HashMap<String, Option<Vec<f32>>>,
}

impl<'a> FocusCtx<'a> {
    pub fn new(st: &'a AppState) -> FocusCtx<'a> {
        FocusCtx {
            st,
            cfg: &st.cfg.read,
            dense: st.index.vector_available().is_ok(),
            queries: HashMap::new(),
        }
    }

    /// Порог «короткой» цели, токенов полного текста (`read.focus_min_tokens`).
    pub fn min_tokens(&self) -> usize {
        self.cfg.focus_min_tokens
    }
}

/// Лист и его подпись для пометок и оглавления.
struct Leaf {
    l: FocusLeaf,
    tokens: usize,
    /// Секция (путь H1–H3, у преамбулы — [`PREAMBLE`]) или подпись части документа.
    label: String,
    /// Запись оглавления, к которой лист относится (индекс в `toc`).
    entry: Option<usize>,
}

/// Фокус цели по тексту `focus`. Листов нет (или нарезка не той ревизии) — `None`: цель выдаётся
/// целиком, как короткая.
pub async fn focus_read(
    fx: &mut FocusCtx<'_>,
    target: &FocusTarget<'_>,
    focus: &str,
) -> Result<Option<FocusOut>, StoreError> {
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
    let raw = focus_leaves(db, tref, zam_store::query_needles(focus)).await?;
    if raw.is_empty() {
        return Ok(None);
    }
    if let FocusTarget::Note { rev, .. } = target
        && raw.iter().any(|l| l.rev != *rev)
    {
        return Ok(None);
    }
    let (toc, leaves) = match target {
        FocusTarget::Note { text, .. } => note_layout(text, raw),
        FocusTarget::Doc { sheets, .. } => doc_layout(sheets, raw),
    };
    let mut warnings = Vec::new();
    let cos = dense_scores(fx, db, focus, &leaves).await;
    if cos.is_none() {
        warnings.push(FOCUS_LEXICAL.to_string());
    }
    let chosen = choose(&leaves, cos.as_ref(), fx.cfg.focus_leaves);
    let doc = matches!(target, FocusTarget::Doc { .. });
    Ok(Some(render(&toc, &leaves, &chosen, doc, warnings)))
}

/// Косинусы листов к запросу (`None` — лексика: нет модели или векторов, срок вышел).
async fn dense_scores(
    fx: &mut FocusCtx<'_>,
    db: &Db,
    focus: &str,
    leaves: &[Leaf],
) -> Option<HashMap<usize, f32>> {
    if !fx.dense {
        return None;
    }
    let st = fx.st;
    let emb = st.index.embedder.clone()?;
    let model = st.index.model_id()?.to_string();
    if let Some(None) = fx.queries.get(focus) {
        return None;
    }
    let cached = fx.queries.get(focus).cloned().flatten();
    let mut keys: Vec<String> = leaves
        .iter()
        .map(|l| l.l.embed_key.clone())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort();
    keys.dedup();
    let text = focus.to_string();
    let work = async {
        #[cfg(feature = "test-hooks")]
        {
            let d = st.focus_delay_ms.load(std::sync::atomic::Ordering::SeqCst);
            if d > 0 {
                tokio::time::sleep(Duration::from_millis(d)).await;
            }
        }
        let q = match cached {
            Some(q) => q,
            None => {
                let e = tokio::task::spawn_blocking(move || emb.embed_query(&text)).await;
                match e {
                    Ok(Ok(e)) => e.dense,
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "фокус: эмбеддинг запроса не удался — лексика");
                        return None;
                    }
                    Err(j) => {
                        tracing::error!(
                            panic = j.is_panic(),
                            "фокус: эмбеддинг аварийно — лексика"
                        );
                        return None;
                    }
                }
            }
        };
        match get_embeddings(db, &model, &keys).await {
            Ok(m) => Some((q, m)),
            Err(e) => {
                tracing::warn!(error = %e, "фокус: векторы листов не прочитаны — лексика");
                None
            }
        }
    };
    let limit = Duration::from_millis(fx.cfg.focus_timeout_ms);
    let got = tokio::time::timeout(limit, work).await.ok().flatten();
    let Some((q, embs)) = got else {
        fx.queries.insert(focus.to_string(), None);
        return None;
    };
    fx.queries.insert(focus.to_string(), Some(q.clone()));
    let cos: HashMap<usize, f32> = leaves
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let e = embs.get(&l.l.embed_key)?;
            Some((i, q.iter().zip(&e.dense).map(|(a, b)| a * b).sum::<f32>()))
        })
        .collect();
    // ни одного вектора в кэше (цель не проиндексирована) — лексика
    (!cos.is_empty()).then_some(cos)
}

/// Выбранные листы (индексы по порядку документа): RRF лексики и косинуса; лексика без
/// совпадений и без векторов — первые `n` листов.
fn choose(leaves: &[Leaf], cos: Option<&HashMap<usize, f32>>, n: usize) -> Vec<usize> {
    let mut score: HashMap<usize, f64> = HashMap::new();
    let mut lex: Vec<usize> = (0..leaves.len())
        .filter(|&i| leaves[i].l.hits > 0)
        .collect();
    lex.sort_by(|&a, &b| leaves[b].l.hits.cmp(&leaves[a].l.hits).then(a.cmp(&b)));
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
    let mut picked: Vec<usize> = if score.is_empty() {
        (0..leaves.len().min(n)).collect()
    } else {
        let mut v: Vec<(usize, f64)> = score.into_iter().collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().take(n).map(|(i, _)| i).collect()
    };
    picked.sort_unstable();
    picked
}

/// Запись оглавления: подпись и номер секции (у документа — части).
struct TocEntry {
    title: String,
}

/// Оглавление заметки (секции H1–H3) и листы с секциями: секция листа — самый длинный префикс
/// его пути заголовков, совпадающий с путём секции (лист H4 — в своей H3; путь склейки коротких
/// секций — путь первой).
fn note_layout(text: &str, raw: Vec<FocusLeaf>) -> (Vec<TocEntry>, Vec<Leaf>) {
    let doc = parse(text);
    let secs: Vec<(String, String)> = doc
        .sections
        .iter()
        .filter(|s| (1..=3).contains(&s.level))
        .map(|s| (s.path.join(" > "), s.title.clone()))
        .collect();
    let index: HashMap<&str, usize> = secs
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.as_str(), i))
        .collect();
    let leaves = raw
        .into_iter()
        .map(|l| {
            let parts: Vec<&str> = if l.heading_path.is_empty() {
                Vec::new()
            } else {
                l.heading_path.split(" > ").collect()
            };
            let entry = (1..=parts.len())
                .rev()
                .find_map(|k| index.get(parts[..k].join(" > ").as_str()).copied());
            let label = entry.map_or_else(|| PREAMBLE.to_string(), |i| secs[i].0.clone());
            Leaf {
                tokens: l.token_count,
                l,
                label,
                entry,
            }
        })
        .collect();
    let toc = secs
        .into_iter()
        .map(|(_, title)| TocEntry { title })
        .collect();
    (toc, leaves)
}

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

/// Оглавление документа (части по порядку) и чанки с подписями (`locator_text` чанка).
fn doc_layout(sheets: &Sheets, raw: Vec<FocusLeaf>) -> (Vec<TocEntry>, Vec<Leaf>) {
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut by_part: HashMap<String, usize> = HashMap::new();
    let leaves = raw
        .into_iter()
        .map(|l| {
            let loc = l.locator.as_deref().and_then(|r| Locator::parse(r).ok());
            let (label, entry) = match &loc {
                Some(loc) => {
                    let part = part_label(sheets, loc);
                    let i = *by_part.entry(part.clone()).or_insert_with(|| {
                        toc.push(TocEntry { title: part });
                        toc.len() - 1
                    });
                    (sheets.shown(loc).1, Some(i))
                }
                None => (PREAMBLE.to_string(), None),
            };
            Leaf {
                tokens: l.token_count,
                l,
                label,
                entry,
            }
        })
        .collect();
    (toc, leaves)
}

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

/// Строка оглавления: записи по порядку, выбранные — `*`; не больше [`TOC_MAX_ENTRIES`] записей
/// и [`TOC_MAX_TOKENS`] токенов, остаток — `…ещё N <noun>`. Записей нет — `None`.
fn toc_line(toc: &[TocEntry], marked: &HashSet<usize>, noun: &str) -> Option<String> {
    if toc.is_empty() {
        return None;
    }
    let items: Vec<String> = toc
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let t = toc_title(&e.title);
            if marked.contains(&i) {
                format!("*{t}")
            } else {
                t
            }
        })
        .collect();
    let line = |k: usize| {
        let mut parts: Vec<String> = items[..k].to_vec();
        if k < items.len() {
            parts.push(format!("…ещё {} {noun}", items.len() - k));
        }
        format!("Оглавление: {}", parts.join(" · "))
    };
    // оценка суммой токенов записей (каждая считается один раз), затем точная проверка строки
    let cost: Vec<usize> = items
        .iter()
        .map(|t| count_tokens(&format!(" · {t}")))
        .collect();
    let head =
        count_tokens("Оглавление: ") + count_tokens(&format!(" · …ещё {} {noun}", items.len()));
    let mut k = 0;
    let mut sum = head;
    while k < items.len().min(TOC_MAX_ENTRIES) && sum + cost[k] <= TOC_MAX_TOKENS {
        sum += cost[k];
        k += 1;
    }
    while k > 0 && count_tokens(&line(k)) > TOC_MAX_TOKENS {
        k -= 1;
    }
    Some(line(k))
}

fn skip_line(label: &str, tokens: usize) -> String {
    format!("[пропущено: {label} · ~{tokens} t]")
}

/// Текст фокуса: оглавление, выбранные листы с подписями, пропуски.
fn render(
    toc: &[TocEntry],
    leaves: &[Leaf],
    chosen: &[usize],
    doc: bool,
    warnings: Vec<String>,
) -> FocusOut {
    let set: HashSet<usize> = chosen.iter().copied().collect();
    let marked: HashSet<usize> = chosen.iter().filter_map(|&i| leaves[i].entry).collect();
    let noun = if doc {
        "частей"
    } else {
        "заголовков"
    };
    let mut out = String::new();
    if let Some(line) = toc_line(toc, &marked, noun) {
        out.push_str(&line);
        out.push('\n');
    }
    // пропуск: (первая подпись, последняя подпись, токены)
    let mut gap: Option<(String, String, usize)> = None;
    let flush = |gap: &mut Option<(String, String, usize)>, out: &mut String| {
        if let Some((first, last, t)) = gap.take() {
            let label = if first == last {
                first
            } else {
                format!("{first} — {last}")
            };
            out.push_str(&skip_line(&label, t));
            out.push('\n');
        }
    };
    let mut last_head: Option<&str> = None;
    for (i, l) in leaves.iter().enumerate() {
        if !set.contains(&i) {
            match &mut gap {
                // заметка — пометка на секцию; документ — подряд идущие части одной пометкой
                Some((_, last, t)) if doc || *last == l.label => {
                    last.clone_from(&l.label);
                    *t += l.tokens;
                }
                _ => {
                    flush(&mut gap, &mut out);
                    gap = Some((l.label.clone(), l.label.clone(), l.tokens));
                }
            }
            last_head = None;
            continue;
        }
        flush(&mut gap, &mut out);
        let head = if doc {
            l.label.as_str()
        } else {
            l.l.heading_path.as_str()
        };
        if last_head != Some(head) && !head.is_empty() {
            out.push_str(&format!("[{head}]\n"));
        }
        last_head = Some(head);
        out.push_str(l.l.text.trim());
        out.push('\n');
    }
    flush(&mut gap, &mut out);
    FocusOut {
        text: out,
        leaves: chosen
            .iter()
            .map(|&i| leaves[i].l.chunk_id.clone())
            .collect(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(i: i64, path: &str, hits: usize, text: &str) -> Leaf {
        Leaf {
            l: FocusLeaf {
                chunk_id: format!("c{i}"),
                heading_path: path.into(),
                ordinal: i,
                locator: None,
                text: text.into(),
                token_count: 10,
                rev: 1,
                embed_key: String::new(),
                hits,
            },
            tokens: 10,
            label: if path.is_empty() {
                PREAMBLE.into()
            } else {
                path.into()
            },
            entry: None,
        }
    }

    /// Лексика: по числу совпадений, при равенстве — раньше в документе; выдача — в порядке
    /// документа; без совпадений — первые листы.
    #[test]
    fn choose_lexical_and_first() {
        let l = vec![
            leaf(0, "A", 0, "a"),
            leaf(1, "A > B", 1, "b"),
            leaf(2, "C", 3, "c"),
            leaf(3, "D", 1, "d"),
        ];
        assert_eq!(choose(&l, None, 2), [1, 2]);
        let none: Vec<Leaf> = (0..4).map(|i| leaf(i, "A", 0, "x")).collect();
        assert_eq!(choose(&none, None, 3), [0, 1, 2]);
        // RRF: лексика и косинус вместе (лист 1 — второй в обоих порядках — обходит лист 0,
        // первый только по косинусу)
        let cos: HashMap<usize, f32> = [(0, 0.9), (1, 0.1), (2, 0.2), (3, 0.0)].into();
        assert_eq!(choose(&l, Some(&cos), 1), [2]);
        assert_eq!(choose(&l, Some(&cos), 2), [1, 2]);
        // без совпадений слов — по косинусу
        let cos: HashMap<usize, f32> = [(0, 0.1), (1, 0.2), (2, 0.3), (3, 0.9)].into();
        assert_eq!(choose(&none, Some(&cos), 1), [3]);
    }

    /// Оглавление — не больше 40 записей и 150 токенов, отмеченные — `*`.
    #[test]
    fn toc_limits() {
        let toc: Vec<TocEntry> = (0..100)
            .map(|i| TocEntry {
                title: format!("Заголовок {i}"),
            })
            .collect();
        let line = toc_line(&toc, &[1].into(), "заголовков").unwrap();
        assert!(count_tokens(&line) <= TOC_MAX_TOKENS, "{line}");
        assert!(line.contains("*Заголовок 1 ·"), "{line}");
        let shown = line.split(" · ").count() - 1;
        assert!(shown <= TOC_MAX_ENTRIES, "{line}");
        assert!(
            line.ends_with(&format!("…ещё {} заголовков", 100 - shown)),
            "{line}"
        );
        let short = toc_line(&toc[..2], &HashSet::new(), "заголовков").unwrap();
        assert_eq!(short, "Оглавление: Заголовок 0 · Заголовок 1");
        assert!(toc_line(&[], &HashSet::new(), "заголовков").is_none());
    }

    /// Пропуски: заметка — по секции, документ — подряд одной пометкой; подпись листа — при
    /// смене пути.
    #[test]
    fn render_gaps() {
        let l = vec![
            leaf(0, "", 0, "пре"),
            leaf(1, "A", 0, "a1"),
            leaf(2, "A", 0, "a2"),
            leaf(3, "B", 1, "b"),
            leaf(4, "B", 1, "b2"),
            leaf(5, "C", 0, "c"),
        ];
        let out = render(&[], &l, &[3, 4], false, Vec::new());
        assert_eq!(
            out.text,
            "[пропущено: начало · ~10 t]\n[пропущено: A · ~20 t]\n[B]\nb\nb2\n\
             [пропущено: C · ~10 t]\n"
        );
        assert_eq!(out.leaves, ["c3", "c4"]);
        let out = render(&[], &l, &[3], true, Vec::new());
        assert_eq!(
            out.text,
            "[пропущено: начало — A · ~30 t]\n[B]\nb\n[пропущено: B — C · ~20 t]\n"
        );
    }
}
