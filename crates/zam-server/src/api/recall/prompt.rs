//! Подсказки к запросу пользователя — событие `prompt` `POST /recall` (§10.3; M6.6 §3: вынесено
//! из `mod.rs`, правила тишины и подсказка-задача). Разделы ответа ([`prompt_sections`]) — по
//! порядку: строки словаря ([`super::term_cands`]), подсказки поиска ([`prompt_hints`]),
//! подсказка-задача ([`task_prompt_cands`]) и подсказки-идеи ([`super::ideas::prompt_ideas`]);
//! у каждого раздела свой предел ([`super::section_limit`]). Правила выдачи, учёт показа и
//! сроки — в документации модуля [`super`].
//!
//! - **Пересланное сообщение** ([`zam_core::text::is_relayed_message`], M6.6 §3.3: отчёт
//!   субагента, сообщение агента, уведомление фоновой задачи) — не слова человека: тишина, без
//!   строк словаря (сервер — и для клиентов 0.0.12 и старше, которые такие сообщения шлют).
//! - **Нет предметных слов** ([`subject_terms`] пуст, M6.6 §3.1: «согласен», «продолжай», «ok»,
//!   только служебные слова) — только строки словаря: имя термина в запросе — уже предметное
//!   слово. Поиска подсказок, задач и идей нет.
//! - **Короткий запрос** (предметных слов не больше [`PROMPT_SHORT_WORDS`], M6.6 §3.2) —
//!   подсказка поиска проходит, только если запись нашёл лексический канал ([`lexical`]): балл
//!   гибрида нормирован по непустым каналам, и у слова без FTS-совпадения ближайшая по вектору
//!   запись получала бы 1,0 при пороге 0,6. Поиск без модели (`degraded`) правило не меняет: там
//!   все карточки и так из FTS.
//! - **Подсказка-задача** (M6.6 §3.4): отдельный проход FTS без эмбеддинга по задачам `open |
//!   in_progress | blocked` решения и Knowledge — строгий запрос (все слова) проходит, ослабленный
//!   (любое слово) — если промпт покрывает слова заголовка ([`title_covered`]); строка
//!   `ZeroAIMemory: задача «<заголовок>» [<статус>] k:<id>`, `reason: task`, не больше
//!   [`TASK_HINTS_MAX`] на ответ, раз за сессию (`session_seen`, как у остальных подсказок).
use std::collections::HashSet;

use axum::http::HeaderMap;
use serde_json::{Value, json};
use zam_core::TargetKind;
use zam_core::text::is_relayed_message;
use zam_search::recall::{StemWord, error_terms, hint_line, one_line, stem_words, subject_terms};
use zam_search::truncate_to_tokens;
use zam_store::{
    EntryKind, EntryStatus, FtsHit, FtsQuery, TargetFilter, count_tokens, search_fts_channels,
};

use super::ideas::prompt_ideas;
use super::{
    Cand, Deadline, REASON_TASK, RecallReq, age_of, entry_spaces, inner_search, item_age,
    prompt_min_score, term_cands,
};
use crate::api::search::Health;
use crate::api::sessions::RequestSession;
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предметных слов запроса ([`subject_terms`]), при которых он «короткий» (M6.6 §3.2,
/// **[решение координатора]** — константа, не ключ конфигурации): подсказка поиска проходит,
/// только если запись нашёл лексический канал (`fts`, `alias` или `trigram`, проверка `lexical`).
pub const PROMPT_SHORT_WORDS: usize = 2;
/// Подсказок-задач в ответе `prompt` (M6.6 §3.4).
pub const TASK_HINTS_MAX: usize = 1;
/// Кандидатов FTS задач с базы цели (M6.6 §3.4).
const TASK_PROMPT_TOP_K: usize = 10;
/// Начало строки подсказки-задачи после `ZeroAIMemory: `.
const TASK_LEAD: &str = "задача ";

/// Разделы ответа `prompt` (см. модуль): строки словаря, подсказки поиска, подсказка-задача,
/// подсказки-идеи (`ideas` — `Some(review)`, если идеи включены, [`super::ideas_enabled`]).
/// Проход задач — до идей: у идей свой срок, и шаг после их просрочки ставил бы
/// `recall timeout`, которого у ответа без идей нет.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prompt_sections(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    session: Option<&RequestSession>,
    ideas: Option<bool>,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<(Option<&'static str>, Vec<Cand>)>, ApiError> {
    let prompt = req.prompt.as_deref().unwrap_or_default();
    if is_relayed_message(prompt) {
        return Ok(Vec::new());
    }
    // строки словаря — перед подсказками insight; у каждого свой предел
    let chars = crate::glossary::HINT_TEXT_CHARS;
    let terms = term_cands(st, prompt, chars, health, deadline).await;
    let query = query_head(prompt);
    let subject = subject_terms(&query);
    if subject.is_empty() {
        return Ok(vec![(None, terms)]);
    }
    let short = subject.len() <= PROMPT_SHORT_WORDS;
    let hints = prompt_hints(st, p, headers, req, &query, short, health, deadline).await?;
    let tasks = task_prompt_cands(&query, &subject, health, deadline).await?;
    // идеи — отдельным поиском после основного, своей строкой и своим пределом
    let ideas = match ideas {
        Some(review) => {
            prompt_ideas(st, p, headers, req, session, health, deadline, review).await?
        }
        None => Vec::new(),
    };
    Ok(vec![
        (None, terms),
        (None, hints),
        (None, tasks),
        (None, ideas),
    ])
}

/// Первые [`zam_store::search::QUERY_MAX_CHARS`] символов промпта — запрос поиска подсказок.
fn query_head(prompt: &str) -> String {
    prompt
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect()
}

/// Карточку внутреннего поиска нашёл лексический канал — `fts`, `alias` или `trigram` в её
/// `sources` (M6.6 §3.2; общая проверка с подтверждением подсказок-идей
/// [`super::ideas`]): балл гибрида нормирован по непустым каналам, и карточка одного векторного
/// канала получает 1,0 на любой запрос.
pub(super) fn lexical(item: &Value) -> bool {
    item["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| matches!(s.as_str(), Some("fts" | "alias" | "trigram")))
}

/// Внутренний `POST /search` (см. модуль [`super`]) по запросу `query` и отбор insight и
/// заметок Rules; `short` — короткий запрос: только карточки с лексическим совпадением
/// ([`lexical`]).
#[allow(clippy::too_many_arguments)]
async fn prompt_hints(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    query: &str,
    short: bool,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": true, "include_rules": true},
        "filters": {"kinds": ["insight", "note"]},
        "mode": "brief",
        "top_k": super::PROMPT_SEARCH_TOP_K,
        "budget_tokens": super::PROMPT_SEARCH_BUDGET,
        // отсечка карточек поиска (M6 §4.2) — не для подсказок: у них свой порог балла
        "brief_min_rel": 0.0,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let area = req.area.as_deref();
    let Some(v) = inner_search(st, p, headers, area, body, None, health, deadline).await? else {
        return Ok(Vec::new());
    };
    let min = prompt_min_score(st, &v);
    let targets = health.targets();
    let mut out: Vec<Cand> = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        if it["score"].as_f64().unwrap_or(0.0) < min || (short && !lexical(it)) {
            continue;
        }
        let space_kind = it["space"]["kind"].as_str().unwrap_or_default();
        let kind = it["kind"].as_str().unwrap_or_default();
        let (tref, raw_id) = match (kind, it["entry_id"].as_str(), it["node_id"].as_str()) {
            ("insight", Some(id), _) => (TargetKind::Entry, id),
            ("note", _, Some(id)) if space_kind == "rules" => (TargetKind::Node, id),
            _ => continue,
        };
        let Ok(id) = raw_id.parse::<zam_core::Id>() else {
            continue;
        };
        let Some(space) = it["space"]["id"]
            .as_str()
            .and_then(|s| s.parse::<zam_core::Id>().ok())
        else {
            continue;
        };
        let Some(target) = targets
            .iter()
            .position(|t| t.spaces.iter().any(|s| s.id == space))
        else {
            continue;
        };
        let certainty = it["certainty"].as_str().map(str::to_string);
        let snippet = one_line(
            &it["snippet"]
                .as_str()
                .unwrap_or_default()
                .replace([zam_store::MARK_START, zam_store::MARK_END], ""),
        );
        let snippet =
            truncate_to_tokens(&snippet, st.cfg.recall.snippet_tokens, &count_tokens).to_string();
        let (reference, title, lead, tags, out_kind, reason) = match tref {
            TargetKind::Entry => (
                format!("i:{id}"),
                it["title"].as_str().unwrap_or_default().to_string(),
                "",
                it["insight_kind"].as_str().unwrap_or_default().to_string(),
                "insight",
                "search",
            ),
            TargetKind::Node => {
                let path = it["path"].as_str().unwrap_or_default();
                let hp = it["heading_path"].as_str().unwrap_or_default();
                let title = if hp.is_empty() {
                    path.to_string()
                } else {
                    format!("{path} › {hp}")
                };
                (
                    format!("n:{id}"),
                    title,
                    "правило ",
                    String::new(),
                    "note",
                    "rules",
                )
            }
            // вложения в подсказки не входят: виды внутреннего поиска заданы явно (`note`, `insight`)
            TargetKind::Attachment => continue,
        };
        let mut block = hint_line(
            lead,
            &title,
            &[tags.as_str(), certainty.as_deref().unwrap_or_default()],
            &reference,
        );
        if !snippet.is_empty() {
            block.push_str("\n  ");
            block.push_str(&snippet);
        }
        out.push(Cand {
            target,
            tref: zam_core::TargetRef { kind: tref, id },
            rev: it["rev"].as_i64().unwrap_or(0),
            reference,
            kind: out_kind,
            title,
            certainty,
            reason,
            block,
            age_days: item_age(it),
        });
    }
    Ok(out)
}

/// Подсказка-задача (см. модуль): FTS по задачам `open | in_progress | blocked` пространств
/// решения и Knowledge каждой живой цели (до [`TASK_PROMPT_TOP_K`] находок с базы), слияние
/// баз — по сырому баллу bm25, как у `post_tool`; находка ослабленного запроса — только с
/// покрытым заголовком ([`title_covered`] по предметным словам `subject`). Предел в ответе —
/// [`TASK_HINTS_MAX`] (раздел упаковки, [`super::section_limit`]): кандидатов больше — на
/// случай уже показанных сессии.
pub(super) async fn task_prompt_cands(
    query: &str,
    subject: &[String],
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
    let words: Vec<StemWord> = subject.iter().cloned().map(StemWord::new).collect();
    let statuses = [
        EntryStatus::Open,
        EntryStatus::InProgress,
        EntryStatus::Blocked,
    ];
    let targets = health.targets();
    // (сырой балл bm25, цель, находка, ослабленный запрос)
    let mut hits: Vec<(f64, usize, FtsHit, bool)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let spaces = entry_spaces(t);
        if spaces.is_empty() || !health.alive[i] {
            continue;
        }
        let q = FtsQuery {
            alias_groups: Vec::new(),
            text: query.to_string(),
            space_ids: spaces,
            path_prefix: None,
            top_k: TASK_PROMPT_TOP_K,
            targets: TargetFilter {
                kinds: vec![EntryKind::Task.as_str().into()],
                statuses: statuses.iter().map(|s| s.as_str().to_string()).collect(),
                area: None,
                certainty: Vec::new(),
                decided_by: None,
                file: None,
                insight_kinds: Vec::new(),
                entry_ids: Vec::new(),
            },
        };
        let Some(r) = deadline.step(search_fts_channels(&t.db, q)).await else {
            break;
        };
        if let Some(ch) = health.soft(i, r)? {
            let relaxed = ch.relaxed;
            hits.extend(
                ch.bm25
                    .into_iter()
                    .take(TASK_PROMPT_TOP_K)
                    .map(|h| (h.raw_score, i, h, relaxed)),
            );
        }
    }
    // bm25: меньше — лучше
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut taken: HashSet<zam_core::TargetRef> = HashSet::new();
    let mut out: Vec<Cand> = Vec::new();
    for (_, i, h, relaxed) in hits {
        let Some(e) = &h.entry else {
            continue;
        };
        // у задачи может быть несколько чанков: заголовок один — и решение одно
        if !taken.insert(h.target) || (relaxed && !title_covered(&e.title, &words)) {
            continue;
        }
        let reference = format!("{}:{}", EntryKind::Task.ref_letter(), h.target.id);
        let status = e
            .status
            .as_deref()
            .unwrap_or(EntryStatus::Open.as_str())
            .to_string();
        let block = hint_line(
            TASK_LEAD,
            &format!("«{}»", one_line(&e.title)),
            &[status.as_str()],
            &reference,
        );
        out.push(Cand {
            target: i,
            tref: h.target,
            rev: h.rev,
            reference,
            kind: EntryKind::Task.as_str(),
            title: e.title.clone(),
            certainty: e.certainty.clone(),
            reason: REASON_TASK,
            block,
            age_days: age_of(h.updated_at, None),
        });
    }
    Ok(out)
}

/// Промпт покрывает слова заголовка задачи (M6.6 §3.4, «почти дословный заголовок»): из `k`
/// значимых слов заголовка (`stem_words(error_terms(title))` — от 3 символов, без служебных) с
/// предметными словами промпта `prompt` совпадают по основам ([`StemWord::matches`]) не меньше
/// `max(min(k, 2), ⌈2k/3⌉)`; заголовок без значимых слов — нет.
fn title_covered(title: &str, prompt: &[StemWord]) -> bool {
    let words = stem_words(&error_terms(title));
    let k = words.len();
    if k == 0 {
        return false;
    }
    let covered = words
        .iter()
        .filter(|w| prompt.iter().any(|p| w.matches(p)))
        .count();
    covered >= k.min(2).max((2 * k).div_ceil(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(prompt: &str) -> Vec<StemWord> {
        subject_terms(prompt)
            .into_iter()
            .map(StemWord::new)
            .collect()
    }

    /// Порог покрытия заголовка: `max(min(k, 2), ⌈2k/3⌉)` слов из `k`, по основам.
    #[test]
    fn task_title_coverage() {
        let title = "Перенос памяти ZeroAgent";
        assert!(title_covered(
            title,
            &words("давай займёмся переносом памяти ZeroAgent")
        ));
        // k = 3 — нужно 2
        assert!(title_covered(title, &words("перенос памяти")));
        assert!(!title_covered(
            title,
            &words("перенос логов на другой диск")
        ));
        // k = 1 — одно слово; k = 2 — оба
        assert!(title_covered("Индексатор", &words("почини индексатор")));
        assert!(!title_covered(
            "Индексатор очереди",
            &words("почини индексатор")
        ));
        assert!(title_covered(
            "Индексатор очереди",
            &words("индексатор и очередь")
        ));
        // k = 6 — нужно 4
        let six = "Миграция схемы базы на новый формат индекса";
        assert!(!title_covered(six, &words("миграция схемы базы")));
        assert!(title_covered(six, &words("миграция схемы базы и индекса")));
        // заголовок из служебных слов — не задача промпта
        assert!(!title_covered("что это", &words("что это такое")));
        assert!(!title_covered(title, &[]));
    }

    #[test]
    fn lexical_sources() {
        assert!(lexical(&json!({"sources": ["vector", "fts"]})));
        assert!(lexical(&json!({"sources": ["alias"]})));
        assert!(lexical(&json!({"sources": ["trigram"]})));
        assert!(!lexical(&json!({"sources": ["vector"]})));
        assert!(!lexical(&json!({"sources": ["vector", "folder"]})));
        assert!(!lexical(&json!({})));
    }
}
