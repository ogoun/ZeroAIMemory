//! Идеи в `POST /recall` (M5.7 §6, M6.5 §7; вынесено из `recall.rs` — M6.6 §11.1): раздел
//! дайджеста [`SECTION_IDEAS`] или очередь разбора [`SECTION_IDEAS_REVIEW`] ([`digest_ideas`]),
//! подсказки-идеи к `prompt` ([`prompt_ideas`]), строка плана из идей ([`plan_line`]) и пакет
//! разбора `idea_review` ([`review_pack`]). Правила выдачи, учёт показа и сроки — в документации
//! модуля [`super`]; общие помощники (`Cand`, `Deadline`, `inner_search`, `shown`, …) — там же.
use std::collections::HashSet;
#[cfg(feature = "test-hooks")]
use std::time::Duration;

use axum::http::HeaderMap;
use serde_json::{Value, json};
#[cfg(feature = "test-hooks")]
use tokio::time::Instant;
use zam_core::{Id, TargetKind, TargetRef};
use zam_search::recall::{
    CoverageRule, HINT_PREFIX, StemWord, WordMatch, error_terms, prepared_terms, stem_words,
};
use zam_store::{
    Entry, EntryFilter, EntryKind, EntryStatus, FtsQuery, GraphStore, RECALL_PART, SpaceKind,
    StoreError, TargetFilter, list_entries, search_fts_channels,
};

use super::{
    Cand, Deadline, Lines, OMIT_IDEAS_REVIEW, PROMPT_SEARCH_BUDGET, RecallReq, clip, entry_cand,
    entry_filter, entry_ref, entry_spaces, inner_search, personal_space, prompt_min_score,
    seen_key, shown, solution_space,
};
use crate::api::search::targets::SearchTarget;
use crate::api::search::{Health, LightSearch};
use crate::api::sessions::RequestSession;
use crate::api::user_settings::{UserFlags, ideas_mode};
use crate::app::AppState;
use crate::attrib::SESSION_HEADER;
use crate::auth::Principal;
use crate::config::IdeasMode;
use crate::error::ApiError;

/// Заголовок раздела идей дайджеста (M5.7 §6.1): сразу после строки открытых вопросов.
const SECTION_IDEAS: &str = "### Идеи";
/// Строка-указание под заголовком [`SECTION_IDEAS`]: правило «без ответа не начинай» приходит
/// ровно тогда, когда идея предложена (блок инструкции v6 тратит на него одну строку).
const IDEAS_HINT: &str = "Предложи человеку одну: memory_update_entry action=propose — вопрос \
    «начать / отложить / отказаться»; без ответа «начать» не начинай.";
/// Добавка к [`IDEAS_HINT`], если в разделе есть идея со снятой задачей (ревью задачи 7, M3).
const IDEAS_HINT_REOPEN: &str = " Со снятой задачей — предложи вернуть: propose с text, почему \
    пора; решает человек.";
/// Хвост пометки строки идеи со снятой задачей (§16, п. 1).
const REOPEN_MARK_TAIL: &str = "можно предложить вернуть";
/// `reason` подсказки-идеи (дайджест и `prompt`).
pub(super) const REASON_IDEA: &str = "idea";
/// Предел заголовка идеи в строке дайджеста и подсказки, символов.
const IDEA_TITLE_CHARS: usize = 120;
/// Заголовок раздела очереди разбора (M6.5 §7, режим `ideas.mode = review`): в дайджесте — на
/// месте [`SECTION_IDEAS`], в пакете `idea_review` — его раздел.
pub const SECTION_IDEAS_REVIEW: &str = "### Идеи на разбор";
/// Строка-указание под [`SECTION_IDEAS_REVIEW`] дайджеста (`{n}` — длина очереди,
/// [`ideas_review_hint`]): агент сам выбирает момент — «когда закончишь текущий шаг».
pub const IDEAS_REVIEW_HINT: &str = "Непросмотренных идей: {n}. Разбери, когда закончишь текущий \
    шаг: memory_recall event=idea_review → memory_update_entry action=review.";
/// Строка плана дайджеста (M6.5 §7, [`plan_line`]): пункты плана из идей и следующий из них.
pub const PLAN_LINE: &str = "План из идей: {n} открытых, следующая — k:{id} «{title}» [{priority}]";
/// Строка плана без открытого пункта (все пункты в работе или заблокированы): «следующей» нет.
pub const PLAN_LINE_NO_NEXT: &str = "План из идей: {n} открытых";
/// `reason` идеи очереди разбора (`new` в режиме `review`): строки дайджеста и подсказки к
/// `prompt`, идеи пакета `idea_review`.
pub(super) const REASON_IDEA_REVIEW: &str = "idea_review";
/// Пометка строки-хвоста раздела [`SECTION_IDEAS_REVIEW`] у отложенной идеи с наступившим
/// `horizon` ([`idea_tail_line`]).
const DEFERRED_DUE_MARK: &str = "срок отсрочки вышел";
/// Указание строки-хвоста раздела [`SECTION_IDEAS_REVIEW`]: строка-указание раздела — о разборе,
/// а такую идею по M5.7 предлагают человеку вопросом ([`IDEAS_HINT`]).
const TAIL_PROPOSE: &str = "(предложи человеку: action=propose — вопрос «начать / отложить / \
    отказаться»; без ответа «начать» не начинай)";
/// Указание строки-хвоста у идеи со снятой задачей ([`IDEAS_HINT_REOPEN`]).
const TAIL_REOPEN: &str = "(предложи вернуть: action=propose с text — почему пора; решает человек)";
/// Идей в пакете `idea_review` по умолчанию (M6.5 §7).
pub const IDEA_REVIEW_LIMIT: usize = 5;
/// Наибольший `limit` пакета `idea_review`.
pub const IDEA_REVIEW_LIMIT_MAX: u32 = 20;
/// Бюджет пакета `idea_review` и карточек его поиска похожих по умолчанию, токенов.
pub const IDEA_REVIEW_BUDGET: usize = 3000;
/// Похожих записей у идеи пакета (M6.5 §7).
pub const IDEA_REVIEW_SIMILAR_MAX: usize = 5;
/// `top_k` поиска похожих: [`IDEA_REVIEW_SIMILAR_MAX`] и сама идея.
const IDEA_REVIEW_TOP_K: usize = IDEA_REVIEW_SIMILAR_MAX + 1;
/// Предел тела идеи в пакете, символов: длиннее — с «…» и указанием `memory_read`.
const IDEA_REVIEW_BODY_CHARS: usize = 1500;
/// Предел строки истории разбора и строки похожей записи, символов.
const IDEA_REVIEW_LINE_CHARS: usize = 200;
/// Предел довода человека в истории идеи пакета, символов.
const IDEA_REVIEW_REASON_CHARS: usize = 500;
/// Виды похожих записей пакета: идеи, задачи (план и сделанное), changelog, решения, заметки.
const IDEA_REVIEW_KINDS: [&str; 5] = ["idea", "task", "changelog", "insight", "note"];
/// Статусы поиска похожих: все статусы идей и задач и действующие insight — без `active` и
/// `revisit` общий фильтр статусов отсёк бы решения (`decision`); у changelog и заметок статуса
/// нет — они проходят всегда.
const IDEA_REVIEW_STATUSES: [&str; 13] = [
    "new",
    "proposed",
    "accepted",
    "deferred",
    "declined",
    "returned",
    "done",
    "open",
    "in_progress",
    "blocked",
    "dropped",
    "active",
    "revisit",
];
/// Строка шагов разбора под заголовком раздела пакета `idea_review` (M6.5 §8).
pub const IDEA_REVIEW_STEPS: &str = "Шаги разбора: сверь идею с планом (открытые задачи), ТЗ и \
    сделанным (changelog, задачи done, код) и с похожими; повтор отклонённой без новых доводов — \
    decline со ссылкой в similar; итог — memory_update_entry id=w:<id> action=review \
    outcome=plan|return|decline|done summary=… (return — missing, plan — task.body с планом \
    шагов).";
/// Пакет `idea_review`: очередь пуста.
pub const IDEA_REVIEW_EMPTY: &str = "Идей на разбор нет.";
/// Пакет `idea_review` без решения в области запроса.
pub const IDEA_REVIEW_NO_SOLUTION: &str =
    "Идей на разбор нет: решение не задано (solution или X-Zam-Solution).";
/// Пакет `idea_review` при режиме `propose` владельца базы решения: разбора нет — у агента не
/// должно быть двух противоречивых указаний (M6.5 §7).
pub const IDEA_REVIEW_OFF: &str = "Разбор идей выключен: ideas.mode = propose — предлагай идею \
    человеку (memory_update_entry action=propose).";
/// Пакет `idea_review` без права записи в решение (токен `ro`, участник с `ro`): разбор дал бы 403.
pub const IDEA_REVIEW_READ_ONLY: &str = "Разбор идей недоступен: нет права записи в решение.";
/// Адрес идей пакета, не вошедших в бюджет, когда часть показана ([`review_shown_line`]):
/// следующий пакет после разбора показанных (разобранные из очереди уходят).
const REVIEW_OMIT_NEXT: &str = "memory_recall event=idea_review после разбора показанных";
/// Адрес, когда в бюджет не вошла ни одна идея ([`review_shown_line`]): разбор показанных
/// очередь не продвинет — нужен бюджет больше или идей меньше.
const REVIEW_OMIT_BUDGET: &str = "подними budget_tokens или уменьши limit";
/// Тестовая метка заголовка идеи: отдельный поиск идей к `prompt` «не укладывается в срок»
/// (фича `test-hooks`, проверка тишины по идеям).
#[cfg(feature = "test-hooks")]
pub const TEST_SLOW_IDEA_MARK: &str = "zam-test-slow-idea";
/// Запросы, по которым шёл второй проход подсказки-идей ([`released_prompt_ideas`]) — только с
/// фичей `test-hooks` (тест: второй проход не нужен, когда первый уже дал предел).
#[cfg(feature = "test-hooks")]
pub static TEST_SECOND_PASS_QUERIES: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// Длительности отдельного поиска идей к `prompt` (внутренний поиск [`search_prompt_ideas`]) и
/// поле `timings` его ответа (этапы поиска, мс; `null` — поиск не ответил) — только с фичей
/// `test-hooks`: микрозамер `ideas_recall::inner_search_ideas_median` (M6 §16).
#[cfg(feature = "test-hooks")]
pub static TEST_IDEA_SEARCH_TIMES: std::sync::Mutex<Vec<(Duration, Value)>> =
    std::sync::Mutex::new(Vec::new());

/// Выключатель лёгкого поиска идей (фича `test-hooks`): микрозамер
/// `ideas_recall::inner_search_ideas_median` чередует лёгкий и обычный поиск в одном процессе —
/// сравнение под одним фоном нагрузки. Вне замера — `false`.
#[cfg(feature = "test-hooks")]
pub static TEST_IDEA_LIGHT_OFF: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Заголовок идеи в строке: до [`IDEA_TITLE_CHARS`] символов, секреты маскируются.
fn idea_title(e: &Entry) -> String {
    zam_markdown::mask_text(&clip(&e.title, IDEA_TITLE_CHARS))
}

/// Пометка строки идеи `accepted` со снятой или удалённой задачей (§16, п. 1): `task` —
/// состояние её задачи ([`zam_store::idea_task_state`]); у прочих статусов — `None`.
fn released_mark(e: &Entry, task: Option<zam_store::TaskState>) -> Option<&'static str> {
    use zam_store::TaskState;
    if e.status != Some(EntryStatus::Accepted) {
        return None;
    }
    match task {
        Some(TaskState::Dropped) => Some("задача снята"),
        Some(TaskState::Deleted) | None => Some("задача удалена"),
        Some(TaskState::Live | TaskState::Done) => None,
    }
}

/// Строка идеи дайджеста: `ZeroAIMemory: идея «<заголовок до 120>» [<priority>] — w:<id>`;
/// у идеи со снятой задачей перед ссылкой — `<пометка>, можно предложить вернуть` (§16).
fn idea_line(e: &Entry, mark: Option<&str>) -> String {
    let priority = zam_store::IdeaData::from_stored(&e.data).priority;
    let mark = mark
        .map(|m| format!(" — {m}, {REOPEN_MARK_TAIL}"))
        .unwrap_or_default();
    format!(
        "ZeroAIMemory: идея «{}» [{}]{mark} — {}",
        idea_title(e),
        priority.as_str(),
        entry_ref(e)
    )
}

/// Строка подсказки идеи к `prompt` (§6.2); у идеи со снятой задачей — пометка и указание
/// «предложи вернуть» (§16).
fn idea_prompt_line(e: &Entry, mark: Option<&str>) -> String {
    match mark {
        None => format!(
            "ZeroAIMemory: к задаче подходит идея «{}» — {} (предложи: action=propose; без \
             ответа не начинай)",
            idea_title(e),
            entry_ref(e)
        ),
        Some(m) => format!(
            "ZeroAIMemory: к задаче подходит идея «{}» — {m}, можно предложить вернуть — {} \
             (предложи: action=propose с text — почему пора; решает человек)",
            idea_title(e),
            entry_ref(e)
        ),
    }
}

/// Подсказка-идея цели `target` со строкой `block` (`kind: idea`): `reason` — [`REASON_IDEA`]
/// (предложение по M5.7) или [`REASON_IDEA_REVIEW`] (очередь разбора, M6.5 §7).
fn idea_cand(target: usize, e: &Entry, reason: &'static str, block: String) -> Cand {
    let mut c = entry_cand(target, e, reason, "");
    c.block = block;
    c
}

/// Строка-указание [`IDEAS_REVIEW_HINT`] с длиной очереди `n`.
fn ideas_review_hint(n: usize) -> String {
    IDEAS_REVIEW_HINT.replace("{n}", &n.to_string())
}

/// Заголовок раздела идей дайджеста со строкой-указанием (раскладка [`super::arrange`]) и
/// признак очереди разбора. Есть идея очереди (M6.5 §7) — [`SECTION_IDEAS_REVIEW`] и
/// [`IDEAS_REVIEW_HINT`] с длиной очереди `queue`: хвост кандидатов предложения несёт указание
/// `propose` в строке ([`idea_tail_line`]). Иначе — [`SECTION_IDEAS`] и [`IDEAS_HINT`], с
/// добавкой [`IDEAS_HINT_REOPEN`], если в разделе есть идея со снятой задачей.
pub(super) fn ideas_section_head(ideas: &[&Cand], queue: usize) -> (String, bool) {
    let review = ideas.iter().any(|c| c.reason == REASON_IDEA_REVIEW);
    let head = if review {
        format!("{SECTION_IDEAS_REVIEW}\n{}", ideas_review_hint(queue))
    } else {
        format!(
            "{SECTION_IDEAS}\n{IDEAS_HINT}{}",
            if ideas.iter().any(|c| c.block.contains(REOPEN_MARK_TAIL)) {
                IDEAS_HINT_REOPEN
            } else {
                ""
            }
        )
    };
    (head, review)
}

/// Строка-хвост раздела [`SECTION_IDEAS_REVIEW`] (M6.5 §7, «прочие случаи M5.7»): идея, которую
/// агент предлагает человеку вопросом, — `deferred` с наступившим `horizon` или `accepted` со
/// снятой задачей (`mark`, [`released_mark`]). Указание `action=propose` — в самой строке:
/// строка-указание раздела говорит о разборе.
fn idea_tail_line(e: &Entry, mark: Option<&str>) -> String {
    match mark {
        None => format!(
            "ZeroAIMemory: идея «{}» [{}] — {DEFERRED_DUE_MARK} — {} {TAIL_PROPOSE}",
            idea_title(e),
            zam_store::IdeaData::from_stored(&e.data).priority.as_str(),
            entry_ref(e)
        ),
        Some(_) => format!("{} {TAIL_REOPEN}", idea_line(e, mark)),
    }
}

/// Строка подсказки к `prompt` у идеи очереди разбора (M6.5 §7): «к задаче подходит идея на
/// разбор w:<id>» — без `propose`: идею `new` в режиме `review` разбирает агент.
fn idea_review_prompt_line(e: &Entry) -> String {
    format!(
        "ZeroAIMemory: к задаче подходит идея на разбор {} «{}» — разбери, когда закончишь \
         текущий шаг (memory_recall event=idea_review)",
        entry_ref(e),
        idea_title(e)
    )
}

/// Строка плана [`PLAN_LINE`]: `n` пунктов плана из идей, следующий — `next`, открытый
/// ([`zam_store::planned_from_ideas`]; нет `data.priority` — `normal`). Открытых нет (все в
/// работе или заблокированы) — [`PLAN_LINE_NO_NEXT`].
fn plan_text(n: usize, next: Option<&Entry>) -> String {
    let Some(next) = next else {
        return PLAN_LINE_NO_NEXT.replace("{n}", &n.to_string());
    };
    let priority = zam_store::TaskData::from_stored(&next.data)
        .priority
        .unwrap_or(zam_store::IdeaPriority::Normal);
    // заголовок — последним: подстановки в тексте человека не раскрываются
    PLAN_LINE
        .replace("{n}", &n.to_string())
        .replace("{id}", &next.id.to_string())
        .replace("{priority}", priority.as_str())
        .replace("{title}", &idea_title(next))
}

/// Проактивность идей (§8): `[ideas] proactive` сервера и `ideas.proactive` пользователя
/// (`server && user`; настройки пользователя — одним чтением с `hooks.<тип>`, сбой чтения —
/// всё включено).
pub(super) fn ideas_enabled(st: &AppState, flags: &UserFlags) -> bool {
    st.cfg.ideas.proactive && flags.ideas_proactive
}

/// Показывалась ли сессии хоть одна идея базы цели `t` (§6.1, «первый `session_start`»):
/// среди записей, выданных сессии (`session_seen`, `RECALL_PART`, любая ревизия, без эпох),
/// есть вид `idea` любого статуса — в том числе отложенная, предложенная или удалённая после
/// показа. Без сессии — нет (учёта показа нет); сбой чтения — WARN и «показывалась»: раздел
/// лучше пропустить, чем повторить.
async fn idea_shown_in_session(s: Option<&RequestSession>, t: &SearchTarget) -> bool {
    let Some(s) = s else {
        return false;
    };
    let ids = match zam_store::seen_entry_ids(&s.db, &s.touch.id, t.owner, RECALL_PART).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "recall: показанное сессии не прочитано, без раздела идей");
            return true;
        }
    };
    match zam_store::any_idea(&t.db, &ids).await {
        Ok(shown) => shown,
        Err(e) => {
            tracing::warn!(error = %e, "recall: виды показанных записей не прочитаны, без раздела идей");
            true
        }
    }
}

/// Идеи раздела [`SECTION_IDEAS`] (§6.1): только пространство решения запроса (личного или
/// общего — база владельца; Knowledge — нет), до `ideas.digest_max` в порядке
/// [`zam_store::proactive_ideas`], и только на первом `session_start` сессии — пока ей не
/// показана ни одна идея этой базы ([`idea_shown_in_session`]). Иначе каждый `/compact`
/// приносил бы следующие три. Сбой чтения идей — WARN и раздела нет: ни ответ, ни остальной
/// дайджест цели (`health.alive`) он не трогает.
///
/// Режим `review` (M6.5 §7, `review` — режим владельца базы решения): сначала очередь разбора
/// [`zam_store::review_queue`] (`reason: idea_review`, раздел [`SECTION_IDEAS_REVIEW`]), за ней —
/// кандидаты предложения по M5.7 [`zam_store::proposal_ideas`] (`deferred` с наступившим
/// `horizon`, `accepted` со снятой задачей; строка — [`idea_tail_line`]), всего не больше
/// `ideas.digest_max`. Очереди нет — раздел M5.7 из одних кандидатов предложения. Итог — идеи и
/// длина очереди (строка «Непросмотренных идей: N»).
pub(super) async fn digest_ideas(
    st: &AppState,
    p: &Principal,
    session: Option<&RequestSession>,
    health: &Health<'_>,
    deadline: &mut Deadline,
    review: bool,
) -> (Vec<Cand>, usize) {
    let max = st.cfg.ideas.digest_max as usize;
    let Some((i, space)) = solution_space(health.targets()) else {
        return (Vec::new(), 0);
    };
    if max == 0 || !health.alive[i] || !ideas_writable(p, &health.targets()[i]) {
        return (Vec::new(), 0);
    }
    let t = &health.targets()[i];
    match deadline.step(idea_shown_in_session(session, t)).await {
        Some(false) => {}
        Some(true) | None => return (Vec::new(), 0),
    }
    let today = zam_store::time::utc_today();
    let db = &t.db;
    let read = async {
        if !review {
            let c = zam_store::proactive_ideas(db, &[space], today, &HashSet::new(), max).await?;
            return Ok::<_, StoreError>((0, Vec::new(), c));
        }
        let (n, queue) = zam_store::review_queue(db, space, max).await?;
        let tail = zam_store::proposal_ideas(db, &[space], today, max - queue.len()).await?;
        Ok((n, queue, tail))
    };
    let Some(r) = deadline.step(read).await else {
        return (Vec::new(), 0);
    };
    let (n, queue, cands) = match r {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "recall: идеи дайджеста не прочитаны");
            return (Vec::new(), 0);
        }
    };
    let mut out: Vec<Cand> = queue
        .iter()
        .map(|e| idea_cand(i, e, REASON_IDEA_REVIEW, idea_line(e, None)))
        .collect();
    for e in &cands {
        // пометка «задача снята» — по состоянию задачи (только у `accepted`, §16)
        let task = if e.status == Some(EntryStatus::Accepted) {
            match deadline.step(zam_store::idea_task_state(&t.db, e)).await {
                Some(Ok(s)) => s,
                Some(Err(err)) => {
                    tracing::warn!(error = %err, "recall: задача идеи дайджеста не прочитана");
                    continue;
                }
                None => break,
            }
        } else {
            None
        };
        let mark = released_mark(e, task);
        // за очередью разбора — строка с указанием `propose`; без очереди — раздел M5.7
        let block = if queue.is_empty() {
            idea_line(e, mark)
        } else {
            idea_tail_line(e, mark)
        };
        out.push(idea_cand(i, e, REASON_IDEA, block));
    }
    (out, if queue.is_empty() { 0 } else { n })
}

/// Режим идей решения запроса (M6.5 §7): `ideas.mode` владельца базы решения — в общем решении
/// действует режим владельца, а не участника; своё решение — режим пользователя запроса
/// (`flags`, уже прочитан). Решения в области нет — режим пользователя. Сбой чтения настроек
/// владельца — WARN и `[ideas] default_mode` сервера.
pub(super) async fn solution_ideas_mode(
    st: &AppState,
    targets: &[SearchTarget],
    user: Id,
    flags: &UserFlags,
) -> IdeasMode {
    match solution_space(targets) {
        Some((i, _)) if targets[i].owner != user => match ideas_mode(st, targets[i].owner).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "recall: режим идей владельца решения не прочитан");
                st.cfg.ideas.default_mode
            }
        },
        _ => flags.ideas_mode,
    }
}

/// Пакет разбора идей `event: idea_review` (M6.5 §7): идеи очереди [`zam_store::review_queue`]
/// пространства решения запроса (до `limit`, по умолчанию [`IDEA_REVIEW_LIMIT`]) — по блоку
/// [`review_block`] на идею (`reason: idea_review`): строка с приоритетом, тело, история
/// ([`review_history`]: прежние разборы и доводы человека) и до [`IDEA_REVIEW_SIMILAR_MAX`]
/// похожих записей решения ([`review_similar`]). Вместо идей — одна строка-уведомление: решения
/// нет ([`IDEA_REVIEW_NO_SOLUTION`]), режим `propose` владельца базы ([`IDEA_REVIEW_OFF`]), нет
/// права записи ([`IDEA_REVIEW_READ_ONLY`]), очередь пуста ([`IDEA_REVIEW_EMPTY`]). Срок сбора
/// вышел — собранное (у идей без поиска — строка «не собраны») и `recall timeout`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn review_pack(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    mode: IdeasMode,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<(Lines, Vec<Cand>), ApiError> {
    let notice = |s: &str| Lines {
        notices: vec![s.to_string()],
        ..Lines::default()
    };
    let targets = health.targets();
    let Some((i, space)) = solution_space(targets) else {
        return Ok((notice(IDEA_REVIEW_NO_SOLUTION), Vec::new()));
    };
    if mode == IdeasMode::Propose {
        return Ok((notice(IDEA_REVIEW_OFF), Vec::new()));
    }
    if !ideas_writable(p, &targets[i]) {
        return Ok((notice(IDEA_REVIEW_READ_ONLY), Vec::new()));
    }
    // решение исключено (`solution unavailable` уже в предупреждениях)
    if !health.alive[i] {
        return Ok((Lines::default(), Vec::new()));
    }
    let db = targets[i].db.clone();
    let limit = req.limit.map_or(IDEA_REVIEW_LIMIT, |l| l as usize);
    let Some(r) = deadline
        .step(zam_store::review_queue(&db, space, limit))
        .await
    else {
        return Ok((Lines::default(), Vec::new()));
    };
    let Some((n, queue)) = health.soft(i, r)? else {
        return Ok((Lines::default(), Vec::new()));
    };
    if queue.is_empty() {
        return Ok((notice(IDEA_REVIEW_EMPTY), Vec::new()));
    }
    let mut out = Vec::with_capacity(queue.len());
    for e in &queue {
        let history = review_history(&db, e, deadline).await;
        let similar = review_similar(st, p, headers, req, e, health, deadline).await?;
        let block = review_block(e, &history, similar.as_deref());
        out.push(idea_cand(i, e, REASON_IDEA_REVIEW, block));
    }
    // длина очереди — для строки «показано k из N» ([`review_shown_line`])
    let lines = Lines {
        ideas_queue: n,
        ..Lines::default()
    };
    Ok((lines, out))
}

/// Строки после пакета `idea_review` (ревью задачи 4, Minor 3; раунд 2): «Показано идей на
/// разбор: k из N.» — `shown` вошедших в текст из `n` в очереди; не вошедшие в бюджет (`picked`
/// идей собрано больше, чем показано) — строка «не показано — идей на разбор: M (…)» в формате
/// строк о снятом ([`Section::omit`]): часть показана — следующий пакет после разбора показанных
/// ([`REVIEW_OMIT_NEXT`]), не вошла ни одна — бюджет мал для одной идеи
/// ([`REVIEW_OMIT_BUDGET`]). Бюджет снимает блоки молча (строки о снятом — только у предела
/// вывода канала, [`pack_limited`]), поэтому строка — здесь; под пределом вывода (`limited`) её
/// даёт сам предел (`omit` раздела, [`omit_of`]). `after` — текст перед строками не пуст: они
/// отделяются от блока последней идеи пустой строкой.
///
/// [`Section::omit`]: zam_search::recall::Section::omit
/// [`pack_limited`]: super::pack_limited
/// [`omit_of`]: super::omit_of
pub(super) fn review_shown_line(
    shown: usize,
    picked: usize,
    n: usize,
    limited: bool,
    after: bool,
) -> String {
    let mut s = String::new();
    if after {
        s.push('\n');
    }
    s.push_str(&format!("Показано идей на разбор: {shown} из {n}.\n"));
    if shown < picked && !limited {
        let whole = if shown == 0 {
            REVIEW_OMIT_BUDGET
        } else {
            REVIEW_OMIT_NEXT
        };
        s.push_str(&format!(
            "{HINT_PREFIX} не показано — {}: {} ({whole})\n",
            OMIT_IDEAS_REVIEW.0,
            picked - shown
        ));
    }
    s
}

/// Блок идеи пакета `idea_review`: строка идеи (как в дайджесте), тело (до
/// [`IDEA_REVIEW_BODY_CHARS`], длиннее — с указанием `memory_read`), «История:» (если есть) и
/// «Похожие:» — строки с отступом; `similar: None` — поиск не состоялся (срок, сбой). Секреты
/// маскируются.
fn review_block(e: &Entry, history: &[String], similar: Option<&[String]>) -> String {
    let mut b = idea_line(e, None);
    let body = zam_markdown::mask_text(e.body.trim());
    let cut = body.chars().count() > IDEA_REVIEW_BODY_CHARS;
    let shown: String = body.chars().take(IDEA_REVIEW_BODY_CHARS).collect();
    for l in shown
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
    {
        b.push_str("\n  ");
        b.push_str(l);
    }
    if cut {
        b.push_str(&format!("…\n  (полностью — memory_read {})", entry_ref(e)));
    }
    if !history.is_empty() {
        b.push_str("\n  История:");
        for h in history {
            b.push_str("\n  - ");
            b.push_str(h);
        }
    }
    match similar {
        None => b.push_str(
            "\n  Похожие: не собраны — срок сбора вышел или поиск не удался (memory_search)",
        ),
        Some([]) => b.push_str("\n  Похожих нет."),
        Some(lines) => {
            b.push_str("\n  Похожие:");
            for l in lines {
                b.push_str("\n  - ");
                b.push_str(l);
            }
        }
    }
    b
}

/// Первая непустая строка текста.
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Строка текста человека или агента в пакете: первая строка, до `max` символов, секреты
/// маскируются.
fn pack_text(s: &str, max: usize) -> String {
    zam_markdown::mask_text(&clip(first_line(s), max))
}

/// Дата UTC времени `t` (`YYYY-MM-DD`).
fn utc_date(t: jiff::Timestamp) -> String {
    t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string()
}

/// История идеи пакета `idea_review` по времени: прежние разборы агента (`data.reviews`:
/// исход, первая строка разбора, у `return` — чего не хватало) и доводы человека — причины
/// ревизий с его решением (`data.decided.by: human`; «Вернуть на разбор» — `reopen`,
/// «Отказаться» с причиной — `decline`). Ревизии читаются, только если по идее было решение;
/// сбой чтения или срок — WARN, без доводов.
async fn review_history(db: &zam_store::Db, e: &Entry, deadline: &mut Deadline) -> Vec<String> {
    let d = zam_store::IdeaData::from_stored(&e.data);
    let mut out: Vec<(jiff::Timestamp, String)> = d
        .reviews
        .iter()
        .map(|r| {
            let mut s = format!(
                "{} разбор {}: {}",
                utc_date(r.at),
                r.outcome.as_str(),
                pack_text(&r.summary, IDEA_REVIEW_LINE_CHARS)
            );
            if let Some(m) = &r.missing {
                s.push_str(&format!(
                    "; не хватало: {}",
                    pack_text(m, IDEA_REVIEW_LINE_CHARS)
                ));
            }
            (r.at, s)
        })
        .collect();
    if d.decided.is_some() {
        match deadline.step(zam_store::entry_revisions(db, e.id)).await {
            Some(Ok(revs)) => {
                for rev in revs {
                    let Some(reason) = rev.reason.as_deref().filter(|r| !r.trim().is_empty())
                    else {
                        continue;
                    };
                    let decided = zam_store::IdeaData::from_stored(&rev.data).decided;
                    if let Some(dc) = decided.filter(|dc| dc.by == zam_store::AnswerBy::Human) {
                        out.push((
                            rev.ts,
                            format!(
                                "{} довод человека ({}): {}",
                                utc_date(rev.ts),
                                dc.decision.as_str(),
                                pack_text(reason, IDEA_REVIEW_REASON_CHARS)
                            ),
                        ));
                    }
                }
            }
            Some(Err(err)) => {
                tracing::warn!(error = %err, "recall: ревизии идеи пакета не прочитаны");
            }
            None => {}
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out.into_iter().map(|(_, s)| s).collect()
}

/// Похожие записи идеи `e` пакета `idea_review` (M6.5 §7): внутренний поиск в пространстве
/// решения запроса по заголовку и началу тела идеи (`mode=brief`, видов
/// [`IDEA_REVIEW_KINDS`], статусов [`IDEA_REVIEW_STATUSES`], решения — `insight_kinds
/// [decision]`, `top_k` [`IDEA_REVIEW_TOP_K`], `budget_tokens` запроса или
/// [`IDEA_REVIEW_BUDGET`], без кэша выжимок, без реранка, без учёта выданного). FTS — сразу
/// ослабленным запросом ([`LightSearch::any_terms`]): строгий по тексту идеи находил бы её одну.
/// Сама идея исключается, остальные — до [`IDEA_REVIEW_SIMILAR_MAX`] строк
/// `<ссылка> · <вид insight> · <статус> · «<заголовок>»` (у заметки — путь); у отклонённой идеи
/// — ` — разбор: <первая строка>` или ` — причина человека: …` ([`decline_reason`]). `None` —
/// срок вышел или поиск не удался.
#[allow(clippy::too_many_arguments)]
async fn review_similar(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    e: &Entry,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Option<Vec<String>>, ApiError> {
    let query: String = format!("{}\n{}", e.title, e.body)
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect();
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": false, "include_rules": false},
        "filters": {
            "kinds": IDEA_REVIEW_KINDS,
            "status": IDEA_REVIEW_STATUSES,
            "insight_kinds": ["decision"],
        },
        "mode": "brief",
        "top_k": IDEA_REVIEW_TOP_K,
        "budget_tokens": req.budget_tokens.unwrap_or(IDEA_REVIEW_BUDGET),
        // отсечка карточек поиска (M6 §4.2) — не для похожих: их немного, порядок — по баллу
        "brief_min_rel": 0.0,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let light = LightSearch {
        channel_depth: (4 * IDEA_REVIEW_TOP_K).max(IDEA_LIGHT_DEPTH_MIN),
        snippets: false,
        any_terms: true,
    };
    // тестовая задержка (частичный пакет по сроку): только с фичей test-hooks
    #[cfg(feature = "test-hooks")]
    if e.title.contains(TEST_SLOW_IDEA_MARK) {
        test_slow_idea(deadline).await;
        return Ok(None);
    }
    let area = req.area.as_deref();
    let Some(v) = inner_search(st, p, headers, area, body, Some(light), health, deadline).await?
    else {
        return Ok(None);
    };
    let targets = health.targets();
    let mut out: Vec<String> = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        if out.len() == IDEA_REVIEW_SIMILAR_MAX {
            break;
        }
        let kind = it["kind"].as_str().unwrap_or_default();
        let id = |k: &str| it[k].as_str().and_then(|s| s.parse::<Id>().ok());
        let (reference, entry) = match (id("entry_id"), id("node_id")) {
            (Some(eid), _) => {
                if eid == e.id {
                    continue;
                }
                let Some(k) = EntryKind::parse(kind) else {
                    continue;
                };
                (format!("{}:{eid}", k.ref_letter()), Some((k, eid)))
            }
            (None, Some(nid)) => (format!("n:{nid}"), None),
            (None, None) => continue,
        };
        let mut parts = vec![reference];
        for k in ["insight_kind", "status"] {
            if let Some(v) = it[k].as_str().filter(|v| !v.is_empty()) {
                parts.push(v.to_string());
            }
        }
        let line = match entry {
            Some(_) => format!(
                "«{}»",
                zam_markdown::mask_text(&clip(
                    it["title"].as_str().unwrap_or_default(),
                    IDEA_TITLE_CHARS
                ))
            ),
            None => zam_markdown::mask_text(&clip(
                it["path"].as_str().unwrap_or_default(),
                IDEA_TITLE_CHARS,
            )),
        };
        parts.push(line);
        let mut s = parts.join(" · ");
        // отклонённая идея — с причиной: повтор без новых доводов агент отклоняет (§8)
        if let Some((EntryKind::Idea, eid)) = entry
            && it["status"] == "declined"
        {
            let db = it["space"]["id"]
                .as_str()
                .and_then(|sp| sp.parse::<Id>().ok())
                .and_then(|sp| targets.iter().find(|t| t.spaces.iter().any(|x| x.id == sp)))
                .map(|t| t.db.clone());
            if let Some(db) = db
                && let Some(why) = decline_reason(&db, eid, deadline).await
            {
                s.push_str(&format!(" — {why}"));
            }
        }
        out.push(s);
    }
    Ok(Some(out))
}

/// Причина отклонения идеи `id` для строки похожей записи (ревью задачи 4, Important 1):
/// отклонил агент разбором (`data.decided` — `decline` канала `review`) — ` — разбор: <первая
/// строка разбора>`; отклонил человек — ` — причина человека: <причина>` только из ревизии, где
/// это решение появилось (первая ревизия с тем же `data.decided`, вместе с `at`): причину в
/// ревизию пишет `decide`, у «Отказаться» она необязательна — причина другой ревизии (довод
/// «вернуть» или «отложить») отказу не принадлежит, а поздние ревизии с тем же решением её не
/// несут. Нет такой причины, сбой чтения, срок — `None`.
async fn decline_reason(db: &zam_store::Db, id: Id, deadline: &mut Deadline) -> Option<String> {
    use zam_store::{AnswerBy, DecidedAs, DecidedVia, ReviewOutcome};
    let e = match deadline.step(zam_store::get_entry(db, id)).await? {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(error = %err, "recall: похожая идея пакета не прочитана");
            return None;
        }
    };
    let d = zam_store::IdeaData::from_stored(&e.data);
    let decided = d.decided.as_ref()?;
    if decided.decision != DecidedAs::Decline {
        return None;
    }
    if decided.channel == DecidedVia::Review {
        let r = d
            .review
            .as_ref()
            .filter(|r| r.outcome == ReviewOutcome::Decline)?;
        return Some(format!(
            "разбор: {}",
            pack_text(&r.summary, IDEA_REVIEW_LINE_CHARS)
        ));
    }
    if decided.by != AnswerBy::Human {
        return None;
    }
    let revs = match deadline.step(zam_store::entry_revisions(db, id)).await? {
        Ok(revs) => revs,
        Err(err) => {
            tracing::warn!(error = %err, "recall: ревизии похожей идеи не прочитаны");
            return None;
        }
    };
    // ревизия, где нынешнее решение появилось (то же `decided`, в том числе `at`, — первая):
    // поздние ревизии с тем же `decided` (правки, хуки) причины не несут (ревью задачи 4, раунд 2)
    let rev = revs
        .iter()
        .find(|r| zam_store::IdeaData::from_stored(&r.data).decided.as_ref() == Some(decided))?;
    let why = rev.reason.as_deref().filter(|s| !s.trim().is_empty())?;
    Some(format!(
        "причина человека: {}",
        pack_text(why, IDEA_REVIEW_LINE_CHARS)
    ))
}

/// Подсказки-идеи к `prompt` (§6.2): отдельный лёгкий поиск после основного (`mode=brief`,
/// первые 512 символов, `kinds [idea]`, `status [new, deferred]`, `top_k`
/// `ideas.prompt_top_k`), основной поиск `prompt` не меняется. Идея — кандидат
/// ([`zam_store::idea_candidate`]); идея решения — с баллом ≥ [`prompt_min_score`], общая
/// идея Knowledge — по [`knowledge_ideas_related`]. Показанное сессии и предел
/// `ideas.prompt_max` отсекает общий отбор [`recall`].
///
/// Второй проход — принятые идеи со снятой задачей (§16; ревью задачи 7, I1′):
/// [`released_prompt_ideas`], без эмбеддинга и полного поиска, и только если первый проход дал
/// меньше `ideas.prompt_max` кандидатов, не показанных сессии. У каждого прохода свой срок:
/// просрочка второго не гасит первый.
///
/// Срок — остаток общего (до общего срока сбора минус `ACCOUNTING_RESERVE_MS`): не уложился —
/// тишина по идеям без предупреждений (ни `recall timeout`, ни ошибок поиска идей).
///
/// `review` — режим `review` владельца базы решения (M6.5 §7): идея решения `new` — строка
/// [`idea_review_prompt_line`] (`reason: idea_review`) вместо «предложи»; прочие — по M5.7.
///
/// [`recall`]: super::recall
#[allow(clippy::too_many_arguments)]
pub(super) async fn prompt_ideas(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    session: Option<&RequestSession>,
    health: &mut Health<'_>,
    deadline: &Deadline,
    review: bool,
) -> Result<Vec<Cand>, ApiError> {
    let max = st.cfg.ideas.prompt_max as usize;
    if max == 0 {
        return Ok(Vec::new());
    }
    let query: String = req
        .prompt
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect();
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let at = deadline.at;
    let mut own = Deadline {
        at,
        expired: deadline.expired,
    };
    let warned = health.warnings.len();
    let r = collect_prompt_ideas(st, p, headers, req, &query, health, &mut own, review).await;
    health.warnings.truncate(warned);
    if own.expired {
        return Ok(Vec::new());
    }
    let mut first = r?;
    // (а) первый проход уже дал предел непоказанных — второй не нужен; срок вышел на чтении
    // показанного — первый отдаётся без второго прохода
    let fresh = {
        let refs: Vec<&Cand> = first.iter().collect();
        let Some(seen) = own.step(shown(session, health.targets(), &refs)).await else {
            return Ok(first);
        };
        first
            .iter()
            .filter(|c| !seen.contains(&seen_key(health.targets(), c)))
            .count()
    };
    if fresh >= max {
        return Ok(first);
    }
    // (в) свой срок второго прохода: его просрочка отдаёт первый как есть
    let mut own2 = Deadline { at, expired: false };
    let warned = health.warnings.len();
    let more = released_prompt_ideas(
        p,
        req,
        headers,
        &query,
        session,
        health,
        &mut own2,
        max - fresh,
    )
    .await;
    health.warnings.truncate(warned);
    if !own2.expired {
        first.extend(more);
    }
    Ok(first)
}

/// Тестовая «медленная идея» (фича `test-hooks`): шаг сбора идей «не уложился в срок» `d`.
/// Сон кончается раньше `d.at` на [`TEST_SLOW_MARGIN`] (не больше половины остатка), после чего
/// срок считается вышедшим: тишина по идеям проверяется так же строго, а упаковке и учёту
/// остаётся запас под нагрузкой (иначе ответ упирался бы в `ACCOUNTING_RESERVE_MS`).
#[cfg(feature = "test-hooks")]
async fn test_slow_idea(d: &mut Deadline) {
    let left = d.at.saturating_duration_since(Instant::now());
    let until = d.at - TEST_SLOW_MARGIN.min(left / 2);
    tokio::time::sleep_until(until).await;
    d.expired = true;
}

/// Запас тестовой «медленной идеи» до срока ([`test_slow_idea`]): под нагрузкой диска упаковке
/// и учёту после неё не хватало 0,3 с (сбор 8,35–8,38 с при сроке 8 с, финальное ревью M6.5, F1).
#[cfg(feature = "test-hooks")]
const TEST_SLOW_MARGIN: Duration = Duration::from_millis(2000);

/// Предлагать идеи цели `t` можно, только если `propose` в неё пройдёт: токен `ro` и участник
/// общего решения с `ro` получили бы 403 — им ни раздела `### Идеи`, ни подсказок-идей
/// (финальное ревью M5.7, Minor 3).
fn ideas_writable(p: &Principal, t: &SearchTarget) -> bool {
    crate::api::scope::effective_write(p, t.role).is_ok()
}

/// Есть ли в доступных на запись целях кандидат первого прохода подсказки (`new`/`deferred`):
/// один SQL `LIMIT 1` на цель вместо поиска (финальное ревью M5.7, Minor 5). Сбой — WARN и
/// «есть» (поиск решит сам); `None` — срок вышел.
async fn any_prompt_candidates(
    p: &Principal,
    health: &Health<'_>,
    d: &mut Deadline,
) -> Option<bool> {
    let today = zam_store::time::utc_today();
    for t in health.targets() {
        let spaces = entry_spaces(t);
        if spaces.is_empty() || !ideas_writable(p, t) {
            continue;
        }
        match d
            .step(zam_store::any_new_idea(&t.db, &spaces, today))
            .await?
        {
            Ok(true) => return Some(true),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "recall: кандидаты идей подсказки не прочитаны");
                return Some(true);
            }
        }
    }
    Some(false)
}

/// Лексическое подтверждение идеи решения в подсказке (финальное ревью M5.7, I-1): балл
/// отдельного поиска только по идеям нормирован по непустым каналам, и без порога вектора
/// ближайшая идея получала бы 1,0 на любой запрос. Нужны канал `fts`, `alias` или `trigram` в
/// `sources` ([`super::prompt::lexical`] — общая проверка с коротким запросом подсказок поиска,
/// M6.6 §3.2) и совпадение слов запроса со словами идеи по основам ([`IdeaQuery::matches`]).
fn idea_lexically_matched(item: &Value, e: &Entry, q: &IdeaQuery) -> bool {
    super::prompt::lexical(item) && q.matches(&e.title, &e.body)
}

/// Значимых слов идеи в пороге [`IdeaQuery::matches`], не больше: длинное тело не делает порог
/// недостижимым (половина от 6 — 3 слова, не больше порога покрытия запроса 4).
const IDEA_WORDS_CAP: usize = 6;
/// Порог покрытия слов запроса у подсказок-идей — свой и фиксированный (M5.7: ⌈n/3⌉ от 2 до 4,
/// основы): ключ `recall.post_tool_coverage` (M6 §9) на него не влияет.
const IDEA_QUERY_RULE: CoverageRule = CoverageRule {
    divisor: 3,
    cap: 4,
    word_match: WordMatch::Stems,
};

/// Стоп-основы общих глаголов и слов работы с кодом — **только для слов запроса подсказок-идей**
/// (M6 §16, задача 16; финальное ревью M5.7): «добавь тест на модуль авторизации» не должен
/// подходить к идее только потому, что в её заголовке «Добавить», а в теле — «тесты» и
/// «модуль». Записаны основами `zam_search::recall::stem` (первые `max(4, длина − 3)`
/// символов), ru и en. Общий список служебных слов (`zam_core::text`) не меняется: он влияет на
/// границы разделов.
pub const IDEA_STOP_STEMS: [&str; 17] = [
    "сдел",
    "добав",
    "исправ",
    "обнов",
    "поправ",
    "почин",
    "модул",
    "тест",
    "функци",
    "файл",
    "make",
    "add",
    "fix",
    "updat",
    "chang",
    "modul",
    "test",
];

/// Слово запроса `norm` (после `zam_search::recall::norm_word`) — служебное для подсказок-идей:
/// начинается со стоп-основы [`IDEA_STOP_STEMS`] и длиннее её не больше чем на 3 символа.
/// «добавьте», «tests», «fixed» — да; «changelog», «модульный», «address», «makefile» — нет.
fn is_idea_stop(norm: &str) -> bool {
    let n = norm.chars().count();
    IDEA_STOP_STEMS
        .iter()
        .any(|s| norm.starts_with(s) && n <= s.chars().count() + 3)
}

/// Слова запроса подсказки-идеи, подготовленные один раз на запрос (перф: раньше стоп-список,
/// нормализация и основы запроса пересчитывались на каждого кандидата).
struct IdeaQuery {
    /// Значимые слова запроса: [`prepared_terms`] при [`WordMatch::Stems`] (без служебных слов
    /// ru/en, `ё` → `е`, без повторов) и без слов на стоп-основу ([`is_idea_stop`]).
    words: Vec<StemWord>,
    /// Слова запроса на стоп-основу: совпадением не считаются; не совпавшие с идеей остаются в
    /// знаменателе порога покрытия ([`IdeaQuery::matches`]).
    stops: Vec<StemWord>,
}

impl IdeaQuery {
    fn new(terms: &[String]) -> IdeaQuery {
        let (stops, words): (Vec<String>, Vec<String>) = prepared_terms(terms, WordMatch::Stems)
            .into_iter()
            .partition(|w| is_idea_stop(w));
        let stems = |v: Vec<String>| v.into_iter().map(StemWord::new).collect();
        IdeaQuery {
            words: stems(words),
            stops: stems(stops),
        }
    }

    /// В запросе нет значимых слов: ни одна идея решения им не подтверждается.
    fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Слова запроса ([`error_terms`]) подтверждаются заголовком `title` и телом `body` идеи
    /// (финальное ревью M5.7, I-1′ и I-1″). Служебные слова не считаются ни в запросе, ни в
    /// идее ([`stem_words`]). Нужно хотя бы одно совпадение по основе со словом **заголовка**:
    /// совпадения только с телом («пользователь», «server») связи с замыслом не доказывают.
    /// Далее: покрыты все значимые слова запроса — да; иначе нужно не меньше
    /// `min(порог покрытия запроса, max(1, ⌈k / 2⌉))` совпавших, где `k` — значимые слова идеи
    /// (до [`IDEA_WORDS_CAP`]). Порог ограничен словами самой идеи: к длинному запросу Claude
    /// Code короткая идея иначе не подходила бы никогда. Общий код слов — `zam_search::recall`
    /// ([`StemWord`], задача 9 M6).
    ///
    /// Слова запроса на стоп-основу ([`is_idea_stop`], M6 §16) совпадением не считаются — ни с
    /// заголовком, ни с телом. В знаменателе порога покрытия остаются только те из них, что с
    /// идеей **не совпали** **[решение исполнителя задачи 16, правка ревью]**:
    /// - убрать все — порог `⌈n / 3⌉` падает, и возвращается ложная подсказка таблицы I-1″
    ///   («fix the server crash when this config is missing from the web request» к «Dark theme
    ///   for the web interface»: 7 слов → порог 3, без «fix» 6 слов → порог 2 при двух случайных
    ///   совпадениях; «fix» с идеей не совпадает — остаётся);
    /// - оставить все — двойной штраф: «update», совпавшее с «updates» идеи, и в числитель не
    ///   идёт, и порог поднимает; такое слово нейтрально — вне числителя и знаменателя.
    ///
    /// Слова идеи не фильтруются: слово запроса «тестирование» находит идею со словом «тесты».
    fn matches(&self, title: &str, body: &str) -> bool {
        let q = &self.words;
        if q.is_empty() {
            return false;
        }
        let title_words = stem_words(&error_terms(title));
        if !q.iter().any(|t| title_words.iter().any(|w| t.matches(w))) {
            return false;
        }
        let words = stem_words(&error_terms(&format!("{title}\n{body}")));
        let covered = q
            .iter()
            .filter(|t| words.iter().any(|w| t.matches(w)))
            .count();
        if covered == q.len() {
            return true;
        }
        let stray_stops = self
            .stops
            .iter()
            .filter(|t| !words.iter().any(|w| t.matches(w)))
            .count();
        let query_need = IDEA_QUERY_RULE.need(q.len() + stray_stops);
        let k = words.len().min(IDEA_WORDS_CAP);
        covered >= query_need.min(k.div_ceil(2).max(1))
    }
}

/// Идея подсказки: (цель, идея, общая из Knowledge, пометка «задача снята»).
type FoundIdea = (usize, Entry, bool, Option<&'static str>);

/// Первый проход [`prompt_ideas`] (`status [new, deferred]`) со своим сроком `d`. Ошибки
/// чтения баз не трогают `health.alive`: иначе сбой поиска идей снял бы основные подсказки той
/// же цели.
#[allow(clippy::too_many_arguments)]
async fn collect_prompt_ideas(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    query: &str,
    health: &mut Health<'_>,
    d: &mut Deadline,
    review: bool,
) -> Result<Vec<Cand>, ApiError> {
    match any_prompt_candidates(p, health, d).await {
        Some(true) => {}
        Some(false) | None => return Ok(Vec::new()),
    }
    let Some(found) =
        search_prompt_ideas(st, p, headers, req, query, &["new", "deferred"], health, d).await?
    else {
        return Ok(Vec::new());
    };
    // тестовая задержка (тишина по сроку): только с фичей test-hooks
    #[cfg(feature = "test-hooks")]
    if found
        .iter()
        .any(|(_, e, _, _)| e.title.contains(TEST_SLOW_IDEA_MARK))
    {
        test_slow_idea(d).await;
        return Ok(Vec::new());
    }
    let shared: Vec<&Entry> = found.iter().filter(|f| f.2).map(|f| &f.1).collect();
    let related = if shared.is_empty() {
        HashSet::new()
    } else {
        knowledge_ideas_related(req, headers, &shared, health, d).await
    };
    Ok(found
        .iter()
        .filter(|(_, e, shared, _)| !shared || related.contains(&e.id))
        .map(|(i, e, shared, mark)| {
            // идея решения `new` в режиме `review` — в очереди разбора (M6.5 §7)
            if review && !shared && e.status == Some(EntryStatus::New) {
                idea_cand(*i, e, REASON_IDEA_REVIEW, idea_review_prompt_line(e))
            } else {
                idea_cand(*i, e, REASON_IDEA, idea_prompt_line(e, *mark))
            }
        })
        .collect())
}

/// Принятых идей со снятой задачей в выборке второго прохода, на базу.
const RELEASED_PROMPT_IDS: usize = 50;

/// Второй проход [`prompt_ideas`] — принятые идеи человека со снятой или удалённой задачей
/// (§16; ревью задачи 7, I1′), без эмбеддинга и полного поиска:
/// - SQL [`zam_store::released_ideas`] на базу цели: до [`RELEASED_PROMPT_IDS`] таких идей без
///   открытого вопроса «вернуть» и не показанных этой сессии (`seen_entry_ids`, без эпох);
///   пусто — FTS не идёт;
/// - релевантность — FTS (`search_fts_channels`, `kinds [idea]`, `status [accepted]`) только
///   среди этих id: строгий запрос (все слова) проходит, ослабленный (любое слово) — при
///   подтверждении по основам слов, как у первого прохода ([`IdeaQuery::matches`]). Принятые
///   идеи с живой задачей сюда не попадают и мест не занимают. Запрос без значимых слов
///   ([`IdeaQuery::is_empty`]: только служебные и стоп-основы идей) — прохода нет;
/// - общая идея Knowledge — по [`knowledge_ideas_related`].
///
/// Не больше `want` кандидатов, после основных. Ошибки — WARN, `health.alive` не трогается.
#[allow(clippy::too_many_arguments)]
async fn released_prompt_ideas(
    p: &Principal,
    req: &RecallReq,
    headers: &HeaderMap,
    query: &str,
    session: Option<&RequestSession>,
    health: &Health<'_>,
    d: &mut Deadline,
    want: usize,
) -> Vec<Cand> {
    let iq = IdeaQuery::new(&error_terms(query));
    // в запросе нет значимых слов (служебные и стоп-основы идей, M6 §16): строгий FTS по одним
    // «почини тест» связи с идеей не доказывает — второго прохода нет, как и подтверждения первого
    if iq.is_empty() {
        return Vec::new();
    }
    let today = zam_store::time::utc_today();
    #[cfg(feature = "test-hooks")]
    if let Ok(mut runs) = TEST_SECOND_PASS_QUERIES.lock() {
        runs.push(query.to_string());
    }
    // (сырой балл bm25, цель, идея, общая из Knowledge, пометка)
    let mut hits: Vec<(f64, FoundIdea)> = Vec::new();
    for (i, t) in health.targets().iter().enumerate() {
        let spaces = entry_spaces(t);
        if spaces.is_empty() || !ideas_writable(p, t) {
            continue;
        }
        let exclude: Vec<Id> = match session {
            Some(s) => {
                let Some(r) = d
                    .step(zam_store::seen_entry_ids(
                        &s.db,
                        &s.touch.id,
                        t.owner,
                        RECALL_PART,
                    ))
                    .await
                else {
                    return Vec::new();
                };
                match r {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::warn!(error = %e, "recall: показанное сессии не прочитано");
                        continue;
                    }
                }
            }
            None => Vec::new(),
        };
        let Some(r) = d
            .step(zam_store::released_ideas(
                &t.db,
                &spaces,
                &exclude,
                RELEASED_PROMPT_IDS,
            ))
            .await
        else {
            return Vec::new();
        };
        let ids: HashSet<Id> = match r {
            Ok(ids) => ids.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "recall: принятые идеи подсказки не прочитаны");
                continue;
            }
        };
        if ids.is_empty() {
            continue;
        }
        let q = FtsQuery {
            alias_groups: Vec::new(),
            text: query.to_string(),
            space_ids: spaces,
            path_prefix: None,
            top_k: 100,
            targets: TargetFilter {
                kinds: vec!["idea".into()],
                statuses: vec!["accepted".into()],
                area: None,
                certainty: Vec::new(),
                decided_by: None,
                file: None,
                insight_kinds: Vec::new(),
                // только выборка: принятые идеи с живой задачей не занимают места канала bm25
                entry_ids: ids.iter().copied().collect(),
            },
        };
        let Some(r) = d.step(search_fts_channels(&t.db, q)).await else {
            return Vec::new();
        };
        let ch = match r {
            Ok(ch) => ch,
            Err(e) => {
                tracing::warn!(error = %e, "recall: поиск принятых идей подсказки не удался");
                continue;
            }
        };
        let mut taken: HashSet<Id> = HashSet::new();
        for h in ch.bm25 {
            if !ids.contains(&h.target.id) || !taken.insert(h.target.id) {
                continue;
            }
            if h.entry.is_none() {
                continue;
            }
            // источник истины — SQLite (`origin`, задача, вопрос «вернуть»)
            let Some(r) = d.step(zam_store::get_entry(&t.db, h.target.id)).await else {
                return Vec::new();
            };
            let e = match r {
                Ok(e) => e,
                Err(StoreError::NotFound) => continue,
                Err(err) => {
                    tracing::warn!(error = %err, "recall: идея подсказки не прочитана");
                    continue;
                }
            };
            // строгий FTS (все слова запроса) проходит сразу; ослабленный — по основам слов
            if ch.relaxed && !iq.matches(&e.title, &e.body) {
                continue;
            }
            let shared = t
                .spaces
                .iter()
                .any(|s| s.id == h.space_id && s.kind == SpaceKind::Knowledge);
            let Some(r) = d.step(zam_store::idea_task_state(&t.db, &e)).await else {
                return Vec::new();
            };
            let task = match r {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(error = %err, "recall: задача идеи подсказки не прочитана");
                    continue;
                }
            };
            if zam_store::idea_candidate(&e, task, today) {
                let mark = released_mark(&e, task);
                hits.push((h.raw_score, (i, e, shared, mark)));
            }
        }
    }
    // bm25: меньше — лучше (слияние баз — по сырому баллу, как в поиске)
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    let found: Vec<FoundIdea> = hits.into_iter().map(|(_, f)| f).collect();
    #[cfg(feature = "test-hooks")]
    if found
        .iter()
        .any(|(_, e, _, _)| e.title.contains(TEST_SLOW_IDEA_MARK))
    {
        test_slow_idea(d).await;
        return Vec::new();
    }
    let shared: Vec<&Entry> = found.iter().filter(|f| f.2).map(|f| &f.1).collect();
    let related = if shared.is_empty() {
        HashSet::new()
    } else {
        knowledge_ideas_related(req, headers, &shared, health, d).await
    };
    found
        .iter()
        .filter(|(_, e, shared, _)| !shared || related.contains(&e.id))
        .take(want)
        .map(|(i, e, _, mark)| idea_cand(*i, e, REASON_IDEA, idea_prompt_line(e, *mark)))
        .collect()
}

/// Наименьшая глубина каналов лёгкого поиска идей ([`idea_light_search`]).
const IDEA_LIGHT_DEPTH_MIN: usize = 20;

/// Облегчение отдельного поиска идей к `prompt` (M6 §16, задача 16): при `top_k` 5 обычный
/// поиск строил хиты каналов FTS до 50 и сниппеты к ним — 8 мс медианы в отладочной сборке на 50
/// идеях против 3 мс основного поиска. Каналы — не длиннее `max(4 × top_k, 20)`, сниппеты не
/// строятся: подсказке нужны только id, балл и каналы находки, а заголовок и тело для
/// лексического подтверждения читаются из базы по id. Основной поиск `prompt` не меняется.
pub(super) fn idea_light_search(top_k: usize) -> LightSearch {
    LightSearch {
        channel_depth: (4 * top_k).max(IDEA_LIGHT_DEPTH_MIN),
        snippets: false,
        any_terms: false,
    }
}

/// Один проход отдельного поиска идей [`collect_prompt_ideas`] по статусам `statuses`:
/// кандидаты [`zam_store::idea_candidate`] в порядке выдачи. `None` — срок истёк. Поиск —
/// лёгкий ([`idea_light_search`]).
#[allow(clippy::too_many_arguments)]
async fn search_prompt_ideas(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    query: &str,
    statuses: &[&str],
    health: &mut Health<'_>,
    d: &mut Deadline,
) -> Result<Option<Vec<FoundIdea>>, ApiError> {
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": true, "include_rules": false},
        "filters": {"kinds": ["idea"], "status": statuses},
        "mode": "brief",
        "top_k": st.cfg.ideas.prompt_top_k,
        "budget_tokens": PROMPT_SEARCH_BUDGET,
        // отсечка карточек поиска (M6 §4.2) — не для подсказок: у них свой порог балла
        "brief_min_rel": 0.0,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let area = req.area.as_deref();
    let light = Some(idea_light_search(st.cfg.ideas.prompt_top_k as usize));
    #[cfg(feature = "test-hooks")]
    let light = light.filter(|_| !TEST_IDEA_LIGHT_OFF.load(std::sync::atomic::Ordering::Relaxed));
    #[cfg(feature = "test-hooks")]
    let search_t = std::time::Instant::now();
    let found = inner_search(st, p, headers, area, body, light, health, d).await?;
    #[cfg(feature = "test-hooks")]
    if let Ok(mut times) = TEST_IDEA_SEARCH_TIMES.lock() {
        let stages = found.as_ref().map_or(Value::Null, |v| v["timings"].clone());
        times.push((search_t.elapsed(), stages));
    }
    let Some(v) = found else {
        return Ok(Some(Vec::new()));
    };
    let min = prompt_min_score(st, &v);
    let today = zam_store::time::utc_today();
    let iq = IdeaQuery::new(&error_terms(query));
    let targets = health.targets();
    let mut found: Vec<FoundIdea> = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        if it["kind"] != "idea" {
            continue;
        }
        let Some(id) = it["entry_id"].as_str().and_then(|s| s.parse::<Id>().ok()) else {
            continue;
        };
        let Some(space) = it["space"]["id"]
            .as_str()
            .and_then(|s| s.parse::<Id>().ok())
        else {
            continue;
        };
        let Some(target) = targets
            .iter()
            .position(|t| t.spaces.iter().any(|s| s.id == space))
        else {
            continue;
        };
        if !ideas_writable(p, &targets[target]) {
            continue;
        }
        let shared = match it["space"]["kind"].as_str() {
            Some("solution") if it["score"].as_f64().unwrap_or(0.0) >= min => false,
            Some("knowledge") => true,
            _ => continue,
        };
        // источник истины — SQLite: `origin`, наступивший `horizon` (статусы выдачи шире)
        let db = targets[target].db.clone();
        let Some(r) = d.step(zam_store::get_entry(&db, id)).await else {
            return Ok(None);
        };
        let e = match r {
            Ok(e) => e,
            Err(StoreError::NotFound) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "recall: идея подсказки не прочитана");
                continue;
            }
        };
        if !shared && !idea_lexically_matched(it, &e, &iq) {
            continue;
        }
        // у `accepted` — состояние задачи (§16): кандидат, только если она снята или удалена
        let task = if e.status == Some(EntryStatus::Accepted) {
            let Some(r) = d.step(zam_store::idea_task_state(&db, &e)).await else {
                return Ok(None);
            };
            match r {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(error = %err, "recall: задача идеи подсказки не прочитана");
                    continue;
                }
            }
        } else {
            None
        };
        if zam_store::idea_candidate(&e, task, today) {
            let mark = released_mark(&e, task);
            found.push((target, e, shared, mark));
        }
    }
    Ok(Some(found))
}

/// **Единственное место правила** «явная связь общей идеи Knowledge с текущей работой»
/// (ответ 2 владельца; ответ владельца на вопрос 8 — вариант А, 2026-11-04): идея из `ideas`
/// проходит, только если
/// - тег или `area` идеи совпадает с тегом или `area` задачи `in_progress`, захваченной
///   сессией запроса (решение и Knowledge), или
/// - у идеи есть ребро (в любую сторону, глубина 1) именно к такой задаче.
///
/// Ребро к произвольной записи решения связи не даёт, косинус не учитывается.
pub(super) async fn knowledge_ideas_related(
    req: &RecallReq,
    headers: &HeaderMap,
    ideas: &[&Entry],
    health: &Health<'_>,
    d: &mut Deadline,
) -> HashSet<Id> {
    let mut out: HashSet<Id> = HashSet::new();
    let targets = health.targets();
    let Some((ki, kn)) = personal_space(targets, SpaceKind::Knowledge) else {
        return out;
    };
    let sol = solution_space(targets);
    // задачи `in_progress`, захваченные сессией запроса
    let session = req.session_id.clone().or_else(|| {
        crate::attrib::header_value(headers, SESSION_HEADER)
            .ok()
            .flatten()
    });
    let mut tasks: Vec<Entry> = Vec::new();
    if let Some(sid) = session.as_deref() {
        let now = zam_store::time::now();
        let spaces: Vec<(usize, Id)> = sol.into_iter().chain([(ki, kn)]).collect();
        for (i, space) in spaces {
            let f = EntryFilter {
                statuses: vec![EntryStatus::InProgress],
                ..entry_filter(space, EntryKind::Task)
            };
            let db = targets[i].db.clone();
            let Some(r) = d.step(list_entries(&db, f)).await else {
                return out;
            };
            match r {
                Ok((rows, _)) => tasks.extend(rows.into_iter().filter(|e| {
                    zam_store::TaskData::from_stored(&e.data)
                        .claimed_by
                        .is_some_and(|c| c.session == sid && c.live(now))
                })),
                Err(e) => tracing::warn!(error = %e, "recall: задачи сессии не прочитаны"),
            }
        }
    }
    if tasks.is_empty() {
        return out;
    }
    let task_ids: HashSet<Id> = tasks.iter().map(|t| t.id).collect();
    let graph_spaces: Vec<Id> = targets[ki].space_ids();
    for e in ideas {
        let by_tag = tasks.iter().any(|t| {
            t.tags.iter().any(|g| e.tags.contains(g)) || (t.area.is_some() && t.area == e.area)
        });
        if by_tag {
            out.insert(e.id);
            continue;
        }
        let start = [TargetRef {
            kind: TargetKind::Entry,
            id: e.id,
        }];
        let db = targets[ki].db.clone();
        let Some(r) = d.step(db.neighbors(&start, 1, &[], &graph_spaces)).await else {
            return out;
        };
        match r {
            Ok(g) => {
                if g.nodes
                    .iter()
                    .any(|n| n.r.id != e.id && !n.deleted && task_ids.contains(&n.r.id))
                {
                    out.insert(e.id);
                }
            }
            Err(err) => tracing::warn!(error = %err, "recall: связи идеи не прочитаны"),
        }
    }
    out
}

/// Строка плана (M6.5 §7, [`PLAN_LINE`]): в решении запроса есть пункты плана из идей —
/// живые задачи `open | in_progress | blocked` с ребром `derived_from` на идею
/// ([`zam_store::planned_from_ideas`]). Нет пунктов, нет права записи (взять пункт плана нельзя),
/// цель исключена или срок вышел — строки нет; сбой чтения — WARN, дайджест не валится.
pub(super) async fn plan_line(
    p: &Principal,
    health: &Health<'_>,
    deadline: &mut Deadline,
    sol: Option<(usize, Id)>,
) -> Option<String> {
    let (i, space) = sol?;
    let t = &health.targets()[i];
    if !health.alive[i] || !ideas_writable(p, t) {
        return None;
    }
    match deadline
        .step(zam_store::planned_from_ideas(&t.db, space))
        .await?
    {
        Ok((n, next)) if n > 0 => Some(plan_text(n, next.as_ref())),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "recall: пункты плана из идей не прочитаны");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // основы слов (бывш. `idea_stem_match_forms_and_short_words`) — тест `zam_search::recall`
    // `stem_word_match_forms_and_short_words` (задача 9 M6)

    /// Правило идей для слов запроса `terms`, заголовка и тела ([`IdeaQuery::matches`]).
    fn idea_words_match(terms: &[String], title: &str, body: &str) -> bool {
        IdeaQuery::new(terms).matches(title, body)
    }

    /// M6 §16 (задача 16): слово служебное для идей, если начинается со стоп-основы и длиннее
    /// её не больше чем на 3 символа. Список — основы `zam_search::recall::stem` своих слов.
    #[test]
    fn idea_stop_stems_examples() {
        use zam_search::recall::{norm_word, stem};
        for w in [
            "добавьте",
            "добавь",
            "добавить",
            "сделай",
            "сделать",
            "исправь",
            "исправить",
            "обнови",
            "обновить",
            "поправь",
            "поправить",
            "почини",
            "починить",
            "модуль",
            "модуля",
            "тест",
            "тесты",
            "тестов",
            "функция",
            "функцию",
            "файл",
            "файле",
            "файлами",
            "make",
            "makes",
            "add",
            "added",
            "fix",
            "fixed",
            "update",
            "updated",
            "change",
            "changes",
            "module",
            "modules",
            "test",
            "tests",
        ] {
            assert!(is_idea_stop(&norm_word(w)), "служебное: {w}");
        }
        // длиннее основы на 4 и больше — другое слово; чужое начало — тоже
        for w in [
            "changelog",
            "модульный",
            "address",
            "makefile",
            "файловый",
            "функциональный",
            "тестировщиков",
            "тестирование",
            "additional",
            "fixtures",
            "экспорт",
            "отчёт",
            "pdf",
            "ad",
            "обновление",
        ] {
            assert!(!is_idea_stop(&norm_word(w)), "не служебное: {w}");
        }
        // регистр и «ё» — после нормализации; без неё заглавное слово не совпало бы с основой
        assert!(is_idea_stop(&norm_word("ДОБАВЬТЕ")) && is_idea_stop(&norm_word("Тёст")));
        // запись списка: каждая основа — основа хотя бы одного своего слова ([`stem`])
        let forms = [
            "сделать",
            "добавить",
            "исправить",
            "обновить",
            "поправить",
            "починить",
            "модулями",
            "тест",
            "функциями",
            "файл",
            "make",
            "add",
            "fix",
            "updating",
            "changing",
            "modulate",
            "test",
        ];
        assert_eq!(forms.len(), IDEA_STOP_STEMS.len());
        for (s, w) in IDEA_STOP_STEMS.iter().zip(forms) {
            assert_eq!(stem(w), *s, "{w}");
        }
        // значимые слова запроса и слова на стоп-основу
        let words = |t: &str| {
            let q = IdeaQuery::new(&error_terms(t));
            (q.words.len(), q.stops.len())
        };
        assert_eq!(words("Добавьте тесты на модуль авторизации"), (1, 3));
        assert_eq!(words("update the changelog and the makefile"), (2, 1));
        assert_eq!(words("fix tests"), (0, 2));
        assert!(IdeaQuery::new(&error_terms("fix tests")).is_empty());
    }

    /// Ревью задачи 4 (Minor 3, раунд 2): строки после пакета — пустая строка после блока,
    /// «показано k из N»; не вошедшие в бюджет — адрес по случаю; под пределом вывода — без
    /// строки «не показано» (её даёт предел).
    #[test]
    fn review_shown_line_advice() {
        assert_eq!(
            review_shown_line(1, 1, 1, false, true),
            "\nПоказано идей на разбор: 1 из 1.\n"
        );
        assert_eq!(
            review_shown_line(1, 1, 3, false, false),
            "Показано идей на разбор: 1 из 3.\n"
        );
        assert_eq!(
            review_shown_line(1, 3, 4, false, true),
            "\nПоказано идей на разбор: 1 из 4.\nZeroAIMemory: не показано — идей на разбор: 2 \
             (memory_recall event=idea_review после разбора показанных)\n"
        );
        assert_eq!(
            review_shown_line(0, 2, 2, false, false),
            "Показано идей на разбор: 0 из 2.\nZeroAIMemory: не показано — идей на разбор: 2 \
             (подними budget_tokens или уменьши limit)\n"
        );
        assert_eq!(
            review_shown_line(1, 3, 4, true, true),
            "\nПоказано идей на разбор: 1 из 4.\n"
        );
    }

    /// M6 §16 (задача 16): глубина каналов лёгкого поиска идей — `max(4 × top_k, 20)`, без
    /// сниппетов.
    #[test]
    fn idea_light_search_depth() {
        for (top_k, depth) in [(1, 20), (5, 20), (6, 24), (8, 32), (20, 80)] {
            assert_eq!(
                idea_light_search(top_k),
                LightSearch {
                    channel_depth: depth,
                    snippets: false,
                    any_terms: false
                },
                "top_k {top_k}"
            );
        }
    }

    /// M6 §16 (задача 16): общий глагол и слова работы с кодом связи с идеей не доказывают;
    /// значимые слова — доказывают по-прежнему.
    #[test]
    fn idea_stop_stems_in_words_match() {
        let title = "Добавить экспорт отчётов в PDF";
        let body = "Сделать модуль выгрузки, функцию печати в файл и тесты.";
        let m = |q: &str| idea_words_match(&error_terms(q), title, body);
        for q in [
            "добавь тест на модуль авторизации",
            "сделай функцию и добавь её в файл",
            "исправь тесты модуля",
            "почини и обнови файл с функцией",
            "поправь модуль",
            "add a test for the module and fix the file",
        ] {
            assert!(!m(q), "только стоп-основы: {q}");
        }
        for q in [
            "добавь экспорт отчётов в PDF",
            "сделай модуль экспорта",
            "экспортировать отчёты в pdf",
            "тесты на экспорт отчётов",
        ] {
            assert!(m(q), "значимые слова: {q}");
        }
        // запрос из одних стоп-основ не подтверждает и идею из тех же слов
        assert!(!idea_words_match(
            &error_terms("обнови тесты модуля"),
            "Обновить тесты модуля",
            "Починить файл."
        ));
        // слова идеи не фильтруются: значимое слово запроса находит слово идеи на стоп-основу
        assert!(idea_words_match(
            &error_terms("тестирование оплаты"),
            "Добавить тесты на модуль оплаты",
            "Замысел."
        ));
        // слово на стоп-основу, не совпавшее с идеей, остаётся в знаменателе порога: без «fix»
        // порог ⌈6/3⌉ = 2 вернул бы ложную подсказку таблицы I-1″ (совпадают только «server» и
        // «web»)
        let q = "fix the server crash when this config is missing from the web request";
        let iq = IdeaQuery::new(&error_terms(q));
        assert_eq!((iq.words.len(), iq.stops.len()), (6, 1));
        assert!(!iq.matches(
            "Dark theme for the web interface",
            "This would make the server UI nicer."
        ));
    }

    /// Ревью задачи 16 (Minor 2): слово запроса на стоп-основу, **совпавшее** со словом идеи,
    /// нейтрально — ни в числителе, ни в знаменателе порога. Иначе двойной штраф: «update» и
    /// «testing» совпадением не считаются и при этом поднимают порог с 2 до 3, и запрос с двумя
    /// значимыми совпадениями («checklist», «payment») идею теряет, хотя M5.7 её находил.
    #[test]
    fn idea_stop_matched_word_is_neutral() {
        let title = "Testing checklist for payment provider updates";
        let body = "A short manual checklist to run on the stand before switching the payment \
                    gateway API version.";
        let q = "update testing checklist for payment, linter docs and deploy";
        let iq = IdeaQuery::new(&error_terms(q));
        // значимых слов 5 (checklist, payment, linter, docs, deploy), на стоп-основу — 2
        assert_eq!((iq.words.len(), iq.stops.len()), (5, 2));
        assert!(iq.matches(title, body), "порог 2 от пяти значимых слов");
        // те же слова на стоп-основу к идее без них — в знаменателе: 5 + 2 = 7 слов → порог 3
        let other = "Checklist for the payment provider";
        let other_body = "A short manual list to run on the stand before switching the gateway.";
        assert!(
            !iq.matches(other, other_body),
            "порог 3 при двух совпадениях"
        );
        // и одно несовпавшее слово на стоп-основу («add») порог не меняет: 5 + 1 = 6 → 2
        let q2 = "add testing checklist for payment, linter docs and deploy";
        assert!(IdeaQuery::new(&error_terms(q2)).matches(title, body));
    }

    /// Порог подтверждения ограничен словами идеи: длинный запрос к короткой идее подходит.
    #[test]
    fn idea_words_match_long_queries() {
        let ok = |q: &str| idea_words_match(&error_terms(q), "Экспорт отчётов в PDF", "Замысел.");
        assert!(ok("экспортировать отчёты в pdf"));
        assert!(ok(
            "Пользователи просят, чтобы отчёты можно было скачать в PDF; посмотри модуль reports и предложи план, с чего начать"
        ));
        assert!(ok("как сделать экспорт отчётов в PDF"));
        assert!(!ok("настрой логирование сервера"));
        assert!(!ok(
            "Почини падающий тест миграции базы данных и обнови документацию по развёртыванию на стенде"
        ));
        assert!(!ok(""));
        // ложные срабатывания по служебным словам и словам тела (I-1″)
        let body = "Нужно, чтобы пользователь мог скачать отчёт. Это удобно для клиентов.";
        assert!(!idea_words_match(
            &error_terms("Нужно поправить авторизацию, чтобы пользователь не вылетал при входе"),
            "Экспорт отчётов в PDF",
            body
        ));
        // с тем же телом связанные запросы проходят (совпадение с заголовком)
        assert!(idea_words_match(
            &error_terms("экспортировать отчёты в pdf"),
            "Экспорт отчётов в PDF",
            body
        ));
        assert!(!idea_words_match(
            &error_terms("fix the server crash when this config is missing from the web request"),
            "Dark theme for the web interface",
            "This would make the server UI nicer."
        ));
        assert!(idea_words_match(
            &error_terms("add a dark theme to the web interface"),
            "Dark theme for the web interface",
            "This would make the server UI nicer."
        ));
        // служебные слова не считаются
        assert!(zam_core::text::is_stop_word("чтобы") && zam_core::text::is_stop_word("the"));
        assert_eq!(
            prepared_terms(&error_terms("Ещё еще отчёты отчеты для"), WordMatch::Stems),
            ["отчеты"]
        );
    }
}
