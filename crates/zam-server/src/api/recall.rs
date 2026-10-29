//! Проактивное вспоминание `POST /recall` (§10.3, §12; задача 8 M4) — подсказки хукам агента
//! (задача 10) и инструменту MCP `memory_recall` (задача 9).
//!
//! ```text
//! POST /recall {event, session_id?, solution?, area?, prompt?, error?, tool?, budget_tokens?,
//!               agent_block_version?, task?, parent_id?}
//!   → 200 {text, items: [{id, kind, title, certainty, reason, age_days?}], tokens, timings,
//!          warnings, hooks?}
//! ```
//!
//! - `text` — готовый Markdown для `additionalContext`; пусто — тишина. Каждая подсказка — строка
//!   `ZeroAIMemory: <заголовок> [<вид>, <certainty>] — i:<id>` (у заметки — `n:<id>`, у
//!   changelog — `c:<id>`), под ней с отступом может идти пояснение (L1 правила, фрагмент).
//!   `items` — показанные подсказки по порядку: `id` — та же ссылка, `kind` — `insight`,
//!   `changelog`, `note` или `folder`, `reason` — откуда подсказка (`rules`, `solution`,
//!   `gotcha`, `failure`, `revisit`, `changelog`, `search`, `error_regex`, `error_fts`),
//!   `age_days` — возраст записи в сутках (M4.5 §7; нет у строк окружения, файлов, терминов и
//!   триггеров `error_regex`: время там неизвестно); `tokens` — размер `text` (счётчик
//!   чанкера), `timings {collect_ms, record_ms, total_ms}`;
//! - **учёт выданного** (M4.5 §9.5): непустой `text` прибавляется к `session_tokens` сессии —
//!   событие `session_start`, `recovery` (и `session_start` после `compact | resume`),
//!   `prompt`, `post_tool` или `brief`, `calls + 1`, `tokens + tokens` ответа — в той же
//!   транзакции, что `session_seen` (без лишней синхронизации диска); `record: false`, нет
//!   сессии или вышел срок учёта — без прибавки;
//! - **`session_start`** — дайджест не больше `budget_tokens` (по умолчанию
//!   `recall.digest_tokens` = 1500) по приоритету: напоминание об устаревшем блоке инструкции
//!   (`agent_block_version` меньше версии сервера [`crate::agent_setup::current_version`] —
//!   `max(AGENT_BLOCK_VERSION, версия шаблонов)`), ожидающие запросы доступа к решениям
//!   пользователя (`access_requests_pending`), `### Задачи` (M4.5 §3: задачи `in_progress` и
//!   `blocked` решения и Knowledge, до 5, захваченные сессией запроса — первыми, затем недавно
//!   менявшиеся; строка `[<статус>] <заголовок> — далее: … — k:<id>`, у `blocked` — ещё
//!   ` — блокер: …`; `reason: task`), `### Ответы человека` (M4.5 §4: вопросы, отвеченные за
//!   [`ANSWERS_DAYS`] дней и ещё не показанные этой сессии — `session_seen`, до 5; строка
//!   `ZeroAIMemory: ответ на «<вопрос>»: <ответ> — q:<id> → i:<решение>`, `reason: answer`) и
//!   `### Открытые вопросы` (`N открытых: «…», «…», «…»`, до [`OPEN_QUESTIONS_TITLES`]
//!   заголовков, не подсказка записи), `### Окружение` (M4.5 §8: до [`ENVIRONMENT_MAX`] записей
//!   Vault `host`/`environment`, `<имя> — <purpose>; <address>; ограничения: … — v:<id>`; без
//!   учёта показа; в `recovery` и `brief` нет), Rules (L1 каждой заметки, до
//!   [`RULES_MAX`] заметок, L1 — до [`RULE_L1_TOKENS`] токенов), «L1 решения» — корневые узлы
//!   пространства решения с L0 (до [`SOLUTION_ROOT_MAX`]), до 5 действующих (`active`)
//!   `gotcha`/`failure` решения и Knowledge (новые первыми), до 5 записей `revisit`, последние
//!   5 записей changelog решения **[решение исполнителя]**; сразу после правил — строка
//!   `Словарь: N терминов — memory_search kinds=[term]` (N > 0, термины решения и Knowledge) и
//!   строка `Проверенные команды: <путь> (проверено <verified_at>)[; …]` — до
//!   [`VERIFIED_COMMANDS_MAX`] заметок пространства решения с `verified_at`, новые проверки
//!   первыми (M4.5 §7, решение владельца 2026-10-28).
//!   Ответ несёт `hooks: {session_start, prompt, pre_tool, post_tool, checkpoint,
//!   subagent_start, file_context, session_journal, recovery}` — настройки типов подсказок
//!   пользователя `hooks.<тип>` (R5, [`super::user_settings`], задача 13). Тип запроса выключен у
//!   пользователя на сервере — пустой `text` без сбора и без учёта показа (`session_seen`,
//!   `usage`; сессия касается), у `session_start` — с `hooks` (N4); у `recovery` и `brief` гейта
//!   нет;
//! - **`prompt`** — поиск `mode=brief` (без реранка, без кэша выжимок, `skip_seen: false`) по
//!   первым 512 символам `prompt` в Knowledge, Rules и решении; из выдачи — только insight и
//!   заметки Rules с баллом ≥ `recall.prompt_min_score` (0,6), не больше [`PROMPT_MAX`];
//!   перед ними — до [`TERM_HINTS_MAX`] строк словаря (M4.5): имя или псевдоним действующего
//!   термина решения и Knowledge в первых 512 символах `prompt` —
//!   `ZeroAIMemory: термин «<имя>» — <определение> — t:<id>`, `reason: glossary`, каждый термин
//!   раз за сессию (`session_seen`); словарь — из кэша [`crate::glossary::GlossaryCache`];
//! - **`post_tool`** — ошибка инструмента: совпадения `error_regex` действующих триггеров
//!   insight решения и Knowledge (по первым [`ERROR_SCAN_MAX`] байтам `error`; триггер с
//!   `tools` — только для этих инструментов, если `tool` задан), затем FTS по insight по
//!   нормализованной ошибке ([`normalize_error`]): строгий запрос (все слова) — как есть,
//!   ослабленный (любое слово) — если запись покрывает достаточно слов ошибки
//!   ([`coverage_enough`]). Не больше [`POST_TOOL_MAX`]; строка — «ZeroAIMemory: эта ошибка уже
//!   разбиралась: …»;
//! - **без повторов**: подсказка `prompt` показывается не больше раза за сессию — по
//!   `session_seen` с частью [`RECALL_PART`] (любая ревизия). Дайджест `session_start` не
//!   фильтруется (после `/compact` и `resume` Claude Code шлёт тот же `session_id`, а контекст
//!   агента потерян), `post_tool` выдаёт подсказку при каждой ошибке; их показы тоже
//!   отмечаются — `prompt` их уже не повторит. Показанное отмечается в `session_seen` базы
//!   пользователя запроса (касание сессии и отметка — одна транзакция), каждый показ —
//!   `usage(recall_shown, <событие>)` в базе цели (у общего решения — владельца). Сессия —
//!   `session_id` или `X-Zam-Session`; без неё подсказки выдаются без учёта повторов и без
//!   `session_seen` **[решение исполнителя]** (MCP без сессии); некорректный id — 400. Сбой учёта —
//!   WARN, ответ не портится;
//! - **область** — как у поиска ([`resolve_targets`]): личная база (Knowledge, Rules, своё
//!   решение) и база владельца доступного чужого решения (только пространство решения). Human
//!   и чужой Knowledge не подмешиваются никогда; решение без доступа исключается с
//!   `warnings: ["solution access pending|rejected|revoked"]`, недоступная база владельца —
//!   `solution unavailable`. Решение — `solution` тела (не GUID — 400) или `X-Zam-Solution`;
//! - **бюджет сервера** `recall.timeout_ms` (200): шаги сбора идут до `timeout_ms −`
//!   [`ACCOUNTING_RESERVE_MS`] (40 мс, не больше половины срока), по превышении отдаётся
//!   собранное и `warnings: ["recall timeout"]`; учёт показов — после сбора и только до полного
//!   срока: вышел — собранное отдаётся без `session_seen` и `usage` (`timings.record_ms: null`)
//!   и с тем же предупреждением. Сбой внутреннего поиска `prompt` — тишина и
//!   `warnings: ["recall search failed"]`;
//! - `record: false` (по умолчанию `true`) — ответ без учёта: ни `session_seen`, ни `usage`,
//!   `timings.record_ms: null` (хук `session-start` с выключенным в клиенте дайджестом берёт
//!   только `hooks`; задача 10, fix round 2);
//! - **пакет восстановления** (M4.5 §9.2) — `event: recovery` (MCP, агенты без
//!   `SessionStart compact`) или `session_start` с `source` `compact | resume`: бюджет по
//!   умолчанию `recall.recovery_tokens` (2000), разделы по порядку — `### Задачи` (захваченные
//!   сессией первыми), `### Ответы человека` **без** фильтра `session_seen` (контекст агента
//!   потерян; показ всё равно отмечается), `### Решения` (действующие `decision` пространства
//!   решения за [`RECOVERY_DECISIONS_DAYS`] дней, до 5, `reason: decision`), затем обычный
//!   дайджест в остатке бюджета (уведомления, открытые вопросы, правила, словарь, …).
//!   У `session_start` ключ пользователя `hooks.recovery` выключен — обычный дайджест,
//!   `hooks.session_start` выключен — тишина. `event: recovery` (MCP) от настроек подсказок
//!   хуков не зависит (§0, §10). `hooks` в ответе — только у `session_start`;
//! - **пакет под задачу `brief`** (M4.5 §9.4) — `task` (первые [`BRIEF_TASK_MAX_CHARS`]
//!   символов, длиннее — обрезка и `warnings: ["task truncated"]`), бюджет по умолчанию
//!   `recall.brief_tokens` (800); разделы: `### Правила` (L0 заметок Rules), `### Решения`
//!   (действующие `decision` с `axiom`/`theorem` по поиску `task`, до 3), `### Термины`
//!   (термины словаря в `task`, до 5, с определением), `### Ловушки` (действующие
//!   `gotcha`/`failure` по поиску `task`, до 3), `### Файлы` (записи со ссылками на пути из
//!   `task` — индекс `file_refs` решения, до 3). Без `task` — мини-дайджест: правила L0,
//!   ловушки решения и Knowledge, `### Задачи` (захваченные `parent_id` или сессией — первыми).
//!   Поиск — `mode=brief`, балл ≥ `recall.prompt_min_score`. Повторы не отсекаются (у субагента
//!   свой контекст); показ учитывается (`usage.source = brief`), кроме записей раздела
//!   «Файлы» (ревизия неизвестна — `session_seen` не отмечается). От настроек подсказок хуков
//!   не зависит: хук `subagent-start` гейтится на клиенте;
//! - неизвестные поля тела игнорируются (клиент хуков может быть новее); прочие `source` хука
//!   (`startup | clear | fork`) — обычный дайджест, `source` пишется в журнал DEBUG;
//! - `budget_tokens` — 1..=200000 (иначе 400); `event` — `session_start | prompt | post_tool |
//!   recovery | brief` (иначе 400; `pre_tool` — только локально в хуке, §10.3).
//!
//! Маршрут — служебный, рядом с `/search` и `/read`: `/recall` пишет только `usage` и
//! `session_seen`, а не память, поэтому `memory_writes` не растёт (решение контроллера R12).
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use zam_core::{AGENT_BLOCK_VERSION, Id, TargetKind, TargetRef};
use zam_search::recall::{
    Section, coverage_enough, error_regex_matches, error_terms, hint_line, normalize_error,
    one_line, pack_sections, term_coverage,
};
use zam_search::truncate_to_tokens;
use zam_store::{
    Certainty, Entry, EntryFilter, EntryKind, EntryStatus, FtsQuery, NodeKind, RECALL_PART,
    SeenItem, SeenKey, SpaceKind, StoreError, TargetFilter, TokenCount, TriggerKind, UsageEvent,
    UsageKind, UsageSource, active_triggers, count_tokens, list_children, list_entries,
    mark_seen_with_tokens, node_content, record_usages, search_fts_channels, seen_revs,
    touch_session,
};

use super::json_body;
use super::nodes::read::l1;
use super::search::targets::{ScopeReq, SearchTarget, dedup_keep_order, resolve_targets};
use super::search::{Health, SOLUTION_HEADER};
use super::sessions::{RequestSession, request_session};
use super::user_settings::{HookFlags, hook_flags};
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, SESSION_HEADER};
use crate::auth::{Principal, target_user_id};
use crate::error::ApiError;

/// Предупреждение: сбор подсказок не уложился в `recall.timeout_ms`.
pub const WARN_RECALL_TIMEOUT: &str = "recall timeout";
/// Предупреждение: внутренний поиск подсказок к запросу не удался (ответ — тишина).
pub const WARN_PROMPT_SEARCH_FAILED: &str = "recall search failed";
/// Резерв срока на учёт показа (`session_seen`, `usage`), мс: сбор идёт до
/// `timeout_ms − резерв` (не больше половины `timeout_ms`).
pub const ACCOUNTING_RESERVE_MS: u64 = 40;
/// Тестовая метка заметки Rules: её чтение задерживается на [`TEST_SLOW_RULE_MS`] (фича
/// `test-hooks`, проверка частичного ответа по тайм-ауту).
#[cfg(feature = "test-hooks")]
pub const TEST_SLOW_RULE_MARK: &str = "zam-test-slow-rule";
#[cfg(feature = "test-hooks")]
const TEST_SLOW_RULE_MS: u64 = 40;
/// Заметок Rules в дайджесте.
pub const RULES_MAX: usize = 20;
/// Предел L1 одной заметки Rules в дайджесте, токенов.
pub const RULE_L1_TOKENS: usize = 200;
/// Корневых узлов решения («L1 решения») в дайджесте.
pub const SOLUTION_ROOT_MAX: usize = 10;
/// Действующих `gotcha`/`failure`, записей `revisit` и changelog в дайджесте — каждого.
pub const DIGEST_ENTRIES_MAX: usize = 5;
/// Подсказок к запросу пользователя.
pub const PROMPT_MAX: usize = 3;
/// Строк словаря в подсказке к запросу пользователя (M4.5, спецификация §2.2 п. 2).
pub const TERM_HINTS_MAX: usize = 2;
/// `reason` подсказки термина словаря.
const REASON_GLOSSARY: &str = "glossary";
/// `reason` записи о файле пакета под задачу: ревизия неизвестна (индекс ссылок её не несёт),
/// поэтому показ в `session_seen` не отмечается — иначе он гасил бы подсказки `prompt`.
const REASON_FILE: &str = "file";
/// Подсказок к ошибке инструмента.
pub const POST_TOOL_MAX: usize = 3;
/// Байт ошибки, по которым сопоставляется `error_regex`.
pub const ERROR_SCAN_MAX: usize = 64 * 1024;
/// Кандидатов записей журнала одного вида с пространства (запас на уже показанные).
const ENTRY_FETCH: usize = 30;
/// Предел `budget_tokens` запроса (как у `/read`).
const BUDGET_MAX: usize = 200_000;
/// Кандидатов поиска `prompt` до фильтра видов, балла и показанного.
const PROMPT_SEARCH_TOP_K: usize = 20;
/// Бюджет карточек внутреннего поиска `prompt` (не текста подсказок), токенов.
const PROMPT_SEARCH_BUDGET: usize = 4000;
/// Предел фрагмента под подсказкой `prompt`, токенов.
const SNIPPET_TOKENS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    SessionStart,
    Prompt,
    PostTool,
    /// Пакет восстановления (M4.5 §9.2) для агентов без `SessionStart compact`.
    Recovery,
    /// Пакет под задачу (M4.5 §9.4): хук `subagent-start` и MCP.
    Brief,
}

impl Event {
    /// Тип подсказки в настройках пользователя `hooks.<тип>` (R5); `None` — без гейта.
    /// `recovery` и `brief` — явные вызовы (MCP, агенты без хуков; §0, §10): настройки
    /// подсказок хуков их не гасят. Хук `subagent-start` гейтится на клиенте, а пакет после
    /// `compact | resume` — ключами `session_start` и `recovery` события `session_start`.
    fn hook_kind(self) -> Option<&'static str> {
        match self {
            Event::SessionStart => Some("session_start"),
            Event::Prompt => Some("prompt"),
            Event::PostTool => Some("post_tool"),
            Event::Recovery | Event::Brief => None,
        }
    }

    /// Источник показа в `usage`: у пакета восстановления — `session_start` (так с задачи 9),
    /// у пакета под задачу — `brief` (в `usage.source` нет CHECK, миграция не нужна).
    fn source(self) -> UsageSource {
        match self {
            Event::SessionStart | Event::Recovery => UsageSource::SessionStart,
            Event::Prompt => UsageSource::Prompt,
            Event::PostTool => UsageSource::PostTool,
            Event::Brief => UsageSource::Brief,
        }
    }
}

/// Событие учёта выданных токенов (`session_tokens`, M4.5 §9.5): пакет восстановления — и по
/// `event: recovery`, и по `session_start` после `compact | resume`.
fn token_event(event: Event, recovery: bool) -> &'static str {
    match event {
        _ if recovery => "recovery",
        Event::SessionStart | Event::Recovery => "session_start",
        Event::Prompt => "prompt",
        Event::PostTool => "post_tool",
        Event::Brief => "brief",
    }
}

/// Предел `task` пакета под задачу, символов (§9.4): длиннее — обрезка.
pub const BRIEF_TASK_MAX_CHARS: usize = 2000;
/// Предупреждение: `task` длиннее [`BRIEF_TASK_MAX_CHARS`] и обрезан.
pub const WARN_TASK_TRUNCATED: &str = "task truncated";
/// Решений, ловушек, файлов и задач в пакете под задачу — каждого.
pub const BRIEF_ENTRIES_MAX: usize = 3;
/// Терминов в пакете под задачу.
pub const BRIEF_TERMS_MAX: usize = 5;
/// Кандидатов поиска пакета под задачу до отбора видов и балла.
const BRIEF_SEARCH_TOP_K: usize = 30;
/// Путей из `task`, проверяемых по индексу ссылок.
const BRIEF_PATHS_MAX: usize = 32;
/// Заголовки разделов (общие для дайджеста и пакетов).
const SECTION_RULES: &str = "### Правила";
const SECTION_TASKS: &str = "### Задачи";
const SECTION_TERMS: &str = "### Термины";
const SECTION_TRAPS: &str = "### Ловушки";
const SECTION_FILES: &str = "### Файлы";

/// `source` хука `SessionStart`, после которого строится пакет восстановления.
const RECOVERY_SOURCES: [&str; 2] = ["compact", "resume"];
/// Давность решений (`decision`) в пакете восстановления, дней (§9.2).
pub const RECOVERY_DECISIONS_DAYS: i64 = 7;
/// Заголовок раздела решений пакета восстановления.
const SECTION_DECISIONS: &str = "### Решения";
/// `reason` подсказки-решения пакета восстановления.
const REASON_DECISION: &str = "decision";

/// Тело запроса. Неизвестные поля игнорируются: клиент хуков может быть новее сервера.
#[derive(Deserialize)]
pub struct RecallReq {
    event: Event,
    /// Источник `SessionStart` хука (`startup | resume | clear | compact | fork`): `compact` и
    /// `resume` — пакет восстановления (M4.5 §9.2), прочие — обычный дайджест.
    source: Option<String>,
    session_id: Option<String>,
    solution: Option<String>,
    area: Option<String>,
    prompt: Option<String>,
    error: Option<String>,
    tool: Option<String>,
    budget_tokens: Option<usize>,
    agent_block_version: Option<u32>,
    /// Учитывать показ (`session_seen`, `usage`); `false` — хук `session-start` с выключенным в
    /// клиенте дайджестом зовёт `/recall` только ради `hooks` (задача 10, fix round 2).
    record: Option<bool>,
    /// Задача пакета `brief` (§9.4); нет или пусто — мини-дайджест.
    task: Option<String>,
    /// Сессия родителя (`brief` субагента): её захваченные задачи — первыми в мини-дайджесте.
    parent_id: Option<String>,
}

/// Кандидат в подсказки.
struct Cand {
    /// Индекс цели (базы) в области запроса.
    target: usize,
    tref: TargetRef,
    rev: i64,
    /// Ссылка `i:|n:|c:<id>`.
    reference: String,
    kind: &'static str,
    title: String,
    certainty: Option<String>,
    reason: &'static str,
    /// Markdown подсказки (строка и, возможно, строки с отступом).
    block: String,
    /// Возраст записи в днях (M4.5 §7); `None` — не запись памяти или время неизвестно.
    age_days: Option<u32>,
}

/// Возраст подсказки (M4.5 §7): от `updated_at`, у заметки с `verified_at` — от более позднего.
fn age_of(updated_at: jiff::Timestamp, verified_at: Option<&str>) -> Option<u32> {
    Some(zam_store::time::age_days(
        jiff::Timestamp::now(),
        updated_at,
        verified_at,
    ))
}

/// Возраст элемента внутреннего поиска (`age_days` его JSON).
fn item_age(it: &Value) -> Option<u32> {
    it["age_days"]
        .as_u64()
        .map(|d| u32::try_from(d).unwrap_or(u32::MAX))
}

/// Срок сбора подсказок (`recall.timeout_ms`).
struct Deadline {
    at: Instant,
    expired: bool,
}

impl Deadline {
    /// Шаг сбора до срока: `None` — срок вышел (до шага или во время него).
    async fn step<T>(&mut self, f: impl std::future::Future<Output = T>) -> Option<T> {
        if self.expired || Instant::now() >= self.at {
            self.expired = true;
            return None;
        }
        match tokio::time::timeout_at(self.at, f).await {
            Ok(v) => Some(v),
            Err(_) => {
                self.expired = true;
                None
            }
        }
    }
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

fn certainty_str(c: Option<Certainty>) -> Option<String> {
    c.map(|c| c.as_str().to_string())
}

/// `data.kind` записи (вид инсайта или изменения), если задан строкой.
fn data_kind(e: &Entry) -> String {
    e.data
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn entry_ref(e: &Entry) -> String {
    format!("{}:{}", e.kind.ref_letter(), e.id)
}

/// Подсказка-запись журнала.
fn entry_cand(target: usize, e: &Entry, reason: &'static str, lead: &str) -> Cand {
    let kind = data_kind(e);
    let certainty = certainty_str(e.certainty);
    let reference = entry_ref(e);
    let block = hint_line(
        lead,
        &e.title,
        &[kind.as_str(), certainty.as_deref().unwrap_or_default()],
        &reference,
    );
    Cand {
        target,
        tref: TargetRef {
            kind: TargetKind::Entry,
            id: e.id,
        },
        rev: e.rev,
        reference,
        kind: e.kind.as_str(),
        title: e.title.clone(),
        certainty,
        reason,
        block,
        age_days: age_of(e.updated_at, None),
    }
}

/// Пространства цели для записей журналов (insight, changelog): решение и Knowledge.
fn entry_spaces(t: &SearchTarget) -> Vec<Id> {
    t.spaces
        .iter()
        .filter(|s| matches!(s.kind, SpaceKind::Knowledge | SpaceKind::Solution))
        .map(|s| s.id)
        .collect()
}

/// Пространство решения в области: `(цель, пространство)`.
fn solution_space(targets: &[SearchTarget]) -> Option<(usize, Id)> {
    targets.iter().enumerate().find_map(|(i, t)| {
        t.spaces
            .iter()
            .find(|s| s.kind == SpaceKind::Solution)
            .map(|s| (i, s.id))
    })
}

/// Пространство вида `kind` личной базы (цель без чужого решения).
fn personal_space(targets: &[SearchTarget], kind: SpaceKind) -> Option<(usize, Id)> {
    targets.iter().enumerate().find_map(|(i, t)| {
        if t.solution.is_some() {
            return None;
        }
        t.spaces.iter().find(|s| s.kind == kind).map(|s| (i, s.id))
    })
}

/// Записи журнала пространств `spaces` по фильтру `f` (без пространства) — новые первыми.
async fn entries_of(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
    f: impl Fn(Id) -> EntryFilter,
) -> Result<Vec<(usize, Entry)>, ApiError> {
    let mut out: Vec<(usize, Entry)> = Vec::new();
    for &(i, space) in spaces {
        if !health.alive[i] {
            continue;
        }
        let db = health.targets()[i].db.clone();
        let Some(r) = deadline.step(list_entries(&db, f(space))).await else {
            break;
        };
        if let Some((rows, _)) = health.soft(i, r)? {
            out.extend(rows.into_iter().map(|e| (i, e)));
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.1.created_at, e.1.id)));
    Ok(out)
}

/// Фильтр ленты записей вида `kind` пространства `space` (без статусов и `data.kind`).
fn entry_filter(space: Id, kind: EntryKind) -> EntryFilter {
    EntryFilter {
        space_id: space,
        kind: Some(kind),
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        decided_by: None,
        since: None,
        until: None,
        cursor: None,
        limit: ENTRY_FETCH,
    }
}

/// Предел `next_step` и `blocker` в строке задачи дайджеста, символов.
const TASK_LINE_TEXT_CHARS: usize = 200;
/// Давность ответа на вопрос для `### Ответы человека`, дней (M4.5 §4).
pub const ANSWERS_DAYS: i64 = 14;
/// Заголовков открытых вопросов в дайджесте.
pub const OPEN_QUESTIONS_TITLES: usize = 3;
/// `reason` подсказки с ответом на вопрос: показывается сессии один раз.
const REASON_ANSWER: &str = "answer";
/// Заголовок раздела дайджеста с ответами.
const SECTION_ANSWERS: &str = "### Ответы человека";
/// Заголовок раздела окружения дайджеста (Vault `host`/`environment`, M4.5 §8).
const SECTION_ENVIRONMENT: &str = "### Окружение";
/// Записей окружения в дайджесте.
pub const ENVIRONMENT_MAX: usize = 5;
/// `reason` строки окружения: не запись журнала — ни `session_seen`, ни `usage`.
const REASON_ENVIRONMENT: &str = "environment";
/// Заголовок раздела дайджеста с открытыми вопросами.
const SECTION_OPEN_QUESTIONS: &str = "### Открытые вопросы";

/// Обрезка текста до `max` символов с многоточием.
fn clip(s: &str, max: usize) -> String {
    let s = one_line(s);
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Строка задачи дайджеста: `ZeroAIMemory: [<статус>] <заголовок> — далее: <next_step> —
/// k:<id>`, у `blocked` после ссылки — ` — блокер: <blocked_by>`.
fn task_block(e: &Entry) -> String {
    let t = zam_store::TaskData::from_stored(&e.data);
    let status = e.status.map_or("open", EntryStatus::as_str);
    let mut s = format!("ZeroAIMemory: [{status}] {}", one_line(&e.title));
    if let Some(n) = &t.next_step {
        s.push_str(&format!(" — далее: {}", clip(n, TASK_LINE_TEXT_CHARS)));
    }
    s.push_str(&format!(" — {}", entry_ref(e)));
    if e.status == Some(EntryStatus::Blocked)
        && let Some(b) = &t.blocked_by
    {
        s.push_str(&format!(" — блокер: {}", clip(b, TASK_LINE_TEXT_CHARS)));
    }
    s
}

/// Задачи `in_progress` и `blocked` пространств `spaces`: захваченные сессией запроса —
/// первыми, затем недавно менявшиеся.
async fn task_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
    session: Option<&str>,
) -> Result<Vec<Cand>, ApiError> {
    let mut rows = entries_of(health, deadline, spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::InProgress, EntryStatus::Blocked],
        ..entry_filter(s, EntryKind::Task)
    })
    .await?;
    let now = jiff::Timestamp::now();
    let mine = |e: &Entry| {
        session.is_some_and(|sid| {
            zam_store::TaskData::from_stored(&e.data)
                .claimed_by
                .is_some_and(|c| c.session == sid && c.live(now))
        })
    };
    rows.sort_by_key(|(_, e)| (!mine(e), std::cmp::Reverse((e.updated_at, e.id))));
    Ok(rows
        .iter()
        .map(|(i, e)| {
            let mut c = entry_cand(*i, e, "task", "");
            c.block = task_block(e);
            c
        })
        .collect())
}

/// Строка ответа на вопрос: `ZeroAIMemory: ответ на «<вопрос>»: <ответ> — q:<id> → i:<решение>`.
fn answer_block(e: &Entry, qd: &zam_store::QuestionData, a: &zam_store::Answer) -> String {
    let q = zam_markdown::mask_text(&clip(&e.title, TASK_LINE_TEXT_CHARS));
    let ans = zam_markdown::mask_text(&clip(&a.brief(&qd.options), TASK_LINE_TEXT_CHARS));
    format!(
        "ZeroAIMemory: ответ на «{q}»: {ans} — {} → i:{}",
        entry_ref(e),
        a.decision_id
    )
}

/// Ответы человека за [`ANSWERS_DAYS`] дней (до сеанса показа — по `session_seen`) и строка
/// открытых вопросов `N открытых: «…», «…», «…»` (до [`OPEN_QUESTIONS_TITLES`]).
async fn question_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
) -> Result<(Vec<Cand>, Option<String>), ApiError> {
    let since = zam_store::days_ago(jiff::Timestamp::now(), ANSWERS_DAYS);
    let mut answered: Vec<(usize, Entry, zam_store::Answer)> = Vec::new();
    // заголовки открытых вопросов всех баз с временем создания: берутся новейшие по общей сортировке
    let (mut open_n, mut titles) = (0usize, Vec::<(jiff::Timestamp, String)>::new());
    for i in 0..health.targets().len() {
        let ids: Vec<Id> = spaces
            .iter()
            .filter(|(t, _)| *t == i)
            .map(|(_, s)| *s)
            .collect();
        if ids.is_empty() || !health.alive[i] {
            continue;
        }
        let db = health.targets()[i].db.clone();
        let Some(r) = deadline
            .step(zam_store::recent_answered(&db, &ids, since, ENTRY_FETCH))
            .await
        else {
            break;
        };
        if let Some(rows) = health.soft(i, r)? {
            for e in rows {
                if let Some(a) = zam_store::QuestionData::from_stored(&e.data).answer {
                    answered.push((i, e, a));
                }
            }
        }
        let Some(r) = deadline
            .step(zam_store::open_questions(&db, &ids, OPEN_QUESTIONS_TITLES))
            .await
        else {
            break;
        };
        if let Some((n, t)) = health.soft(i, r)? {
            open_n += n;
            titles.extend(t);
        }
    }
    answered.sort_by_key(|(_, e, a)| std::cmp::Reverse((a.answered_at, e.id)));
    let cands = answered
        .iter()
        .map(|(i, e, a)| {
            let qd = zam_store::QuestionData::from_stored(&e.data);
            let mut c = entry_cand(*i, e, REASON_ANSWER, "");
            c.block = answer_block(e, &qd, a);
            c
        })
        .collect();
    titles.sort_by(|a, b| b.cmp(a));
    let line = (open_n > 0).then(|| {
        let list: Vec<String> = titles
            .iter()
            .take(OPEN_QUESTIONS_TITLES)
            .map(|(_, t)| {
                format!(
                    "«{}»",
                    zam_markdown::mask_text(&clip(t, TASK_LINE_TEXT_CHARS))
                )
            })
            .collect();
        format!("{open_n} открытых: {}", list.join(", "))
    });
    Ok((cands, line))
}

/// Строка окружения: `ZeroAIMemory: <имя> — <purpose>; <address>; ограничения: <limits> —
/// v:<id>`; пустые поля опускаются. Берёт только открытые поля: секрета и шифротекста здесь нет.
fn environment_block(m: &zam_store::VaultMeta) -> String {
    let field = |k: &str| {
        m.data
            .get(k)
            .and_then(Value::as_str)
            .map(|v| zam_markdown::mask_text(&clip(v, TASK_LINE_TEXT_CHARS)))
            .filter(|v| !v.is_empty())
    };
    let mut parts: Vec<String> = Vec::new();
    parts.extend(field("purpose"));
    parts.extend(field("address"));
    parts.extend(field("limits").map(|l| format!("ограничения: {l}")));
    let mut s = format!("ZeroAIMemory: {}", one_line(&m.title));
    if !parts.is_empty() {
        s.push_str(&format!(" — {}", parts.join("; ")));
    }
    s.push_str(&format!(" — v:{}", m.id));
    s
}

/// Записи `host`/`environment` Vault пользователя запроса: личного и решения запроса (Vault
/// участника лежит в его базе, чужой не виден). Читает метаданные без мастер-ключа, так что
/// заблокированный Vault раздел не прячет. Строки — не подсказки журнала: учёта показа нет.
async fn environment_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    req: &RecallReq,
    headers: &HeaderMap,
) -> Result<Vec<Cand>, ApiError> {
    // база пользователя запроса — цель без чужого решения
    let Some(i) = health.targets().iter().position(|t| t.solution.is_none()) else {
        return Ok(Vec::new());
    };
    if !health.alive[i] {
        return Ok(Vec::new());
    }
    let guid = super::files::query_solution(req.solution.as_deref(), headers)
        .ok()
        .flatten();
    let db = health.targets()[i].db.clone();
    let Some(r) = deadline
        .step(zam_store::vault_environment(
            &db,
            guid.as_ref(),
            ENVIRONMENT_MAX,
        ))
        .await
    else {
        return Ok(Vec::new());
    };
    let Some(items) = health.soft(i, r)? else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .map(|m| Cand {
            target: i,
            tref: TargetRef {
                kind: TargetKind::Entry,
                id: m.id,
            },
            rev: m.rev,
            reference: format!("v:{}", m.id),
            kind: "vault",
            title: m.title.clone(),
            certainty: None,
            reason: REASON_ENVIRONMENT,
            block: environment_block(m),
            age_days: None,
        })
        .collect())
}

/// Разделы дайджеста `session_start` (см. модуль); каждый — кандидаты по приоритету.
/// `recovery` — пакет восстановления: после ответов — раздел решений ([`decision_cands`]).
async fn digest(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    recovery: bool,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Digest, ApiError> {
    let targets = health.targets();
    let mut notices: Vec<String> = Vec::new();
    if let Some(v) = req.agent_block_version {
        // версия сервера — та же, что отдаёт `GET /agent-setup` (решение контроллера T11-a);
        // сбой чтения настроек подсказку не валит — сравнение с версией сборки
        let current = match crate::agent_setup::current_version(&st.system).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "версия блока инструкции не прочитана");
                AGENT_BLOCK_VERSION
            }
        };
        if v < current {
            notices.push(format!(
                "ZeroAIMemory: Блок ZeroAIMemory устарел (v{v} → v{current}): обнови \
                 через `zam init` или WebUI"
            ));
        }
    }
    let user = target_user_id(st, p, headers)?;
    if let Some(r) = deadline
        .step(st.system.pending_requests_for_owner(user))
        .await
    {
        let n = r?;
        if n > 0 {
            notices.push(format!(
                "ZeroAIMemory: запросы доступа к вашим решениям ждут ответа: {n} — WebUI \
                 «Администрирование → Решения» или `zam solution access requests`"
            ));
        }
    }
    let mut sections: Vec<(Option<&'static str>, Vec<Cand>)> = Vec::new();

    // задачи в работе — первыми после уведомлений (M4.5 §3): их ждёт агент, начавший сессию
    let sol = solution_space(targets);
    let mut insight_spaces: Vec<(usize, Id)> = sol.into_iter().collect();
    insight_spaces.extend(personal_space(targets, SpaceKind::Knowledge));
    let session = req.session_id.clone().or_else(|| {
        crate::attrib::header_value(headers, SESSION_HEADER)
            .ok()
            .flatten()
    });
    let tasks = task_cands(health, deadline, &insight_spaces, session.as_deref()).await?;
    sections.push((Some(SECTION_TASKS), tasks));
    // ответы человека и открытые вопросы — сразу после задач (M4.5 §4)
    let (answers, open_line) = question_cands(health, deadline, &insight_spaces).await?;
    sections.push((Some(SECTION_ANSWERS), answers));
    // окружение из Vault — после задач и ответов; в пакет восстановления не входит (§9.2: его
    // состав фиксирован, окружение агент находит `vault?q=`/`memory_vault`)
    if !recovery {
        let env = environment_cands(health, deadline, req, headers).await?;
        sections.push((Some(SECTION_ENVIRONMENT), env));
    }
    if recovery {
        let decisions = decision_cands(health, deadline, sol).await?;
        sections.push((Some(SECTION_DECISIONS), decisions));
    }

    // Rules — L1 каждой заметки
    let mut rules: Vec<Cand> = Vec::new();
    if let Some((i, space)) = personal_space(targets, SpaceKind::Rules) {
        let db = targets[i].db.clone();
        if let Some(r) = deadline.step(list_children(&db, space, None, 16)).await {
            let notes: Vec<zam_store::Node> = r?
                .into_iter()
                .filter(|n| n.kind == NodeKind::Note)
                .take(RULES_MAX)
                .collect();
            for n in notes {
                let Some(r) = deadline.step(node_content(&db, n.id, None)).await else {
                    break;
                };
                let (rev, text) = match r {
                    Ok(v) => v,
                    // узел удалён между чтениями — без него
                    Err(StoreError::NotFound) => continue,
                    Err(e) => return Err(e.into()),
                };
                // тестовая задержка (частичный ответ по тайм-ауту): только с фичей test-hooks
                #[cfg(feature = "test-hooks")]
                if text.contains(TEST_SLOW_RULE_MARK)
                    && deadline
                        .step(tokio::time::sleep(Duration::from_millis(TEST_SLOW_RULE_MS)))
                        .await
                        .is_none()
                {
                    break;
                }
                let certainty = certainty_str(n.certainty);
                let reference = format!("n:{}", n.id);
                let mut block = hint_line(
                    "правило ",
                    &n.path,
                    &[certainty.as_deref().unwrap_or_default()],
                    &reference,
                );
                // секреты в тексте правила маскируются, как в чанках и L0 (§7.6)
                let summary = zam_markdown::mask_text(&l1(&text));
                let summary = truncate_to_tokens(&summary, RULE_L1_TOKENS, &count_tokens);
                for line in summary.lines().filter(|l| !l.trim().is_empty()) {
                    block.push_str("\n  ");
                    block.push_str(line.trim_end());
                }
                rules.push(Cand {
                    target: i,
                    tref: TargetRef {
                        kind: TargetKind::Node,
                        id: n.id,
                    },
                    rev,
                    reference,
                    kind: "note",
                    title: n.path.clone(),
                    certainty,
                    reason: "rules",
                    block,
                    age_days: age_of(n.updated_at, n.verified_at.as_deref()),
                });
            }
        }
    }
    sections.push((Some(SECTION_RULES), rules));

    // L1 решения — корневые узлы пространства решения с L0
    let mut overview: Vec<Cand> = Vec::new();
    if let Some((i, space)) = sol
        && health.alive[i]
    {
        let db = targets[i].db.clone();
        if let Some(r) = deadline.step(list_children(&db, space, None, 1)).await
            && let Some(nodes) = health.soft(i, r)?
        {
            for n in nodes.into_iter().take(SOLUTION_ROOT_MAX) {
                let folder = n.kind == NodeKind::Folder;
                let mut title = n.path.clone();
                if folder {
                    title.push('/');
                }
                if let Some(l0) = n.l0.as_deref().map(one_line).filter(|s| !s.is_empty()) {
                    title.push_str(" — ");
                    title.push_str(&l0);
                }
                let certainty = certainty_str(n.certainty);
                let reference = format!("n:{}", n.id);
                let block = hint_line(
                    "",
                    &title,
                    &[certainty.as_deref().unwrap_or_default()],
                    &reference,
                );
                overview.push(Cand {
                    target: i,
                    tref: TargetRef {
                        kind: TargetKind::Node,
                        id: n.id,
                    },
                    rev: n.current_rev,
                    reference,
                    kind: if folder { "folder" } else { "note" },
                    title: n.path.clone(),
                    certainty,
                    reason: "solution",
                    block,
                    age_days: age_of(n.updated_at, n.verified_at.as_deref()),
                });
            }
        }
    }
    sections.push((Some("### Решение"), overview));

    // действующие gotcha/failure, revisit — решение и Knowledge; changelog — решение
    let traps = trap_cands(health, deadline, &insight_spaces).await?;
    sections.push((Some("### Ловушки и сбои"), traps));
    let revisit = entries_of(health, deadline, &insight_spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::Revisit],
        ..entry_filter(s, EntryKind::Insight)
    })
    .await?
    .iter()
    .map(|(i, e)| entry_cand(*i, e, "revisit", ""))
    .collect();
    sections.push((Some("### Пересмотреть (revisit)"), revisit));
    let changes = entries_of(
        health,
        deadline,
        &sol.into_iter().collect::<Vec<_>>(),
        |s| entry_filter(s, EntryKind::Changelog),
    )
    .await?
    .iter()
    .map(|(i, e)| entry_cand(*i, e, "changelog", ""))
    .collect();
    sections.push((Some("### Последние изменения"), changes));
    // строки после правил: словарь и проверенные команды решения (M4.5 §7)
    let mut after_rules: Vec<String> = glossary_line(st, health, deadline)
        .await
        .into_iter()
        .collect();
    after_rules.extend(verified_line(health, deadline, sol).await);
    Ok((notices, sections, after_rules, open_line))
}

/// Действующие `gotcha`/`failure` пространств `spaces`, новые первыми.
async fn trap_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
) -> Result<Vec<Cand>, ApiError> {
    let mut traps: Vec<(usize, Entry)> = Vec::new();
    for kind in ["gotcha", "failure"] {
        traps.extend(
            entries_of(health, deadline, spaces, |s| EntryFilter {
                statuses: vec![EntryStatus::Active],
                data_kind: Some(kind.to_string()),
                decided_by: None,
                ..entry_filter(s, EntryKind::Insight)
            })
            .await?,
        );
    }
    traps.sort_by_key(|e| std::cmp::Reverse((e.1.created_at, e.1.id)));
    Ok(traps
        .iter()
        .map(|(i, e)| {
            let reason = if data_kind(e) == "failure" {
                "failure"
            } else {
                "gotcha"
            };
            entry_cand(*i, e, reason, "")
        })
        .collect())
}

/// L0 заметок Rules (пакет под задачу): `ZeroAIMemory: правило <путь> — <L0> — n:<id>`.
async fn rules_l0(health: &mut Health<'_>, deadline: &mut Deadline) -> Result<Vec<Cand>, ApiError> {
    let targets = health.targets();
    let Some((i, space)) = personal_space(targets, SpaceKind::Rules) else {
        return Ok(Vec::new());
    };
    let db = targets[i].db.clone();
    let Some(r) = deadline.step(list_children(&db, space, None, 16)).await else {
        return Ok(Vec::new());
    };
    Ok(r?
        .into_iter()
        .filter(|n| n.kind == NodeKind::Note)
        .take(RULES_MAX)
        .map(|n| {
            let mut title = n.path.clone();
            if let Some(l0) = n.l0.as_deref().map(one_line).filter(|s| !s.is_empty()) {
                title.push_str(" — ");
                title.push_str(&zam_markdown::mask_text(&l0));
            }
            let certainty = certainty_str(n.certainty);
            let reference = format!("n:{}", n.id);
            let block = hint_line(
                "правило ",
                &title,
                &[certainty.as_deref().unwrap_or_default()],
                &reference,
            );
            Cand {
                target: i,
                tref: TargetRef {
                    kind: TargetKind::Node,
                    id: n.id,
                },
                rev: n.current_rev,
                reference,
                kind: "note",
                title: n.path.clone(),
                certainty,
                reason: "rules",
                block,
                age_days: age_of(n.updated_at, n.verified_at.as_deref()),
            }
        })
        .collect())
}

/// Разделы пакета под задачу `brief` (см. модуль); `task` нет — мини-дайджест.
async fn brief(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    task: Option<&str>,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<(Option<&'static str>, Vec<Cand>)>, ApiError> {
    let targets = health.targets();
    // участнику — только пространство решения (его цель другой базы иных пространств не несёт)
    let mut insight_spaces: Vec<(usize, Id)> = solution_space(targets).into_iter().collect();
    insight_spaces.extend(personal_space(targets, SpaceKind::Knowledge));
    let mut sections: Vec<(Option<&'static str>, Vec<Cand>)> = Vec::new();
    sections.push((Some(SECTION_RULES), rules_l0(health, deadline).await?));
    let Some(task) = task else {
        let traps = trap_cands(health, deadline, &insight_spaces).await?;
        sections.push((Some(SECTION_TRAPS), traps));
        let session = req
            .parent_id
            .clone()
            .or_else(|| req.session_id.clone())
            .or_else(|| {
                crate::attrib::header_value(headers, SESSION_HEADER)
                    .ok()
                    .flatten()
            });
        let tasks = task_cands(health, deadline, &insight_spaces, session.as_deref()).await?;
        sections.push((Some(SECTION_TASKS), tasks));
        return Ok(sections);
    };
    let (decisions, traps) = brief_search(st, p, headers, req, task, health, deadline).await?;
    sections.push((Some(SECTION_DECISIONS), decisions));
    let terms = term_cands(st, task, BRIEF_TASK_MAX_CHARS, health, deadline).await;
    sections.push((Some(SECTION_TERMS), terms));
    sections.push((Some(SECTION_TRAPS), traps));
    let files = file_cands(req, headers, task, health, deadline).await;
    sections.push((Some(SECTION_FILES), files));
    Ok(sections)
}

/// Поиск пакета под задачу: действующие insight по `task` (`mode=brief`, балл ≥
/// `recall.prompt_min_score`) → `(решения axiom/theorem, ловушки gotcha/failure)`.
async fn brief_search(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    task: &str,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<(Vec<Cand>, Vec<Cand>), ApiError> {
    let query: String = task
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect();
    let (mut decisions, mut traps) = (Vec::new(), Vec::new());
    if query.trim().is_empty() {
        return Ok((decisions, traps));
    }
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": true, "include_rules": false},
        "filters": {"kinds": ["insight"], "status": ["active"]},
        "mode": "brief",
        "top_k": BRIEF_SEARCH_TOP_K,
        "budget_tokens": PROMPT_SEARCH_BUDGET,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let area = req.area.as_deref();
    let Some(v) = inner_search(st, p, headers, area, body, health, deadline).await? else {
        return Ok((decisions, traps));
    };
    let min = st.cfg.recall.prompt_min_score;
    let targets = health.targets();
    for it in v["items"].as_array().into_iter().flatten() {
        if it["score"].as_f64().unwrap_or(0.0) < min || it["kind"] != "insight" {
            continue;
        }
        let certainty = it["certainty"].as_str().map(str::to_string);
        let reason: &'static str = match it["insight_kind"].as_str().unwrap_or_default() {
            "decision" if matches!(certainty.as_deref(), Some("axiom" | "theorem")) => {
                REASON_DECISION
            }
            "gotcha" => "gotcha",
            "failure" => "failure",
            _ => continue,
        };
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
        let title = it["title"].as_str().unwrap_or_default().to_string();
        let reference = format!("i:{id}");
        let ik = it["insight_kind"].as_str().unwrap_or_default();
        let block = hint_line(
            "",
            &title,
            &[ik, certainty.as_deref().unwrap_or_default()],
            &reference,
        );
        let c = Cand {
            target,
            tref: TargetRef {
                kind: TargetKind::Entry,
                id,
            },
            rev: it["rev"].as_i64().unwrap_or(0),
            reference,
            kind: "insight",
            title,
            certainty,
            reason,
            block,
            age_days: item_age(it),
        };
        if reason == REASON_DECISION {
            decisions.push(c);
        } else {
            traps.push(c);
        }
    }
    Ok((decisions, traps))
}

/// Пути файлов, упомянутые в `task`: слова с `/`, `\` или `.`, без кавычек, скобок,
/// знаков препинания по краям и суффикса `:<строка>[:<столбец>]`, нормализованные
/// ([`zam_store::file_refs::normalize_ref_path`]); до [`BRIEF_PATHS_MAX`], без повторов.
fn task_paths(task: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let words = task.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | ';'
                    | '«'
                    | '»'
            )
    });
    for w in words {
        let mut w = w.trim_end_matches(['.', ':', '!', '?']);
        for _ in 0..2 {
            if let Some((a, b)) = w.rsplit_once(':')
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
            {
                w = a;
            }
        }
        if !w.contains(['/', '\\', '.']) {
            continue;
        }
        let Ok(p) = zam_store::file_refs::normalize_ref_path(&w.replace('\\', "/")) else {
            continue;
        };
        if !out.contains(&p) {
            out.push(p);
        }
        if out.len() >= BRIEF_PATHS_MAX {
            break;
        }
    }
    out
}

/// Записи со ссылками на файлы из `task` (индекс `file_refs` решения запроса): путь совпадает
/// точно или оканчивается на `/<путь из task>`. Без решения — пусто; сбой чтения — WARN.
async fn file_cands(
    req: &RecallReq,
    headers: &HeaderMap,
    task: &str,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    // совпадения по хвосту — после точных всех баз
    let mut tail_out: Vec<Cand> = Vec::new();
    let Ok(Some(guid)) = super::files::query_solution(req.solution.as_deref(), headers) else {
        return out;
    };
    let paths = task_paths(task);
    if paths.is_empty() {
        return out;
    }
    for i in 0..health.targets().len() {
        if !health.alive[i] {
            continue;
        }
        let (db, spaces) = {
            let t = &health.targets()[i];
            (t.db.clone(), entry_spaces(t))
        };
        if spaces.is_empty() {
            continue;
        }
        let Some(r) = deadline
            .step(zam_store::file_refs::refs_index(
                &db,
                guid.as_str(),
                &spaces,
            ))
            .await
        else {
            break;
        };
        let index = match r {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "recall: индекс ссылок на файлы не прочитан");
                continue;
            }
        };
        let (exact, tail) = matched_paths(&index, &paths);
        for (pr, is_exact) in exact
            .into_iter()
            .map(|p| (p, true))
            .chain(tail.into_iter().map(|p| (p, false)))
        {
            {
                for it in &pr.items {
                    let Some((letter, raw)) = it.id.split_once(':') else {
                        continue;
                    };
                    let Ok(id) = raw.parse::<Id>() else {
                        continue;
                    };
                    let (tkind, kind) = match letter {
                        "n" => (TargetKind::Node, "note"),
                        _ => (
                            TargetKind::Entry,
                            EntryKind::parse(&it.kind).map_or("entry", EntryKind::as_str),
                        ),
                    };
                    let stale = if it.state == zam_store::RefState::Stale {
                        "возможно устарело"
                    } else {
                        ""
                    };
                    let block = hint_line(
                        &format!("файл {}: ", pr.path),
                        &it.title,
                        &[kind, stale],
                        &it.id,
                    );
                    let c = Cand {
                        target: i,
                        tref: TargetRef { kind: tkind, id },
                        rev: 0,
                        reference: it.id.clone(),
                        kind,
                        title: it.title.clone(),
                        certainty: None,
                        reason: REASON_FILE,
                        block,
                        age_days: None,
                    };
                    if is_exact {
                        out.push(c);
                    } else {
                        tail_out.push(c);
                    }
                }
            }
        }
    }
    out.extend(tail_out);
    out
}

/// Пути индекса, упомянутые в `task`: `(точные совпадения, совпадения по хвосту /<путь>)` —
/// каждая группа в порядке путей `paths`, путь индекса — не больше одного раза.
fn matched_paths<'a>(
    index: &'a [zam_store::PathRefs],
    paths: &[String],
) -> (Vec<&'a zam_store::PathRefs>, Vec<&'a zam_store::PathRefs>) {
    let mut exact: Vec<&zam_store::PathRefs> = Vec::new();
    for want in paths {
        exact.extend(index.iter().filter(|pr| pr.path == *want));
    }
    let mut tail: Vec<&zam_store::PathRefs> = Vec::new();
    for want in paths {
        let suffix = format!("/{want}");
        for pr in index.iter().filter(|pr| pr.path.ends_with(&suffix)) {
            if !exact.iter().chain(&tail).any(|p| std::ptr::eq(*p, pr)) {
                tail.push(pr);
            }
        }
    }
    exact.dedup_by(|a, b| std::ptr::eq(*a, *b));
    (exact, tail)
}

/// Действующие решения (`decision`) пространства решения за [`RECOVERY_DECISIONS_DAYS`] дней,
/// новые первыми (пакет восстановления, §9.2). Без решения в области — пусто.
async fn decision_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    sol: Option<(usize, Id)>,
) -> Result<Vec<Cand>, ApiError> {
    let since = zam_store::days_ago(jiff::Timestamp::now(), RECOVERY_DECISIONS_DAYS);
    let spaces: Vec<(usize, Id)> = sol.into_iter().collect();
    Ok(entries_of(health, deadline, &spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::Active],
        data_kind: Some("decision".to_string()),
        since: Some(since),
        ..entry_filter(s, EntryKind::Insight)
    })
    .await?
    .iter()
    .map(|(i, e)| entry_cand(*i, e, REASON_DECISION, ""))
    .collect())
}

/// Дайджест `session_start`: уведомления, разделы-кандидаты, строка словаря и строка открытых
/// вопросов.
type Digest = (
    Vec<String>,
    Vec<(Option<&'static str>, Vec<Cand>)>,
    Vec<String>,
    Option<String>,
);

/// Проверенных команд в строке дайджеста (M4.5 §7).
pub const VERIFIED_COMMANDS_MAX: usize = 3;

/// Строка `Проверенные команды: <path> (проверено <verified_at>)[; …]` — до
/// [`VERIFIED_COMMANDS_MAX`] заметок пространства решения с `verified_at`, новые проверки первыми
/// (решение владельца 2026-10-28: проверенные команды — заметки с `verified_at`); `None` — таких
/// нет, решения нет или срок вышел. Сбой чтения базы чужого решения исключает его (как всюду).
async fn verified_line(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    sol: Option<(usize, Id)>,
) -> Option<String> {
    let (i, space) = sol?;
    if !health.alive[i] {
        return None;
    }
    let db = health.targets()[i].db.clone();
    let r = deadline
        .step(zam_store::verified_notes(&db, space, VERIFIED_COMMANDS_MAX))
        .await?;
    let notes = match health.soft(i, r) {
        Ok(Some(n)) => n,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "recall: проверенные команды не прочитаны");
            return None;
        }
    };
    let parts: Vec<String> = notes
        .iter()
        .map(|n| {
            format!(
                "{} (проверено {})",
                one_line(&n.path),
                one_line(n.verified_at.trim())
            )
        })
        .collect();
    (!parts.is_empty()).then(|| format!("Проверенные команды: {}", parts.join("; ")))
}

/// Строка `Словарь: N терминов — memory_search kinds=[term]` (N — действующие термины решения
/// и Knowledge области); `None` — терминов нет или срок вышел.
async fn glossary_line(
    st: &AppState,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Option<String> {
    let mut n = 0;
    for i in 0..health.targets().len() {
        if !health.alive[i] {
            continue;
        }
        let (db, spaces) = {
            let t = &health.targets()[i];
            (t.db.clone(), entry_spaces(t))
        };
        if spaces.is_empty() {
            continue;
        }
        match deadline.step(st.glossary.index(&db)).await? {
            Ok(idx) => n += idx.count_in(&spaces),
            Err(e) => tracing::warn!(error = %e, "recall: словарь не прочитан"),
        }
    }
    (n > 0).then(|| format!("Словарь: {n} терминов — memory_search kinds=[term]"))
}

/// Строки словаря к тексту (`prompt` — первые 512 символов, `task` пакета — 2000): термины
/// решения и Knowledge, чьё имя или псевдоним встречается в первых `chars` символах `prompt`
/// (см. модуль). Сбой чтения словаря — без подсказок.
async fn term_cands(
    st: &AppState,
    prompt: &str,
    chars: usize,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    if prompt.trim().is_empty() {
        return out;
    }
    for i in 0..health.targets().len() {
        if !health.alive[i] {
            continue;
        }
        let (db, spaces) = {
            let t = &health.targets()[i];
            (t.db.clone(), entry_spaces(t))
        };
        if spaces.is_empty() {
            continue;
        }
        let Some(r) = deadline.step(st.glossary.index(&db)).await else {
            break;
        };
        let idx = match r {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(error = %e, "recall: словарь не прочитан, без подсказок терминов");
                continue;
            }
        };
        for t in idx.find_in_head(prompt, &spaces, chars) {
            if out.iter().any(|c| c.tref.id == t.id) {
                continue;
            }
            let reference = format!("t:{}", t.id);
            let def = zam_markdown::mask_text(&one_line(&t.definition));
            let block = hint_line(
                "термин ",
                &format!("«{}» — {def}", t.title),
                &[],
                &reference,
            );
            out.push(Cand {
                target: i,
                tref: TargetRef {
                    kind: TargetKind::Entry,
                    id: t.id,
                },
                rev: t.rev,
                reference,
                kind: "term",
                title: t.title.clone(),
                certainty: None,
                reason: REASON_GLOSSARY,
                block,
                age_days: None,
            });
        }
    }
    out
}

/// `prompt`: внутренний `POST /search` (см. модуль) и отбор insight и заметок Rules.
async fn prompt_hints(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
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
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": true, "include_rules": true},
        "filters": {"kinds": ["insight", "note"]},
        "mode": "brief",
        "top_k": PROMPT_SEARCH_TOP_K,
        "budget_tokens": PROMPT_SEARCH_BUDGET,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let area = req.area.as_deref();
    let Some(v) = inner_search(st, p, headers, area, body, health, deadline).await? else {
        return Ok(Vec::new());
    };
    let min = st.cfg.recall.prompt_min_score;
    let targets = health.targets();
    let mut out: Vec<Cand> = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        if it["score"].as_f64().unwrap_or(0.0) < min {
            continue;
        }
        let space_kind = it["space"]["kind"].as_str().unwrap_or_default();
        let kind = it["kind"].as_str().unwrap_or_default();
        let (tref, raw_id) = match (kind, it["entry_id"].as_str(), it["node_id"].as_str()) {
            ("insight", Some(id), _) => (TargetKind::Entry, id),
            ("note", _, Some(id)) if space_kind == "rules" => (TargetKind::Node, id),
            _ => continue,
        };
        let Ok(id) = raw_id.parse::<Id>() else {
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
        let certainty = it["certainty"].as_str().map(str::to_string);
        let snippet = one_line(
            &it["snippet"]
                .as_str()
                .unwrap_or_default()
                .replace([zam_store::MARK_START, zam_store::MARK_END], ""),
        );
        let snippet = truncate_to_tokens(&snippet, SNIPPET_TOKENS, &count_tokens).to_string();
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
            tref: TargetRef { kind: tref, id },
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

/// Внутренний `POST /search` подсказок (тело `body`): без сессии (показанное учитывает сам
/// `/recall`), `area` — в `X-Zam-Area`, если его нет. Срок вышел — `None`; сбой поиска —
/// `None` и `warnings: ["recall search failed"]`; предупреждения поиска переходят в ответ.
async fn inner_search(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    area: Option<&str>,
    body: Value,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Option<Value>, ApiError> {
    let mut h = headers.clone();
    h.remove(SESSION_HEADER);
    if let Some(a) = zam_store::normalize_area(area)
        && !h.contains_key(AREA_HEADER)
    {
        h.insert(
            AREA_HEADER,
            HeaderValue::from_str(&a)
                .map_err(|_| ApiError::BadRequest("area: печатные символы".into()))?,
        );
    }
    let search = super::search::search(
        State(st.clone()),
        p.clone(),
        h,
        Ok(Bytes::from(body.to_string())),
    );
    let Some(r) = deadline.step(search).await else {
        return Ok(None);
    };
    // сбой поиска подсказки не валит: тишина и предупреждение
    let failed = |health: &mut Health<'_>, what: &dyn std::fmt::Display| {
        tracing::warn!(error = %what, "recall: поиск подсказок не удался");
        health.warnings.push(WARN_PROMPT_SEARCH_FAILED.into());
        Ok(None)
    };
    let resp = match r {
        Ok(resp) => resp,
        Err(e) => return failed(health, &e),
    };
    let status = resp.status();
    if status != StatusCode::OK {
        return failed(health, &status);
    }
    let bytes = match axum::body::to_bytes(resp.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => return failed(health, &e),
    };
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return failed(health, &e),
    };
    if let Some(ws) = v["warnings"].as_array() {
        health
            .warnings
            .extend(ws.iter().filter_map(Value::as_str).map(str::to_string));
    }
    Ok(Some(v))
}

/// `post_tool`: `error_regex`, затем FTS по insight (см. модуль).
async fn error_hints(
    req: &RecallReq,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
    let raw = req.error.as_deref().unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut cut = raw.len().min(ERROR_SCAN_MAX);
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    let scan = &raw[..cut];
    let targets = health.targets();
    let lead = "эта ошибка уже разбиралась: ";
    let mut out: Vec<Cand> = Vec::new();
    let mut taken: HashSet<TargetRef> = HashSet::new();

    // 1. error_regex действующих триггеров
    for (i, t) in targets.iter().enumerate() {
        let spaces = entry_spaces(t);
        if spaces.is_empty() || !health.alive[i] {
            continue;
        }
        let Some(r) = deadline.step(active_triggers(&t.db, &spaces)).await else {
            break;
        };
        let Some(rows) = health.soft(i, r)? else {
            continue;
        };
        for row in rows {
            if row.kind != TriggerKind::ErrorRegex
                || out.len() >= POST_TOOL_MAX
                || taken.contains(&TargetRef {
                    kind: TargetKind::Entry,
                    id: row.entry_id,
                })
            {
                continue;
            }
            if let (Some(tools), Some(tool)) = (&row.tools, req.tool.as_deref())
                && !tools.iter().any(|x| x == tool)
            {
                continue;
            }
            if !error_regex_matches(&row.pattern, scan) {
                continue;
            }
            // поля подсказки — из строки триггера (живой insight `active`/`revisit`); ревизия
            // отметке `recall` не нужна (повторы считаются без ревизии) — 0
            let tref = TargetRef {
                kind: TargetKind::Entry,
                id: row.entry_id,
            };
            taken.insert(tref);
            let reference = format!("i:{}", row.entry_id);
            let certainty = certainty_str(row.certainty);
            let block = hint_line(
                lead,
                &row.title,
                &[
                    row.insight_kind.as_str(),
                    certainty.as_deref().unwrap_or_default(),
                ],
                &reference,
            );
            out.push(Cand {
                target: i,
                tref,
                rev: 0,
                reference,
                kind: "insight",
                title: row.title.clone(),
                certainty,
                reason: "error_regex",
                block,
                age_days: None,
            });
        }
    }

    // 2. FTS по insight по нормализованной ошибке
    let normalized = normalize_error(raw);
    let terms = error_terms(&normalized);
    if out.len() < POST_TOOL_MAX && !terms.is_empty() {
        let mut hits: Vec<(f64, usize, zam_store::FtsHit, bool)> = Vec::new();
        for (i, t) in targets.iter().enumerate() {
            let spaces = entry_spaces(t);
            if spaces.is_empty() || !health.alive[i] {
                continue;
            }
            let q = FtsQuery {
                alias_groups: Vec::new(),
                text: normalized.clone(),
                space_ids: spaces,
                path_prefix: None,
                top_k: 100,
                targets: TargetFilter {
                    kinds: vec!["insight".into()],
                    statuses: vec!["active".into(), "revisit".into()],
                    area: None,
                    certainty: Vec::new(),
                    decided_by: None,
                    file: None,
                    insight_kinds: Vec::new(),
                },
            };
            let Some(r) = deadline.step(search_fts_channels(&t.db, q)).await else {
                break;
            };
            if let Some(ch) = health.soft(i, r)? {
                hits.extend(ch.bm25.into_iter().map(|h| (h.raw_score, i, h, ch.relaxed)));
            }
        }
        // bm25: меньше — лучше (слияние баз — по сырому баллу, как в поиске)
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, i, h, relaxed) in hits {
            if out.len() >= POST_TOOL_MAX {
                break;
            }
            let Some(e) = &h.entry else {
                continue;
            };
            if taken.contains(&h.target) {
                continue;
            }
            if relaxed
                && !coverage_enough(
                    term_coverage(&terms, &format!("{}\n{}", e.title, h.text)),
                    terms.len(),
                )
            {
                continue;
            }
            taken.insert(h.target);
            let reference = format!("i:{}", h.target.id);
            let kind = e.insight_kind.clone().unwrap_or_default();
            let block = hint_line(
                lead,
                &e.title,
                &[kind.as_str(), e.certainty.as_deref().unwrap_or_default()],
                &reference,
            );
            out.push(Cand {
                target: i,
                tref: h.target,
                rev: h.rev,
                reference,
                kind: "insight",
                title: e.title.clone(),
                certainty: e.certainty.clone(),
                reason: "error_fts",
                block,
                age_days: age_of(h.updated_at, h.verified_at.as_deref()),
            });
        }
    }
    Ok(out)
}

/// Ключ показа подсказки в `session_seen`.
fn seen_key(targets: &[SearchTarget], c: &Cand) -> SeenKey {
    SeenKey {
        owner_id: targets[c.target].owner,
        target: c.tref,
        part: RECALL_PART.to_string(),
    }
}

/// Уже показанные этой сессии подсказки (любая ревизия). Сбой чтения — WARN, без фильтра.
async fn shown(
    s: Option<&RequestSession>,
    targets: &[SearchTarget],
    cands: &[&Cand],
) -> HashSet<SeenKey> {
    let Some(s) = s else {
        return HashSet::new();
    };
    let keys: Vec<SeenKey> = cands.iter().map(|c| seen_key(targets, c)).collect();
    match seen_revs(&s.db, &s.touch.id, &keys).await {
        Ok(m) => m.into_keys().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "recall: показанное сессии не прочитано, без фильтра повторов");
            HashSet::new()
        }
    }
}

/// Учёт показа: `session_seen` (с касанием сессии) и выданные токены `session_tokens`
/// (M4.5 §9.5) одной транзакцией базы сессии, `usage` в базах целей. Сбой — WARN.
async fn record(
    s: Option<&RequestSession>,
    targets: &[SearchTarget],
    user: Id,
    event: Event,
    shown: &[&Cand],
    tokens: &[TokenCount],
) {
    if let Some(s) = s {
        let items: Vec<SeenItem> = shown
            .iter()
            .filter(|c| c.reason != REASON_FILE && c.reason != REASON_ENVIRONMENT)
            .map(|c| SeenItem {
                key: seen_key(targets, c),
                rev: c.rev,
            })
            .collect();
        let r = if items.is_empty() && tokens.is_empty() {
            touch_session(&s.db, &s.touch).await
        } else {
            mark_seen_with_tokens(&s.db, &s.touch, &items, tokens).await
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "recall: учёт сессии не выполнен");
        }
    }
    let mut by_target: HashMap<usize, Vec<UsageEvent>> = HashMap::new();
    for c in shown.iter().filter(|c| c.reason != REASON_ENVIRONMENT) {
        by_target.entry(c.target).or_default().push(UsageEvent {
            user_id: user,
            session_id: s.map(|s| s.touch.id.clone()),
            target: c.tref,
            event: UsageKind::RecallShown,
            source: Some(event.source()),
        });
    }
    for (i, events) in by_target {
        if let Err(e) = record_usages(&targets[i].db, &events).await {
            tracing::warn!(error = %e, "recall: показы не записаны в usage");
        }
    }
}

/// Предел подсказок раздела `title`: у пакета под задачу — свои (§9.4), у дайджеста — по
/// разделу, у разделов без заголовка — строки словаря или общий предел события `limit`.
fn section_limit(event: Event, title: Option<&str>, cands: &[Cand], limit: usize) -> usize {
    if event == Event::Brief {
        return match title {
            Some(SECTION_RULES) => RULES_MAX,
            Some(SECTION_TERMS) => BRIEF_TERMS_MAX,
            _ => BRIEF_ENTRIES_MAX,
        };
    }
    match title {
        Some(SECTION_RULES) => RULES_MAX,
        Some(SECTION_ENVIRONMENT) => ENVIRONMENT_MAX,
        Some("### Решение") => SOLUTION_ROOT_MAX,
        Some(_) => DIGEST_ENTRIES_MAX,
        None if cands.iter().any(|c| c.reason == REASON_GLOSSARY) => TERM_HINTS_MAX,
        None => limit,
    }
}

/// Раскладка отобранных разделов в разделы упаковки и подсказки каждого (у строк, что не
/// подсказки записей — уведомления, словарь, открытые вопросы, — пустой список: ни
/// `session_seen`, ни `usage`). Уведомления — первыми; в пакете восстановления — после задач,
/// ответов и решений (§9.2), а если других разделов нет — в конце. Открытые вопросы — после
/// ответов (в пакете — вместе с уведомлениями), строки словаря и проверенных команд — после
/// правил.
fn arrange<'c>(
    recovery: bool,
    notices: Vec<String>,
    open_line: Option<String>,
    after_rules: &[String],
    chosen: Vec<(Option<&'static str>, Vec<&'c Cand>)>,
) -> (Vec<Section>, Vec<Vec<&'c Cand>>) {
    let mut out = Arranged::default();
    let mut notices = Some(notices);
    let mut open_line = open_line;
    if !recovery {
        out.line(None, notices.take().unwrap_or_default());
    }
    for (title, cands) in chosen {
        if recovery
            && notices.is_some()
            && !matches!(
                title,
                Some(SECTION_TASKS | SECTION_ANSWERS | SECTION_DECISIONS)
            )
        {
            out.notices(&mut notices, &mut open_line);
        }
        out.packed.push(Section {
            title: title.map(str::to_string),
            blocks: cands.iter().map(|c| c.block.clone()).collect(),
        });
        out.picked.push(cands);
        if title == Some(SECTION_ANSWERS)
            && !recovery
            && let Some(line) = open_line.take()
        {
            out.line(Some(SECTION_OPEN_QUESTIONS), vec![line]);
        }
        if title == Some(SECTION_RULES) && !after_rules.is_empty() {
            out.line(None, after_rules.to_vec());
        }
    }
    // пакет восстановления без раздела после трёх первых: уведомления и вопросы не теряются
    out.notices(&mut notices, &mut open_line);
    (out.packed, out.picked)
}

/// Накопитель [`arrange`].
#[derive(Default)]
struct Arranged<'c> {
    packed: Vec<Section>,
    picked: Vec<Vec<&'c Cand>>,
}

impl Arranged<'_> {
    /// Раздел строк, что не подсказки записей.
    fn line(&mut self, title: Option<&str>, blocks: Vec<String>) {
        self.packed.push(Section {
            title: title.map(str::to_string),
            blocks,
        });
        self.picked.push(Vec::new());
    }

    /// Уведомления и строка открытых вопросов, если ещё не выведены.
    fn notices(&mut self, notices: &mut Option<Vec<String>>, open_line: &mut Option<String>) {
        if let Some(n) = notices.take() {
            self.line(None, n);
        }
        if let Some(l) = open_line.take() {
            self.line(Some(SECTION_OPEN_QUESTIONS), vec![l]);
        }
    }
}

/// `POST /recall`.
pub async fn recall(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let t0 = Instant::now();
    let req: RecallReq = json_body(body)?;
    if req
        .budget_tokens
        .is_some_and(|b| !(1..=BUDGET_MAX).contains(&b))
    {
        return Err(ApiError::BadRequest(format!(
            "budget_tokens: от 1 до {BUDGET_MAX}"
        )));
    }
    let area = zam_store::normalize_area(req.area.as_deref());
    if area.as_ref().is_some_and(|a| {
        a.chars().count() > zam_store::AREA_MAX_CHARS || a.chars().any(char::is_control)
    }) {
        return Err(ApiError::BadRequest(format!(
            "area: до {} печатных символов",
            zam_store::AREA_MAX_CHARS
        )));
    }
    if let Some(g) = &req.solution
        && g.trim().parse::<zam_core::SolutionGuid>().is_err()
    {
        return Err(ApiError::BadRequest("solution: GUID решения".into()));
    }
    // сессия: некорректный id — 400; сбой её базы подсказки не валит (без учёта повторов)
    let mut session = match request_session(&st, &p, &headers, req.session_id.as_deref()).await {
        Ok(s) => s,
        // некорректный id — 400, несуществующий пользователь (`X-Zam-User`) — 404
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "recall: база сессии недоступна, без учёта повторов");
            None
        }
    };
    if let Some(s) = &mut session {
        // известное из тела дописывается в сессию
        if let Some(g) = &req.solution {
            s.touch.solution_guid = g.trim().parse().ok();
        }
        if area.is_some() {
            s.touch.area = area.clone();
        }
    }
    let user = target_user_id(&st, &p, &headers)?;
    // R5: переключатели пользователя; сбой чтения — все включены (подсказки не валятся)
    let hooks = match hook_flags(&st.system, user).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "recall: настройки hooks пользователя не прочитаны");
            HookFlags::default()
        }
    };
    // N4: тип выключен на сервере — пустой `text`, показ не учитывается (сессия касается)
    let enabled = req.event.hook_kind().is_none_or(|k| hooks.enabled(k));
    // пакет восстановления (§9.2): `event: recovery` (без гейта) или `session_start` после
    // `compact` и `resume`; у хука выключенный `recovery` — обычный дайджест
    let recovery = req.event == Event::Recovery
        || (req.event == Event::SessionStart
            && hooks.enabled("recovery")
            && req
                .source
                .as_deref()
                .is_some_and(|s| RECOVERY_SOURCES.contains(&s.trim())));
    let brief_event = req.event == Event::Brief;
    let budget = req.budget_tokens.unwrap_or(if recovery {
        st.cfg.recall.recovery_tokens
    } else if brief_event {
        st.cfg.recall.brief_tokens
    } else {
        st.cfg.recall.digest_tokens
    });
    // задача пакета `brief`: первые 2000 символов, длиннее — обрезка с предупреждением
    let task: Option<String> = req
        .task
        .as_deref()
        .map(str::trim)
        .filter(|t| brief_event && !t.is_empty())
        .map(|t| t.chars().take(BRIEF_TASK_MAX_CHARS).collect());
    let task_truncated = brief_event
        && req
            .task
            .as_deref()
            .is_some_and(|t| t.trim().chars().count() > BRIEF_TASK_MAX_CHARS);
    let mut sh = headers.clone();
    if req.solution.is_some() {
        // решение тела сильнее заголовка
        sh.remove(SOLUTION_HEADER);
    }
    let scope = ScopeReq {
        solution: req.solution.clone(),
        include_knowledge: Some(true),
        include_rules: Some(true),
        ..ScopeReq::default()
    };
    let (targets, warnings) = resolve_targets(&st, &p, &sh, &scope).await?;
    let mut health = Health::new(&targets, warnings);
    if task_truncated {
        health.warnings.push(WARN_TASK_TRUNCATED.into());
    }
    if let Some(src) = req.source.as_deref() {
        tracing::debug!(event = ?req.event, source = %src, recovery, "recall");
    }
    // сбор — до срока минус резерв на учёт показа; учёт — только до полного срока
    let timeout = st.cfg.recall.timeout_ms;
    let total_deadline = t0 + Duration::from_millis(timeout);
    let mut deadline = Deadline {
        at: t0 + Duration::from_millis(timeout - ACCOUNTING_RESERVE_MS.min(timeout / 2)),
        expired: false,
    };

    // сбор кандидатов
    let (notices, sections, repeatable, after_rules, open_line) = match req.event {
        _ if !enabled => (Vec::new(), Vec::new(), true, Vec::new(), None),
        // дайджест повторяется: после `/compact` и `resume` Claude Code шлёт тот же
        // `session_id`, а контекст агента уже потерян (показ всё равно отмечается)
        Event::SessionStart | Event::Recovery => {
            let (n, s, g, o) =
                digest(&st, &p, &sh, &req, recovery, &mut health, &mut deadline).await?;
            (n, s, true, g, o)
        }
        Event::Prompt => {
            // строки словаря — перед подсказками insight; у каждого свой предел
            let prompt = req.prompt.as_deref().unwrap_or_default();
            let chars = crate::glossary::HINT_TEXT_CHARS;
            let terms = term_cands(&st, prompt, chars, &mut health, &mut deadline).await;
            let c = prompt_hints(&st, &p, &sh, &req, &mut health, &mut deadline).await?;
            (
                Vec::new(),
                vec![(None, terms), (None, c)],
                false,
                Vec::new(),
                None,
            )
        }
        Event::PostTool => {
            let c = error_hints(&req, &mut health, &mut deadline).await?;
            (Vec::new(), vec![(None, c)], true, Vec::new(), None)
        }
        // у субагента свой контекст: пакет повторяется (показ отмечается)
        Event::Brief => {
            let t = task.as_deref();
            let s = brief(&st, &p, &sh, &req, t, &mut health, &mut deadline).await?;
            (Vec::new(), s, true, Vec::new(), None)
        }
    };
    // без повторов: показанное сессии, повторы между разделами, исключённые цели; ответ на
    // вопрос показывается сессии один раз и в повторяемом дайджесте, а в пакете восстановления —
    // снова (контекст потерян, §9.2)
    let all: Vec<&Cand> = sections.iter().flat_map(|(_, c)| c).collect();
    let seen = if recovery || brief_event {
        HashSet::new()
    } else if repeatable {
        let once: Vec<&Cand> = all
            .iter()
            .copied()
            .filter(|c| c.reason == REASON_ANSWER)
            .collect();
        shown(session.as_ref(), &targets, &once).await
    } else {
        shown(session.as_ref(), &targets, &all).await
    };
    let limit = match req.event {
        Event::SessionStart | Event::Recovery | Event::Brief => usize::MAX,
        Event::Prompt => PROMPT_MAX,
        Event::PostTool => POST_TOOL_MAX,
    };
    let mut dup: HashSet<TargetRef> = HashSet::new();
    let chosen: Vec<(Option<&'static str>, Vec<&Cand>)> = sections
        .iter()
        .map(|(title, cands)| {
            let per = section_limit(req.event, *title, cands, limit);
            let chosen: Vec<&Cand> = cands
                .iter()
                .filter(|c| health.alive[c.target])
                .filter(|c| !seen.contains(&seen_key(&targets, c)))
                .filter(|c| dup.insert(c.tref))
                .take(per)
                .collect();
            (*title, chosen)
        })
        .collect();
    let (packed, picked) = arrange(recovery, notices, open_line, &after_rules, chosen);
    let (text, taken) = pack_sections(&packed, budget, &count_tokens);
    let shown_now: Vec<&Cand> = taken
        .iter()
        .filter_map(|&(si, bi)| picked[si].get(bi).copied())
        .collect();
    let collect_ms = ms(t0);

    // учёт показа — если полный срок ещё не вышел; иначе собранное отдаётся без учёта
    let rt = Instant::now();
    let record_ms = if req.record == Some(false) {
        None
    } else if rt < total_deadline {
        // выданное сессии по событию (§9.5): пустой ответ — не выдача
        let tokens: Vec<TokenCount> = (!text.is_empty())
            .then(|| TokenCount {
                event: token_event(req.event, recovery).into(),
                calls: 1,
                tokens: count_tokens(&text) as u64,
            })
            .into_iter()
            .collect();
        record(
            session.as_ref(),
            &targets,
            user,
            req.event,
            &shown_now,
            &tokens,
        )
        .await;
        Some(ms(rt))
    } else {
        deadline.expired = true;
        None
    };

    let mut warnings = std::mem::take(&mut health.warnings);
    if deadline.expired {
        warnings.push(WARN_RECALL_TIMEOUT.into());
    }
    dedup_keep_order(&mut warnings);
    let items: Vec<Value> = shown_now
        .iter()
        .map(|c| {
            let mut o = json!({
                "id": c.reference,
                "kind": c.kind,
                "title": c.title,
                "certainty": c.certainty,
                "reason": c.reason,
            });
            if let Some(d) = c.age_days {
                o["age_days"] = json!(d);
            }
            o
        })
        .collect();
    let mut out = json!({
        "text": text,
        "items": items,
        "tokens": count_tokens(&text),
        "timings": {
            "collect_ms": collect_ms,
            "record_ms": record_ms,
            "total_ms": ms(t0),
        },
        "warnings": warnings,
    });
    if req.event == Event::SessionStart {
        // R5: настройки типов подсказок пользователя (`hooks.<тип>`, задача 13)
        out["hooks"] = hooks.to_json();
    }
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(title: &str) -> Cand {
        Cand {
            target: 0,
            tref: TargetRef {
                kind: TargetKind::Entry,
                id: Id::new(),
            },
            rev: 1,
            reference: "k:x".into(),
            kind: "task",
            title: title.into(),
            certainty: None,
            reason: "task",
            block: format!("ZeroAIMemory: {title}"),
            age_days: None,
        }
    }

    /// Ревью задачи 9: в пакете восстановления без раздела после трёх первых уведомления и
    /// открытые вопросы выводятся в конце, а не теряются.
    #[test]
    fn recovery_notices_kept_without_later_section() {
        let t = cand("задача");
        let chosen = vec![
            (Some(SECTION_TASKS), vec![&t]),
            (Some(SECTION_ANSWERS), Vec::new()),
        ];
        let notices = vec!["ZeroAIMemory: уведомление".to_string()];
        let (packed, picked) = arrange(true, notices, Some("1 открытых: «в»".into()), &[], chosen);
        assert_eq!(packed.len(), picked.len());
        let titles: Vec<Option<&str>> = packed.iter().map(|s| s.title.as_deref()).collect();
        assert_eq!(
            titles,
            [
                Some(SECTION_TASKS),
                Some(SECTION_ANSWERS),
                None,
                Some(SECTION_OPEN_QUESTIONS)
            ]
        );
        assert_eq!(packed[2].blocks, ["ZeroAIMemory: уведомление"]);
        assert!(picked[2].is_empty() && picked[3].is_empty());
        // обычный дайджест: уведомления первыми, вопросы — после ответов
        let chosen = vec![
            (Some(SECTION_TASKS), vec![&t]),
            (Some(SECTION_ANSWERS), Vec::new()),
            (Some(SECTION_RULES), Vec::new()),
        ];
        let (packed, _) = arrange(
            false,
            vec!["n".into()],
            Some("q".into()),
            &[
                "Словарь".to_string(),
                "Проверенные команды: a (проверено 2026-10-28)".into(),
            ],
            chosen,
        );
        let blocks: Vec<&str> = packed
            .iter()
            .flat_map(|s| s.blocks.iter().map(String::as_str))
            .collect();
        assert_eq!(
            blocks,
            [
                "n",
                "ZeroAIMemory: задача",
                "q",
                "Словарь",
                "Проверенные команды: a (проверено 2026-10-28)"
            ]
        );
    }

    #[test]
    fn task_paths_extracted_and_normalized() {
        let p = task_paths(
            "Поправь `crates/zam-cli/src/main.rs:42:7`, затем (src\\lib.rs) и ./docs/a.md. \
             Не путь: слово, C:\\abs\\x.rs",
        );
        assert_eq!(p, ["crates/zam-cli/src/main.rs", "src/lib.rs", "docs/a.md"]);
    }

    /// Ревью задачи 10 (M-1): точные совпадения пути — раньше совпадений по хвосту.
    #[test]
    fn matched_paths_exact_first() {
        let pr = |p: &str| zam_store::PathRefs {
            path: p.into(),
            hash: None,
            items: Vec::new(),
        };
        // индекс — по алфавиту: хвостовые совпадения «mod.rs» идут раньше точного
        let index = vec![pr("a/mod.rs"), pr("b/mod.rs"), pr("mod.rs"), pr("x/y.rs")];
        let paths = vec!["mod.rs".to_string(), "x/y.rs".to_string()];
        let (exact, tail) = matched_paths(&index, &paths);
        let names = |v: &[&zam_store::PathRefs]| -> Vec<String> {
            v.iter().map(|p| p.path.clone()).collect()
        };
        assert_eq!(names(&exact), ["mod.rs", "x/y.rs"]);
        assert_eq!(names(&tail), ["a/mod.rs", "b/mod.rs"]);
    }
}
