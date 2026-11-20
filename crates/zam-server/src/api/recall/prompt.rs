//! Подсказки к запросу пользователя — событие `prompt` `POST /recall` (§10.3; M6.6 §3: вынесено
//! из `mod.rs`, правила тишины и подсказка-задача). Разделы ответа ([`prompt_sections`]) — по
//! порядку: строки словаря ([`super::term_cands`]), подсказки поиска ([`prompt_hints`]),
//! подсказка-задача ([`task_prompt_cands`]), подсказки-идеи ([`super::ideas::prompt_ideas`]) и
//! строка очереди разбора ([`super::ideas::review_queue_head`], [`super::ideas::QueueHead::line`]);
//! у каждого раздела свой предел ([`super::section_limit`]). Правила выдачи, учёт показа и
//! сроки — в документации модуля [`super`]. Правило тишины — общее с моделью калибровки
//! `xtask eval --hints`: [`zam_search::recall::prompt_gate`] (ревью задачи 2, I1).
//!
//! - **Пересланное сообщение** ([`zam_core::text::is_relayed_message`], M6.6 §3.3: отчёт
//!   субагента, сообщение агента, уведомление фоновой задачи) — не слова человека: тишина, без
//!   строк словаря (сервер — и для клиентов 0.0.12 и раньше, которые такие сообщения шлют).
//! - **Нет предметных слов** ([`zam_search::recall::subject_terms`] пуст, M6.6 §3.1:
//!   «согласен», «продолжай», «ok», только служебные слова) — только строки словаря (имя термина
//!   в запросе — уже предметное слово) и строка очереди разбора (§3.5, ниже). Поиска подсказок,
//!   задач и идей нет.
//! - **Короткий запрос** (предметных слов не больше [`PROMPT_SHORT_WORDS`], M6.6 §3.2) —
//!   подсказка поиска проходит, только если запись нашёл лексический канал
//!   ([`PromptGate::admits_card`]): балл гибрида нормирован по непустым каналам, и у слова без
//!   FTS-совпадения ближайшая по вектору запись получала бы 1,0 при пороге 0,6. Поиск без модели
//!   (`degraded`) правило не меняет: там все карточки и так из FTS.
//! - **Подсказка-задача** (M6.6 §3.4): отдельный проход FTS без эмбеддинга по задачам `open |
//!   in_progress | blocked` решения и Knowledge — строгий запрос (все слова) проходит, ослабленный
//!   (любое слово) — если промпт покрывает слова заголовка ([`title_covered`]); у короткого
//!   запроса заголовок нужен и строгому (**[решение координатора]**, ревью задачи 2, M1: одно
//!   слово из тела задачи подсказки не даёт); строка
//!   `ZeroAIMemory: задача «<заголовок>» [<статус>] k:<id>`, `reason: task`, не больше
//!   [`TASK_HINTS_MAX`] на ответ, раз за сессию (`session_seen`, как у остальных подсказок).
//! - **Очередь разбора** (M6.6 §3.5, замечание 36): режим `review`, владелец базы решения, идеи
//!   включены — строка `ZeroAIMemory: новая идея на разбор «<заголовок>» w:<id>` (при
//!   нескольких непоказанных — ` (ещё N)`), `reason: idea_review`, одна на ответ, раз на идею за
//!   эпоху сессии в её текущей ревизии (показ дайджестом той же эпохи считается; после сжатия
//!   контекста — снова; идея, вернувшаяся в `new` новой ревизией, — снова). От близости к
//!   запросу не зависит и идёт мимо правила «нет предметных слов» («продолжай»), но не мимо
//!   пересылки; голова очереди читается до поиска; срок — общий срок `prompt`, не уложился —
//!   тишина без предупреждений. Режим `propose`, участник общего решения, выключенные идеи — без
//!   строки; снятая пределом вывода канала — строка «не показано — идей на разбор: 1
//!   (memory_recall event=idea_review)».
//!
//! [`PROMPT_SHORT_WORDS`]: zam_search::recall::PROMPT_SHORT_WORDS
use std::collections::HashSet;

use axum::http::HeaderMap;
use serde_json::json;
use zam_core::TargetKind;
use zam_search::recall::{
    PromptGate, StemWord, error_terms, hint_line, one_line, prompt_gate, stem_words,
};
use zam_search::truncate_to_tokens;
use zam_store::{
    EntryKind, EntryStatus, FtsHit, FtsQuery, TargetFilter, count_tokens, search_fts_channels,
};

use super::ideas::{prompt_ideas, review_queue_head};
use super::{
    Cand, Deadline, REASON_TASK, RecallReq, age_of, entry_spaces, inner_search, item_age,
    prompt_min_score, term_cands,
};
use crate::api::search::Health;
use crate::api::sessions::RequestSession;
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Подсказок-задач в ответе `prompt` (M6.6 §3.4).
pub const TASK_HINTS_MAX: usize = 1;
/// Кандидатов FTS задач с базы цели (M6.6 §3.4).
const TASK_PROMPT_TOP_K: usize = 10;
/// Начало строки подсказки-задачи после `ZeroAIMemory: `.
const TASK_LEAD: &str = "задача ";

/// Разделы ответа `prompt` (см. модуль): строки словаря, подсказки поиска, подсказка-задача,
/// подсказки-идеи (`ideas_on` — идеи включены, [`super::ideas_enabled`]; `review` — режим
/// `ideas.mode = review`) — и отдельно строка очереди разбора (M6.6 §3.5): у неё свой отбор
/// показанного (эпоха и ревизия, [`super::ideas::review_queue_head`]), общий фильтр показанного
/// `recall` её не трогает. Проход задач — до идей: у идей свой срок, и шаг после их просрочки
/// ставил бы `recall timeout`, которого у ответа без идей нет.
///
/// Голова очереди читается после правила пересылки (§3.3) и до правила «нет предметных слов»
/// (§3.1) — до поиска, чтобы срок поиска её не вытеснял (ревью задачи 14, M1); строка выбирается
/// после подсказок-идей и идёт в ответе последней: от запроса она не зависит и не повторяет
/// идею, которую ответ уже несёт подсказкой-идеей.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prompt_sections(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    session: Option<&RequestSession>,
    ideas_on: bool,
    review: bool,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<(Vec<(Option<&'static str>, Vec<Cand>)>, Vec<Cand>), ApiError> {
    let prompt = req.prompt.as_deref().unwrap_or_default();
    // признак пересылки — по всему промпту, как у хука; предметные слова — по запросу поиска
    let gate = prompt_gate(prompt, zam_store::search::QUERY_MAX_CHARS);
    if gate == PromptGate::Relayed {
        return Ok((Vec::new(), Vec::new()));
    }
    // строки словаря — перед подсказками insight; у каждого свой предел
    let chars = crate::glossary::HINT_TEXT_CHARS;
    let terms = term_cands(st, prompt, chars, health, deadline).await;
    // очередь разбора (M6.6 §3.5) — и к реплике без предметных слов; пустой запрос — не запрос
    // человека (как у подсказок-идей)
    let queue = if ideas_on && review && !prompt.trim().is_empty() {
        review_queue_head(st, p, session, health, deadline).await
    } else {
        None
    };
    let mut out = vec![(None, terms)];
    let mut ideas = Vec::new();
    if let PromptGate::Search { subject, short } = &gate {
        let query = query_head(prompt);
        let hints = prompt_hints(st, p, headers, req, &query, &gate, health, deadline).await?;
        let tasks = task_prompt_cands(&query, subject, *short, health, deadline).await?;
        // идеи — отдельным поиском после основного, своей строкой и своим пределом
        if ideas_on {
            ideas = prompt_ideas(st, p, headers, req, session, health, deadline, review).await?;
        }
        out.extend([(None, hints), (None, tasks)]);
    }
    let line = queue.map(|q| q.line(&ideas)).unwrap_or_default();
    out.push((None, ideas));
    Ok((out, line))
}

/// Первые [`zam_store::search::QUERY_MAX_CHARS`] символов промпта — запрос поиска подсказок.
fn query_head(prompt: &str) -> String {
    prompt
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect()
}

/// Внутренний `POST /search` (см. модуль [`super`]) по запросу `query` и отбор insight и заметок
/// Rules: карточка — с баллом не ниже порога и принятая правилом тишины `gate`
/// ([`PromptGate::admits_card`]: у короткого запроса — только лексические).
#[allow(clippy::too_many_arguments)]
async fn prompt_hints(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    query: &str,
    gate: &PromptGate,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
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
        if it["score"].as_f64().unwrap_or(0.0) < min || !gate.admits_card(it) {
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
/// баз — по сырому баллу bm25, как у `post_tool`; находка ослабленного запроса, а у короткого
/// запроса (`short`) и строгого, — только с покрытым заголовком ([`title_covered`] по
/// предметным словам `subject`; **[решение координатора]**, ревью задачи 2, M1). Предел в
/// ответе — [`TASK_HINTS_MAX`] (раздел упаковки, [`super::section_limit`]): кандидатов больше —
/// на случай уже показанных сессии.
async fn task_prompt_cands(
    query: &str,
    subject: &[String],
    short: bool,
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
        // у задачи может быть несколько чанков: заголовок один — и решение одно; строгое
        // совпадение одного-двух слов (они могут быть только в теле) заголовок не заменяет
        if !taken.insert(h.target) || ((relaxed || short) && !title_covered(&e.title, &words)) {
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
        zam_search::recall::subject_terms(prompt)
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
}
