//! Проактивное вспоминание `POST /recall` (§10.3, §12; задача 8 M4) — подсказки хукам агента
//! (задача 10) и инструменту MCP `memory_recall` (задача 9).
//!
//! ```text
//! POST /recall {event, session_id?, solution?, area?, prompt?, error?, tool?, budget_tokens?,
//!               agent_block_version?, task?, parent_id?}
//!   → 200 {text, items: [{id, kind, title, certainty, reason, age_days?}], tokens,
//!          rules_tokens, timings, warnings, hooks?}
//! ```
//!
//! - `text` — готовый Markdown для `additionalContext`; пусто — тишина. Каждая подсказка — строка
//!   `ZeroAIMemory: <заголовок> [<вид>·<cert>] i:<id>` (M6 §4.3; у заметки — `n:<id>`, у
//!   changelog — `c:<id>`), под ней с отступом может идти пояснение (L1 правила, фрагмент).
//!   `items` — показанные подсказки по порядку: `id` — та же ссылка, `kind` — `insight`,
//!   `changelog`, `note` или `folder`, `reason` — откуда подсказка (`rules`, `solution`,
//!   `gotcha`, `failure`, `revisit`, `changelog`, `search`, `error_regex`, `error_fts`),
//!   `age_days` — возраст записи в сутках (M4.5 §7; нет у строк окружения, файлов, терминов и
//!   триггеров `error_regex`: время там неизвестно); `tokens` — размер `text` (счётчик
//!   выдачи), `rules_tokens` — размер раздела-резерва Rules (M6 §8, §17 п. 3; 0 — резерва нет),
//!   `timings {collect_ms, record_ms, total_ms}`;
//! - **учёт выданного** (M4.5 §9.5): непустой `text` прибавляется к `session_tokens` сессии —
//!   событие `session_start`, `recovery` (и `session_start` после `compact | resume`),
//!   `prompt`, `post_tool` или `brief`, `calls + 1`, `tokens + tokens` ответа — в той же
//!   транзакции, что `session_seen` (без лишней синхронизации диска); резерв Rules — отдельной
//!   строкой `rules` (M6 §8, §17 п. 3: `calls + 1`, `tokens + rules_tokens`), событие — без него;
//!   вызов из MCP считает инструмент (`mcp`); `record: false`, нет сессии или вышел срок учёта —
//!   без прибавки;
//! - **`session_start`** — дайджест: Rules — **резерв сверх бюджета** (M6 §8: все заметки до
//!   [`RULES_MAX`], текст каждой до [`RULE_L1_TOKENS`], длиннее — с «…», не вытесняются),
//!   остальное — не больше `budget_tokens` (по умолчанию `recall.digest_tokens` = 1500) по
//!   приоритету — порядку разделов (уведомления, задачи, ответы и идеи — первыми, их не
//!   вытесняют индексы; проверка — `budget_cap`, M6 §4.2); детерминирован (M6 §8: порядок не зависит от `usage` и показов
//!   других сессий, окна — от начала суток UTC, в тексте нет возраста): напоминание об
//!   устаревшем блоке инструкции
//!   (`agent_block_version` меньше версии сервера [`crate::agent_setup::current_version`] —
//!   `max(AGENT_BLOCK_VERSION, версия шаблонов)`), ожидающие запросы доступа к решениям
//!   пользователя (`access_requests_pending`), `### Задачи` (M4.5 §3: задачи `in_progress` и
//!   `blocked` решения и Knowledge и `open` с живым захватом `claim` (задача 8a), до
//!   `recall.digest_entries` (5), захваченные сессией запроса — первыми, затем `in_progress` и
//!   `blocked`, затем `open`, захваченные другими; внутри — недавно менявшиеся; строка
//!   `[<статус>] <заголовок> — далее: … — k:<id>`, у `open` — `[open · захвачена этой сессией]`
//!   или `[open · захвачена другой сессией]`, у `blocked` — ещё ` — блокер: …`;
//!   `reason: task`), `### Ответы человека` (M4.5 §4: вопросы, отвеченные за [`ANSWERS_DAYS`]
//!   суток от начала текущих суток UTC и ещё не показанные этой сессии — `session_seen`; строка
//!   `ZeroAIMemory: ответ на «<вопрос>»: <ответ> — q:<id> → i:<решение>`, `reason: answer`) и
//!   `### Открытые вопросы` (`N открытых: «…», «…», «…»`, до [`OPEN_QUESTIONS_TITLES`]
//!   заголовков, не подсказка записи), `### Окружение` (M4.5 §8: до [`ENVIRONMENT_MAX`] записей
//!   Vault `host`/`environment`, `<имя> — <purpose>; <address>; ограничения: … — v:<id>`; без
//!   учёта показа; в `recovery` и `brief` нет), Rules (текст каждой заметки, до
//!   [`RULES_MAX`] заметок, текст — до [`RULE_L1_TOKENS`] токенов), «L1 решения» — корневые узлы
//!   пространства решения, строка на корень — путь и L0 до [`SOLUTION_ROOT_L0_TOKENS`] (до
//!   [`SOLUTION_ROOT_MAX`]), действующие (`active`) `gotcha`/`failure` решения и Knowledge
//!   (новые первыми; строка — заголовок, без фрагмента), записи `revisit` и последние записи
//!   changelog решения — индексы `<ссылка> «<заголовок>» · Nt` (M6 §8; разделов — до
//!   `recall.digest_entries`); сразу после правил — строка
//!   `Словарь: N терминов — memory_search kinds=[term]` (N > 0, термины решения и Knowledge) и
//!   строка `Проверенные команды: <путь> (проверено <verified_at>)[; …]` — до
//!   [`VERIFIED_COMMANDS_MAX`] заметок пространства решения с `verified_at`, новые проверки
//!   первыми (M4.5 §7, решение владельца 2026-10-28), за ней — строка карты кода решения
//!   (M5.6 §10, [`super::code::code_line`]: размер, дата снимка UTC, центральные символы;
//!   ≤ 80 токенов; нет карты или `code.enabled = false` — строки нет; у хуков (`session_start`
//!   и запрос с полем `code_map`) её гасят и `hooks.code_map`, и `code_map: false` клиента;
//!   так же в пакете восстановления), при `recall.digest_code_map_tokens > 0` — мини-карта
//!   `### Карта кода` (центральные символы в пределах ключа; по умолчанию 0 — нет).
//!   Ответ несёт `hooks: {session_start, prompt, pre_tool, post_tool, checkpoint,
//!   subagent_start, file_context, session_journal, recovery}` — настройки типов подсказок
//!   пользователя `hooks.<тип>` (R5, [`super::user_settings`], задача 13). Тип запроса выключен у
//!   пользователя на сервере — пустой `text` без сбора и без учёта показа (`session_seen`,
//!   `usage`; сессия касается), у `session_start` — с `hooks` (N4); у `recovery` и `brief` гейта
//!   нет;
//! - **`prompt`** — поиск `mode=brief` (без реранка, без кэша выжимок, `skip_seen: false`) по
//!   первым 512 символам `prompt` в Knowledge, Rules и решении; из выдачи — только insight и
//!   заметки Rules с баллом ≥ `recall.prompt_min_score` (0,6; поиск только FTS —
//!   `recall.prompt_min_score_fts`, M6 §9), не больше [`PROMPT_MAX`];
//!   перед ними — до [`TERM_HINTS_MAX`] строк словаря (M4.5): имя или псевдоним действующего
//!   термина решения и Knowledge в первых 512 символах `prompt` —
//!   `ZeroAIMemory: термин «<имя>» — <определение> t:<id>`, `reason: glossary`, каждый термин
//!   раз за сессию (`session_seen`); словарь — из кэша [`crate::glossary::GlossaryCache`];
//! - **идеи** (M5.7 §6; `[ideas] proactive` сервера и `ideas.proactive` пользователя): в
//!   дайджесте `session_start` — раздел `### Идеи` сразу после строки открытых вопросов
//!   ([`digest_ideas`]: строка-указание и до `ideas.digest_max` строк
//!   `ZeroAIMemory: идея «…» [<priority>] — w:<id>`, `reason: idea`; только пространство
//!   решения и только первый `session_start` сессии; нет в `recovery` и `brief`); к `prompt` —
//!   до `ideas.prompt_max` строк отдельного лёгкого поиска идей после основного
//!   ([`prompt_ideas`]; общие идеи Knowledge — [`knowledge_ideas_related`]). Кандидаты —
//!   [`zam_store::idea_candidate`]: «от агента», `proposed`, `declined`, `done`, `deferred` до
//!   `horizon` и `accepted` с живой задачей не предлагаются никогда; `accepted` со снятой или
//!   удалённой задачей (§16) — после `new`/`deferred`, строка с пометкой «задача снята|удалена,
//!   можно предложить вернуть»; показ — `session_seen`, раз за сессию;
//! - **`post_tool`** — ошибка инструмента: совпадения `error_regex` действующих триггеров
//!   insight решения и Knowledge (по первым [`ERROR_SCAN_MAX`] байтам `error`; триггер с
//!   `tools` — только для этих инструментов, если `tool` задан), затем FTS по insight по
//!   нормализованной ошибке ([`normalize_error`]): строгий запрос (все слова) — как есть,
//!   ослабленный (любое слово) — если запись покрывает достаточно слов ошибки
//!   ([`CoverageQuery`], правило `recall.post_tool_coverage`, M6 §9). Не больше [`POST_TOOL_MAX`]; строка — «ZeroAIMemory: эта ошибка уже
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
//! - **эпоха `session_seen`** (M6 §7.2): `session_start` с `source` `compact | resume | clear`
//!   и `event: recovery` начинают новую эпоху сессии ([`EPOCH_SOURCES`], `bump_epoch`) до сбора:
//!   выданное в прежних эпохах больше не считается показанным (повторы подсказок, ответы на
//!   вопросы), а показ этого ответа отмечается в новой. Не зависит от `record` и настроек
//!   подсказок: эпоха — состояние контекста агента. Показ идей — без эпохи: раздел `### Идеи` и
//!   подсказки-идеи `prompt` видят показ любой эпохи. Сбой — WARN, эпоха прежняя, и этот
//!   ответ ничего не прячет как показанное (фильтра повторов текущей эпохи нет): лучше
//!   повторить, чем потерять выданное до сжатия;
//! - **пакет восстановления** (M4.5 §9.2) — `event: recovery` (MCP, агенты без
//!   `SessionStart compact`) или `session_start` с `source` `compact | resume`: бюджет по
//!   умолчанию `recall.recovery_tokens` (2000), разделы по порядку — `### Задачи` (захваченные
//!   сессией первыми), `### Ответы человека` **без** фильтра `session_seen` (контекст агента
//!   потерян; показ всё равно отмечается), `### Решения` (действующие `decision` пространства
//!   решения за [`RECOVERY_DECISIONS_DAYS`] суток от начала текущих суток UTC, до
//!   `recall.digest_entries`, `reason: decision`), затем обычный дайджест в остатке бюджета
//!   (уведомления, открытые вопросы, словарь, …); Rules — резерв сверх бюджета, как в дайджесте.
//!   У `session_start` ключ пользователя `hooks.recovery` выключен — обычный дайджест,
//!   `hooks.session_start` выключен — тишина. `event: recovery` (MCP) от настроек подсказок
//!   хуков не зависит (§0, §10). `hooks` в ответе — только у `session_start`;
//! - **пакет под задачу `brief`** (M4.5 §9.4) — `task` (первые [`BRIEF_TASK_MAX_CHARS`]
//!   символов, длиннее — обрезка и `warnings: ["task truncated"]`), бюджет по умолчанию
//!   `recall.brief_tokens` (800); разделы: `### Правила` (L0 заметок Rules), `### Решения`
//!   (действующие `decision` с `axiom`/`theorem` по поиску `task`, до 3), `### Термины`
//!   (термины словаря в `task`, до 5, с определением), `### Ловушки` (действующие
//!   `gotcha`/`failure` по поиску `task`, до 3), `### Файлы` (записи со ссылками на пути из
//!   `task` — индекс `file_refs` решения, до 3), `### Код` (M5.6 §10: символы карты с именами
//!   или `qual` из `task` — точные `qual` первыми, затем символы путей из `task`, до
//!   [`BRIEF_CODE_MAX`], строка `<путь>:<line> <kind> <sig или qual>`, бюджет раздела —
//!   `min(code.brief_tokens, остаток)`; у хука `subagent-start` (поле `code_map`) выключенный
//!   `hooks.code_map` или `code_map: false` — раздела нет, явный вызов не гасится; учёта показа
//!   нет). Без `task` — мини-дайджест: правила L0,
//!   ловушки решения и Knowledge, `### Задачи` (захваченные `parent_id` или сессией — первыми).
//!   Поиск — `mode=brief`, балл ≥ `recall.prompt_min_score`. Повторы не отсекаются (у субагента
//!   свой контекст); показ учитывается (`usage.source = brief`), кроме записей раздела
//!   «Файлы» (ревизия неизвестна — `session_seen` не отмечается). От настроек подсказок хуков
//!   не зависит: хук `subagent-start` гейтится на клиенте;
//! - неизвестные поля тела игнорируются (клиент хуков может быть новее); прочие `source` хука
//!   (`startup | clear | fork`) — обычный дайджест, `source` пишется в журнал DEBUG;
//! - `output_limit {unit: chars|bytes, max}` (ревью 8a M6, C1) — предел канала хука: весь
//!   `text` в нём (Rules — первыми, хвост их приоритета — L0, затем снимаются разделы с меньшим
//!   `keep`, правила — со строкой «не показано — правил: N»); хук без поля (есть `source` или
//!   `code_map`) — 7500 байт; `max` вне 1..=1 000 000 или иная единица — 400;
//! - `budget_tokens` — 1..=200000 (иначе 400); `event` — `session_start | prompt | post_tool |
//!   recovery | brief` (иначе 400; `pre_tool` — только локально в хуке, §10.3).
//!
//! Маршрут — служебный, рядом с `/search` и `/read`: `/recall` пишет только `usage` и
//! `session_seen`, а не память, поэтому `memory_writes` не растёт (решение контроллера R12).
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use zam_core::{AGENT_BLOCK_VERSION, Id, SolutionGuid, TargetKind, TargetRef};
use zam_search::recall::{
    CoverageQuery, CoverageRule, HINT_PREFIX, Section, StemWord, WordMatch, error_regex_matches,
    error_terms, hint_line, normalize_error, one_line, pack_sections, reserved_text, stem_words,
};
use zam_search::{truncate_marked, truncate_to_tokens};
use zam_store::{
    Certainty, Entry, EntryFilter, EntryKind, EntryStatus, FtsQuery, GraphStore, NodeKind,
    RECALL_PART, SeenItem, SeenKey, SpaceKind, StoreError, TargetFilter, TokenCount, TriggerKind,
    UsageEvent, UsageKind, UsageSource, active_triggers, bump_epoch, count_tokens, list_children,
    list_entries, mark_seen_with_tokens, node_content, record_usages, search_fts_channels,
    seen_revs, seen_revs_any_epoch, touch_session,
};

use super::json_body;
use super::search::targets::{ScopeReq, SearchTarget, dedup_keep_order, resolve_targets};
use super::search::{Health, SOLUTION_HEADER};
use super::sessions::{McpOrigin, RequestSession, explicit_only, seen_session};
use super::user_settings::{UserFlags, user_flags};
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
/// Тестовое начало id сессии: новая эпоха `session_seen` в `/recall` «не записывается» (фича
/// `test-hooks`, проверка ответа без фильтра повторов при сбое `bump_epoch`).
#[cfg(feature = "test-hooks")]
pub const TEST_EPOCH_FAIL_PREFIX: &str = "zam-test-epoch-fail";
/// Заметок Rules в дайджесте.
pub const RULES_MAX: usize = 20;
/// Предел текста одной заметки Rules в дайджесте и пакете восстановления (L1 правила, M6 §8),
/// токенов.
pub const RULE_L1_TOKENS: usize = 200;
/// Корневых узлов решения («L1 решения») в дайджесте.
pub const SOLUTION_ROOT_MAX: usize = 10;
/// Предел L0 корня решения в строке дайджеста, токенов (M6 §8: индекс корней).
pub const SOLUTION_ROOT_L0_TOKENS: usize = 20;
/// Предел L0 правила в строке, сокращённой под предел канала, токенов (повторное ревью 8a):
/// первая фраза L0, длиннее — с «…».
pub const RULE_L0_TOKENS: usize = 40;
/// Правило переводится в L0, только если L1 длиннее L0 больше чем в 1,4 раза (×10): выигрыш
/// меньше — смысл теряется сильнее, чем экономится канал (повторное ревью 8a, п. 3).
const RULE_L0_GAIN_X10: usize = 14;
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
/// Раздел пакета под задачу с символами карты кода (M5.6 §10).
const SECTION_CODE: &str = "### Код";
/// Символов карты в разделе [`SECTION_CODE`].
pub const BRIEF_CODE_MAX: usize = 5;

/// `source` хука `SessionStart`, после которого строится пакет восстановления.
const RECOVERY_SOURCES: [&str; 2] = ["compact", "resume"];
/// `source` хука `SessionStart`, начинающие новую эпоху `session_seen` (M6 §7.2): контекст
/// агента сжат, восстановлен или очищен.
pub const EPOCH_SOURCES: [&str; 3] = ["compact", "resume", "clear"];
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
    /// Карта кода в ответе по настройке клиента `[hooks] code_map` (M5.6 §10): поле шлют хуки
    /// (`session-start`, `subagent-start`); есть поле — действует и `hooks.code_map`
    /// пользователя, нет (явный вызов REST/MCP `recovery`/`brief`) — не гасится.
    code_map: Option<bool>,
    /// Предел канала вывода хука (ревью 8a, C1): весь `text` укладывается в него сервером,
    /// клиент режет только страховочно. Нет у хука (старый клиент) — [`HOOK_OUTPUT_LIMIT`].
    output_limit: Option<OutputLimit>,
}

/// Единица предела вывода: символы (Claude Code) или байты UTF-8 (Codex).
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum LimitUnit {
    Chars,
    Bytes,
}

/// Предел вывода канала `{unit, max}`.
#[derive(Deserialize, Clone, Copy, Debug)]
struct OutputLimit {
    unit: LimitUnit,
    max: usize,
}

impl OutputLimit {
    /// Размер текста в единицах предела.
    fn size(&self, text: &str) -> usize {
        match self.unit {
            LimitUnit::Chars => text.chars().count(),
            LimitUnit::Bytes => text.len(),
        }
    }

    fn fits(&self, text: &str) -> bool {
        self.size(text) <= self.max
    }
}

/// Предел вывода хука без поля `output_limit` (клиент до ревью 8a): самый строгий из
/// клиентских — Codex, 7500 байт (`zam-cli` `hook::output::CODEX_MAX_BYTES`).
const HOOK_OUTPUT_LIMIT: OutputLimit = OutputLimit {
    unit: LimitUnit::Bytes,
    max: 7500,
};
/// Наибольший `output_limit.max`.
const OUTPUT_LIMIT_MAX: usize = 1_000_000;

/// Предел вывода ответа: из запроса; у хука без поля (есть `source` или `code_map` — их шлют
/// только хуки, MCP — нет) — [`HOOK_OUTPUT_LIMIT`]; MCP и прочие — без предела.
fn output_limit(req: &RecallReq, mcp: bool) -> Option<OutputLimit> {
    req.output_limit.or_else(|| {
        (!mcp && (req.source.is_some() || req.code_map.is_some())).then_some(HOOK_OUTPUT_LIMIT)
    })
}

/// Правило раздела-резерва при укладке в предел вывода (ревью 8a, C1).
#[derive(Clone)]
struct FitRule<'c> {
    /// Место в приоритете ([`rule_order`]).
    rank: usize,
    cand: &'c Cand,
    /// Блок: текст правила (до [`RULE_L1_TOKENS`]) или строка L0.
    block: String,
    /// Блок сокращён — текст обрезан с «…» или заменён строкой L0 (строка [`RULES_SHORT_NOTE`]).
    cut: bool,
}

/// Правила раздела-резерва при укладке в предел вывода (ревью 8a, C1) — в порядке вывода;
/// `hidden` — правил, что не показаны (строка [`rules_hidden_line`]).
#[derive(Clone)]
struct RulesFit<'c> {
    ri: usize,
    items: Vec<FitRule<'c>>,
    hidden: usize,
}

impl<'c> RulesFit<'c> {
    /// Раздел-резерв упаковки; нет — `None`.
    fn of(
        packed: &[Section],
        picked: &[Vec<&'c Cand>],
        short: &HashMap<TargetRef, RuleShort>,
        hidden: usize,
    ) -> Option<Self> {
        let ri = packed.iter().position(|s| s.reserved)?;
        let items = picked[ri]
            .iter()
            .zip(packed[ri].blocks.iter())
            .map(|(c, b)| {
                let s = short.get(&c.tref);
                FitRule {
                    rank: s.map_or(usize::MAX, |s| s.rank),
                    cand: c,
                    block: b.clone(),
                    cut: s.is_some_and(|s| s.cut),
                }
            })
            .collect();
        Some(Self { ri, items, hidden })
    }

    /// Блоки и подсказки раздела — в упаковку.
    fn apply(&self, packed: &mut [Section], picked: &mut [Vec<&'c Cand>]) {
        let sec = &mut packed[self.ri];
        sec.blocks = self.items.iter().map(|r| r.block.clone()).collect();
        if self.items.iter().any(|r| r.cut) {
            sec.blocks.push(RULES_SHORT_NOTE.to_string());
        }
        if self.hidden > 0 {
            sec.blocks.push(rules_hidden_line(self.hidden));
        }
        picked[self.ri] = self.items.iter().map(|r| r.cand).collect();
    }

    /// У правила с конца приоритета, ещё с L1, — строка L0, если L1 длиннее L0 больше чем в
    /// `gain_x10 / 10` раза ([`RULE_L0_GAIN_X10`] — шаг (1), 10 — шаг (1′)); нечего — `false`.
    fn degrade(&mut self, short: &HashMap<TargetRef, RuleShort>, gain_x10: usize) -> bool {
        let next = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                short.get(&r.cand.tref).is_some_and(|s| {
                    // сравнивается смысл: строки L1 под заголовком правила и фраза L0
                    let b = &r.block;
                    s.l0 != *b
                        && s.l0.len() < b.len()
                        && l1_text(b).chars().count() * 10
                            > l0_text(&s.l0).chars().count() * gain_x10
                })
            })
            .max_by_key(|(_, r)| r.rank)
            .map(|(k, _)| k);
        let Some(k) = next else {
            return false;
        };
        let item = &mut self.items[k];
        item.block = short[&item.cand.tref].l0.clone();
        item.cut = true;
        true
    }

    /// У правила с конца приоритета, ещё не голого, — строка пути `<путь> … n:<id>` (шаг (1″));
    /// нечего — `false`.
    fn strip(&mut self, short: &HashMap<TargetRef, RuleShort>) -> bool {
        let next = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                short
                    .get(&r.cand.tref)
                    .is_some_and(|s| s.bare != r.block && s.bare.len() < r.block.len())
            })
            .max_by_key(|(_, r)| r.rank)
            .map(|(k, _)| k);
        let Some(k) = next else {
            return false;
        };
        let item = &mut self.items[k];
        item.block = short[&item.cand.tref].bare.clone();
        item.cut = true;
        true
    }

    /// Правило с конца приоритета — прочь (его учитывает строка о непоказанных).
    fn drop_last(&mut self) -> bool {
        let Some(k) = (0..self.items.len()).max_by_key(|&k| self.items[k].rank) else {
            return false;
        };
        self.items.remove(k);
        self.hidden += 1;
        true
    }
}

/// Текст L1 блока правила — строки под строкой правила.
fn l1_text(block: &str) -> &str {
    block.split_once('\n').map_or("", |(_, rest)| rest.trim())
}

/// Фраза строки L0 правила (`<путь> — <фраза> n:<id>`).
fn l0_text(line: &str) -> &str {
    let tail = line.split_once(" — ").map_or("", |(_, t)| t);
    tail.rsplit_once(" n:").map_or(tail, |(t, _)| t).trim()
}

/// Упаковка в бюджет и предел вывода `limit` (ревью 8a, C1; M6 §8): Rules — первыми по
/// приоритету и сверх `digest_tokens`, остальное — в `min(cap, остаток предела)`. Не влезает:
/// (1) у правил с конца приоритета ([`rule_order`]) L1 заменяется строкой L0 (первая фраза,
/// до [`RULE_L0_TOKENS`], обрезка — с «…»; правило с L1 ≤ L0 × 1,4 пропускается), пока правила
/// и разделы в бюджете не уложатся, и в разделе — строка [`RULES_SHORT_NOTE`]; (2) снимаются
/// блоки в бюджете по стойкости `Section::keep` (0 → 1 → 2), внутри раздела — с конца; (1″)
/// прежде чем снять блок со стойкостью ≥ 2 (обязательные строки), строки L0 правил с конца
/// приоритета сокращаются до пути `<путь> … n:<id>` ([`rule_bare_block`]);
/// (3) правила (уже L0) снимаются с конца, а строка [`rules_hidden_line`] называет их число —
/// ни одно правило не пропадает молча.
fn pack_limited(
    packed: &mut [Section],
    picked: &mut [Vec<&Cand>],
    short: &HashMap<TargetRef, RuleShort>,
    hidden: usize,
    cap: usize,
    limit: Option<OutputLimit>,
) -> (String, Vec<(usize, usize)>) {
    let free = |p: &[Section]| pack_sections(p, cap, &count_tokens, &|_| true);
    let Some(limit) = limit else {
        return free(packed);
    };
    let fits = |t: &str| limit.fits(t);
    let mut fit = RulesFit::of(packed, picked, short, hidden);
    if let Some(f) = fit.as_mut() {
        // (1) L1 → L0 с заметным выигрышем, пока правила с разделами в бюджете не влезут
        while !fits(&free(packed).0) && f.degrade(short, RULE_L0_GAIN_X10) {
            f.apply(packed, picked);
        }
        // (1′) предел снял бы ловушки, указатели или обязательные строки (`keep` ≥ 1) — L1 → L0
        // у остальных правил (L0 — целая первая фраза), прежде чем их снимать
        while loses_kept(packed, cap, &fits) && f.degrade(short, 10) {
            f.apply(packed, picked);
        }
        // (1″) предел снял бы обязательные строки (`keep` ≥ 2: уведомления, задачи, ответы, идеи,
        // первая ловушка) — у правил с конца приоритета строка L0 сокращается до пути
        // (`<путь> … n:<id>`; ТС-2 Codex sh-05, правки ревью 8b): правило остаётся на месте со
        // ссылкой, обязательная строка — тоже. Впустую не сокращается: если обязательную строку
        // не спасти и со всеми правилами до пути, шаг пропускается (повторное ревью 8b)
        if loses_at(packed, cap, &fits, KEEP_MANDATORY)
            && rescues_mandatory(f, packed, picked, short, cap, &fits)
        {
            while loses_at(packed, cap, &fits, KEEP_MANDATORY) && f.strip(short) {
                f.apply(packed, picked);
            }
        }
        // (3) сами правила (L0) не влезают — с конца, со строкой о непоказанных
        while limit.size(&reserved_text(packed)) + 1 > limit.max && f.drop_last() {
            f.apply(packed, picked);
        }
    }
    // (2) разделы в бюджете — по стойкости, пока весь текст (со строками о снятом) не влезет;
    // не влезает и без них — правила с конца, со строкой о непоказанных
    loop {
        let r = pack_sections(packed, cap, &count_tokens, &fits);
        if fits(&r.0) {
            return r;
        }
        let Some(f) = fit.as_mut() else {
            return r;
        };
        if !f.drop_last() {
            return r;
        }
        f.apply(packed, picked);
    }
}

/// Снимет ли упаковка в пределе хоть один блок со стойкостью ≥ [`KEEP_POINTERS`] (ловушки,
/// указатели, обязательные строки), что вошёл бы в бюджет без предела, — условие шага (1′).
fn loses_kept(packed: &[Section], cap: usize, fits: &dyn Fn(&str) -> bool) -> bool {
    loses_at(packed, cap, fits, KEEP_POINTERS)
}

/// Спасёт ли шаг (1″) обязательные строки: пробная укладка со всеми правилами до пути не снимает
/// блоков со стойкостью ≥ [`KEEP_MANDATORY`].
fn rescues_mandatory<'c>(
    f: &RulesFit<'c>,
    packed: &[Section],
    picked: &[Vec<&'c Cand>],
    short: &HashMap<TargetRef, RuleShort>,
    cap: usize,
    fits: &dyn Fn(&str) -> bool,
) -> bool {
    let mut trial = f.clone();
    while trial.strip(short) {}
    let (mut tp, mut tk) = (packed.to_vec(), picked.to_vec());
    trial.apply(&mut tp, &mut tk);
    !loses_at(&tp, cap, fits, KEEP_MANDATORY)
}

/// Снимет ли упаковка в пределе хоть один блок со стойкостью ≥ `keep`, что вошёл бы в бюджет без
/// предела.
fn loses_at(packed: &[Section], cap: usize, fits: &dyn Fn(&str) -> bool, keep: u8) -> bool {
    let (_, all) = pack_sections(packed, cap, &count_tokens, &|_| true);
    let (_, got) = pack_sections(packed, cap, &count_tokens, fits);
    all.iter().any(|&(si, bi)| {
        !packed[si].reserved && packed[si].keep_of(bi) >= keep && !got.contains(&(si, bi))
    })
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
        zam_store::time::now(),
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

/// Подсказка-индекс записи журнала (M6 §8: changelog и revisit): `<ссылка> «<заголовок>» · Nt`,
/// `N` — размер записи (заголовок и тело) счётчиком выдачи: агент решает, читать ли её.
fn index_cand(target: usize, e: &Entry, reason: &'static str) -> Cand {
    let mut c = entry_cand(target, e, reason, "");
    let size = count_tokens(&format!("{}\n\n{}", e.title, e.body));
    // заголовок — до `SOLUTION_ROOT_L0_TOKENS`, как L0 корня (ревью 8a, M3)
    let title = one_line(&e.title);
    let cap = zam_store::budget_cap(SOLUTION_ROOT_L0_TOKENS);
    let (title, _) = truncate_marked(&title, cap, &count_tokens);
    c.block = format!("{} «{title}» · {size}t", c.reference);
    c
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

/// GUID решения запроса (`solution` тела или `X-Zam-Solution`) — только если решение в
/// области [`resolve_targets`]: своё (пространство решения личной цели) или чужое с действующим
/// доступом (отдельная цель). Ожидание, отказ, отзыв доступа и неизвестный GUID — `None`:
/// разделы по GUID (окружение Vault, файлы `brief`) исключают решение так же, как поиск.
fn scoped_solution(
    targets: &[SearchTarget],
    req: &RecallReq,
    headers: &HeaderMap,
) -> Option<SolutionGuid> {
    let guid = super::files::query_solution(req.solution.as_deref(), headers)
        .ok()
        .flatten()?;
    targets
        .iter()
        .any(|t| match &t.solution {
            Some(info) => info.guid == guid,
            None => t.spaces.iter().any(|s| {
                s.kind == SpaceKind::Solution && s.solution_guid.as_deref() == Some(guid.as_str())
            }),
        })
        .then_some(guid)
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
        with_reports: false,
        deleted_only: false,
        origin: None,
        priority: None,
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
const REASON_IDEA: &str = "idea";
/// Предел заголовка идеи в строке дайджеста и подсказки, символов.
const IDEA_TITLE_CHARS: usize = 120;
/// Тестовая метка заголовка идеи: отдельный поиск идей к `prompt` «не укладывается в срок»
/// (фича `test-hooks`, проверка тишины по идеям).
#[cfg(feature = "test-hooks")]
pub const TEST_SLOW_IDEA_MARK: &str = "zam-test-slow-idea";
/// Запросы, по которым шёл второй проход подсказки-идей ([`released_prompt_ideas`]) — только с
/// фичей `test-hooks` (тест: второй проход не нужен, когда первый уже дал предел).
#[cfg(feature = "test-hooks")]
pub static TEST_SECOND_PASS_QUERIES: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

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
/// k:<id>`, у `blocked` после ссылки — ` — блокер: <blocked_by>`. `next_step` и `blocked_by` —
/// свободный текст до 1000 символов (туда попадают команды с ключами): секреты маскируются, как
/// в L1 правил и ответах (финальное ревью M4.5, M-2). `claim` — пометка захвата у `open`:
/// `[open · <пометка>]`.
fn task_block(e: &Entry, claim: Option<&str>) -> String {
    let t = zam_store::TaskData::from_stored(&e.data);
    let status = e.status.map_or("open", EntryStatus::as_str);
    let status = match claim {
        Some(m) => format!("{status} · {m}"),
        None => status.to_string(),
    };
    let mut s = format!("ZeroAIMemory: [{status}] {}", one_line(&e.title));
    if let Some(n) = &t.next_step {
        let n = clip(&zam_markdown::mask_text(n), TASK_LINE_TEXT_CHARS);
        s.push_str(&format!(" — далее: {n}"));
    }
    s.push_str(&format!(" — {}", entry_ref(e)));
    if e.status == Some(EntryStatus::Blocked)
        && let Some(b) = &t.blocked_by
    {
        let b = clip(&zam_markdown::mask_text(b), TASK_LINE_TEXT_CHARS);
        s.push_str(&format!(" — блокер: {b}"));
    }
    s
}

/// Пометка захваченной задачи `open` в строке дайджеста (предложение zero-agent, задача 8a).
const CLAIMED_MINE: &str = "захвачена этой сессией";
/// Пометка задачи `open`, захваченной другой сессией.
const CLAIMED_OTHER: &str = "захвачена другой сессией";
/// Пометка захваченной задачи `open` в ответе без сессии (чья — неизвестно).
const CLAIMED_ANY: &str = "захвачена";
/// Пометка задачи `open`, захваченной родителем, в пакете субагенту (`brief`, ревью 8a, M1).
const CLAIMED_PARENT: &str = "захвачена родительской сессией";

/// Задачи `in_progress` и `blocked` пространств `spaces` и задачи `open` с живым захватом
/// (`claim` статус не меняет — без них захваченная задача не видна): захваченные сессией
/// запроса — первыми, затем `in_progress`/`blocked`, затем `open`, захваченные другими; внутри —
/// недавно менявшиеся первыми. У `open` в строке — пометка захвата ([`CLAIMED_MINE`],
/// [`CLAIMED_OTHER`]).
async fn task_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
    session: Option<&str>,
    mine_mark: &'static str,
) -> Result<Vec<Cand>, ApiError> {
    let now = zam_store::time::now();
    let claimed = |e: &Entry| {
        zam_store::TaskData::from_stored(&e.data)
            .claimed_by
            .filter(|c| c.live(now))
    };
    let mut rows = entries_of(health, deadline, spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::InProgress, EntryStatus::Blocked],
        ..entry_filter(s, EntryKind::Task)
    })
    .await?;
    // `open` — отдельной выборкой (последние `ENTRY_FETCH` по пространству): незахваченные
    // задачи `open` не вытесняют из выборки задачи в работе
    let open = entries_of(health, deadline, spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::Open],
        ..entry_filter(s, EntryKind::Task)
    })
    .await?;
    rows.extend(open.into_iter().filter(|(_, e)| claimed(e).is_some()));
    let mine = |e: &Entry| session.is_some_and(|sid| claimed(e).is_some_and(|c| c.session == sid));
    let rank = |e: &Entry| match (mine(e), e.status) {
        (true, _) => 0,
        (false, Some(EntryStatus::Open)) => 2,
        _ => 1,
    };
    rows.sort_by_key(|(_, e)| (rank(e), std::cmp::Reverse((e.updated_at, e.id))));
    Ok(rows
        .iter()
        .map(|(i, e)| {
            let mut c = entry_cand(*i, e, "task", "");
            let mark = (e.status == Some(EntryStatus::Open)).then(|| match session {
                _ if mine(e) => mine_mark,
                Some(_) => CLAIMED_OTHER,
                None => CLAIMED_ANY,
            });
            c.block = task_block(e, mark);
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
    let since = zam_store::days_ago(
        zam_store::time::day_start_utc(zam_store::time::now()),
        ANSWERS_DAYS,
    );
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

/// Подсказка-идея цели `target` со строкой `block` (`kind: idea`, `reason: idea`).
fn idea_cand(target: usize, e: &Entry, block: String) -> Cand {
    let mut c = entry_cand(target, e, REASON_IDEA, "");
    c.block = block;
    c
}

/// Проактивность идей (§8): `[ideas] proactive` сервера и `ideas.proactive` пользователя
/// (`server && user`; настройки пользователя — одним чтением с `hooks.<тип>`, сбой чтения —
/// всё включено).
fn ideas_enabled(st: &AppState, flags: &UserFlags) -> bool {
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
async fn digest_ideas(
    st: &AppState,
    p: &Principal,
    session: Option<&RequestSession>,
    health: &Health<'_>,
    deadline: &mut Deadline,
) -> Vec<Cand> {
    let max = st.cfg.ideas.digest_max as usize;
    let Some((i, space)) = solution_space(health.targets()) else {
        return Vec::new();
    };
    if max == 0 || !health.alive[i] || !ideas_writable(p, &health.targets()[i]) {
        return Vec::new();
    }
    let t = &health.targets()[i];
    match deadline.step(idea_shown_in_session(session, t)).await {
        Some(false) => {}
        Some(true) | None => return Vec::new(),
    }
    let Some(r) = deadline
        .step(zam_store::proactive_ideas(
            &t.db,
            &[space],
            zam_store::time::utc_today(),
            &HashSet::new(),
            max,
        ))
        .await
    else {
        return Vec::new();
    };
    let cands = match r {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "recall: идеи дайджеста не прочитаны");
            return Vec::new();
        }
    };
    let mut out = Vec::with_capacity(cands.len());
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
        out.push(idea_cand(i, e, idea_line(e, released_mark(e, task))));
    }
    out
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
async fn prompt_ideas(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    session: Option<&RequestSession>,
    health: &mut Health<'_>,
    deadline: &Deadline,
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
    let r = collect_prompt_ideas(st, p, headers, req, &query, health, &mut own).await;
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

/// Запас тестовой «медленной идеи» до срока ([`test_slow_idea`]).
#[cfg(feature = "test-hooks")]
const TEST_SLOW_MARGIN: Duration = Duration::from_millis(300);

/// Предлагать идеи цели `t` можно, только если `propose` в неё пройдёт: токен `ro` и участник
/// общего решения с `ro` получили бы 403 — им ни раздела `### Идеи`, ни подсказок-идей
/// (финальное ревью M5.7, Minor 3).
fn ideas_writable(p: &Principal, t: &SearchTarget) -> bool {
    super::scope::effective_write(p, t.role).is_ok()
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
/// `sources` и совпадение слов запроса со словами идеи по основам ([`idea_words_match`]).
fn idea_lexically_matched(item: &Value, e: &Entry, q: &IdeaQuery) -> bool {
    let lexical = item["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| matches!(s.as_str(), Some("fts" | "alias" | "trigram")));
    lexical && q.matches(&e.title, &e.body)
}

/// Значимых слов идеи в пороге [`idea_words_match`], не больше: длинное тело не делает порог
/// недостижимым (половина от 6 — 3 слова, не больше порога покрытия запроса 4).
const IDEA_WORDS_CAP: usize = 6;
/// Порог покрытия слов запроса у подсказок-идей — свой и фиксированный (M5.7: ⌈n/3⌉ от 2 до 4,
/// основы): ключ `recall.post_tool_coverage` (M6 §9) на него не влияет.
const IDEA_QUERY_RULE: CoverageRule = CoverageRule {
    divisor: 3,
    cap: 4,
    word_match: WordMatch::Stems,
};

/// Слова запроса `terms` ([`error_terms`]) подтверждаются заголовком `title` и телом `body`
/// идеи (финальное ревью M5.7, I-1′ и I-1″). Служебные слова не считаются ни в запросе, ни в
/// идее ([`stem_words`]). Нужно хотя бы одно совпадение по основе со словом **заголовка**:
/// совпадения только с телом («пользователь», «server») связи с замыслом не доказывают. Далее:
/// покрыты все слова запроса — да; иначе нужно не меньше
/// `min(порог покрытия запроса, max(1, ⌈k / 2⌉))` совпавших, где `k` — значимые слова идеи (до
/// [`IDEA_WORDS_CAP`]). Порог ограничен словами самой идеи: к длинному запросу Claude Code
/// короткая идея иначе не подходила бы никогда. Общий код слов — `zam_search::recall`
/// ([`StemWord`], задача 9 M6).
#[cfg(test)]
fn idea_words_match(terms: &[String], title: &str, body: &str) -> bool {
    IdeaQuery::new(terms).matches(title, body)
}

/// Слова запроса подсказки-идеи, подготовленные один раз на запрос (перф: раньше стоп-список,
/// нормализация и основы запроса пересчитывались на каждого кандидата).
struct IdeaQuery(Vec<StemWord>);

impl IdeaQuery {
    fn new(terms: &[String]) -> IdeaQuery {
        IdeaQuery(stem_words(terms))
    }

    /// Правило [`idea_words_match`] для заголовка `title` и тела `body` идеи.
    fn matches(&self, title: &str, body: &str) -> bool {
        let q = &self.0;
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
        let query_need = IDEA_QUERY_RULE.need(q.len());
        let k = words.len().min(IDEA_WORDS_CAP);
        covered >= query_need.min(k.div_ceil(2).max(1))
    }
}

/// Идея подсказки: (цель, идея, общая из Knowledge, пометка «задача снята»).
type FoundIdea = (usize, Entry, bool, Option<&'static str>);

/// Первый проход [`prompt_ideas`] (`status [new, deferred]`) со своим сроком `d`. Ошибки
/// чтения баз не трогают `health.alive`: иначе сбой поиска идей снял бы основные подсказки той
/// же цели.
async fn collect_prompt_ideas(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    query: &str,
    health: &mut Health<'_>,
    d: &mut Deadline,
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
        .map(|(i, e, _, mark)| idea_cand(*i, e, idea_prompt_line(e, *mark)))
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
///   идеи с живой задачей сюда не попадают и мест не занимают;
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
        .map(|(i, e, _, mark)| idea_cand(*i, e, idea_prompt_line(e, *mark)))
        .collect()
}

/// Один проход отдельного поиска идей [`collect_prompt_ideas`] по статусам `statuses`:
/// кандидаты [`zam_store::idea_candidate`] в порядке выдачи. `None` — срок истёк.
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
    let Some(v) = inner_search(st, p, headers, area, body, health, d).await? else {
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
async fn knowledge_ideas_related(
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

/// Записи `host`/`environment` Vault пользователя запроса: личного и решения запроса, если оно
/// в области ([`scoped_solution`]; Vault участника лежит в его базе, чужой не виден, после
/// отзыва доступа — только личный). Читает метаданные без мастер-ключа, так что
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
    // решение без доступа (отзыв, ожидание, отказ, неизвестный GUID) — только личный Vault
    let guid = scoped_solution(health.targets(), req, headers);
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
#[allow(clippy::too_many_arguments)]
async fn digest(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    recovery: bool,
    code_on: bool,
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
    let tasks = task_cands(
        health,
        deadline,
        &insight_spaces,
        session.as_deref(),
        CLAIMED_MINE,
    )
    .await?;
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

    // Rules — текст каждой заметки (до `RULE_L1_TOKENS`); больше `RULES_MAX` — первые по
    // приоритету ([`rule_order`]), остальные — строкой «не показано — правил: N»; вывод — по пути
    let mut rules: Vec<Cand> = Vec::new();
    let mut rules_short: HashMap<TargetRef, RuleShort> = HashMap::new();
    let mut rules_hidden = 0usize;
    let mut rules_cut = false;
    if let Some((i, space)) = personal_space(targets, SpaceKind::Rules) {
        let db = targets[i].db.clone();
        if let Some(r) = deadline.step(list_children(&db, space, None, 16)).await {
            let mut notes: Vec<zam_store::Node> = r?
                .into_iter()
                .filter(|n| n.kind == NodeKind::Note)
                .collect();
            notes.sort_by(rule_order);
            rules_hidden = notes.len().saturating_sub(RULES_MAX);
            notes.truncate(RULES_MAX);
            // место в приоритете — для урезания под предел вывода (сначала — последние)
            let rank: HashMap<Id, usize> =
                notes.iter().enumerate().map(|(k, n)| (n.id, k)).collect();
            notes.sort_by(|a, b| a.path.cmp(&b.path));
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
                // строка правила — `<путь> [надёжность] n:<id>`: под заголовком `### Правила`
                // метка подсказки и слово «правило» лишние, как у строки L0 (задача 8c)
                let mut block = rule_head(&n.path, certainty.as_deref(), &reference);
                // текст правила целиком до `RULE_L1_TOKENS` (решение контролёра по ревью 8a:
                // оглавление с первыми фразами секций теряло вторую фразу правила); длиннее —
                // с «…» и строкой [`RULES_SHORT_NOTE`]; секреты маскируются, как в чанках и L0;
                // абзац — одной строкой, заголовок `# `, повторяющий путь, опущен (задача 8c)
                let (summary, cut) = rule_l1(&text, &n.path);
                for line in summary.lines().filter(|l| !l.trim().is_empty()) {
                    block.push_str("\n  ");
                    block.push_str(line.trim_end());
                }
                rules_cut |= cut;
                rules_short.insert(
                    TargetRef {
                        kind: TargetKind::Node,
                        id: n.id,
                    },
                    RuleShort {
                        rank: rank[&n.id],
                        l0: rule_l0_block(&n),
                        bare: rule_bare_block(&n),
                        cut,
                    },
                );
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
                // индекс корней (M6 §8): путь и L0 до `SOLUTION_ROOT_L0_TOKENS`
                if let Some(l0) = n.l0.as_deref().map(one_line).filter(|s| !s.is_empty()) {
                    let cap = zam_store::budget_cap(SOLUTION_ROOT_L0_TOKENS);
                    let (l0, _) = truncate_marked(&l0, cap, &count_tokens);
                    if !l0.is_empty() {
                        title.push_str(" — ");
                        title.push_str(&l0);
                    }
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
    .map(|(i, e)| index_cand(*i, e, "revisit"))
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
    .map(|(i, e)| index_cand(*i, e, "changelog"))
    .collect();
    sections.push((Some("### Последние изменения"), changes));
    // строки после правил: словарь и проверенные команды решения (M4.5 §7)
    // адрес снятой строки — вместе со строкой (ревью 8a, круг 3)
    let mut after_rules: Vec<(String, Omit)> = glossary_line(st, health, deadline)
        .await
        .into_iter()
        .map(|l| (l, OMIT_GLOSSARY))
        .collect();
    after_rules.extend(
        verified_line(health, deadline, sol)
            .await
            .map(|l| (l, OMIT_VERIFIED)),
    );
    // строка карты кода — после проверенных команд (M5.6 §10)
    let mut code_map = Vec::new();
    let mut code_line = None;
    if code_on {
        code_line = code_digest_line(req, headers, health, deadline).await;
        // мини-карта (M6 §8, решение 16) — только при `digest_code_map_tokens > 0`
        code_map = code_digest_map(st, req, headers, health, deadline).await;
    }
    let lines = Lines {
        notices,
        open_line,
        after_rules,
        code_line,
        code_map,
        rules_short,
        rules_hidden,
        rules_cut,
    };
    Ok((lines, sections))
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

/// L0 заметок Rules (пакет под задачу): `<путь> — <L0> [надёжность] n:<id>` — под заголовком
/// `### Правила` без метки подсказки и слова «правило», как в дайджесте (ревью 8c, п. 13).
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
            let block = rule_head(&title, certainty.as_deref(), &reference);
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
        // захваченные родителем (`parent_id`) — «родительской сессией» (ревью 8a, M1)
        let mark = if req.parent_id.is_some() {
            CLAIMED_PARENT
        } else {
            CLAIMED_MINE
        };
        let tasks = task_cands(health, deadline, &insight_spaces, session.as_deref(), mark).await?;
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
        // отсечка карточек поиска (M6 §4.2) — не для подсказок: у них свой порог балла
        "brief_min_rel": 0.0,
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
/// точно или оканчивается на `/<путь из task>`. Без решения или решение вне области
/// ([`scoped_solution`]) — пусто; сбой чтения — WARN.
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
    // решение без доступа исключено, как в поиске
    let Some(guid) = scoped_solution(health.targets(), req, headers) else {
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
    let since = zam_store::days_ago(
        zam_store::time::day_start_utc(zam_store::time::now()),
        RECOVERY_DECISIONS_DAYS,
    );
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

/// Строки дайджеста, что не подсказки записей (ни `session_seen`, ни `usage`): уведомления,
/// строка открытых вопросов, строки после правил (словарь, проверенные команды, строка карты
/// кода) и мини-карта кода `### Карта кода`.
#[derive(Default)]
struct Lines {
    notices: Vec<String>,
    open_line: Option<String>,
    /// Строки словаря и проверенных команд — с адресом снятой строки ([`Omit`]).
    after_rules: Vec<(String, Omit)>,
    /// Строка карты кода (M5.6 §10): в дайджесте и восстановлении — резерв вместе с Rules, под
    /// пределом канала не снимается (повторное ревью 8a, 2а).
    code_line: Option<String>,
    code_map: Vec<String>,
    /// Правила раздела: место в приоритете ([`rule_order`]) и строка L0 — для урезания под
    /// предел вывода канала ([`fit_rules`]).
    rules_short: HashMap<TargetRef, RuleShort>,
    /// Правил сверх `RULES_MAX` (строка [`rules_hidden_line`]).
    rules_hidden: usize,
    /// Текст хоть одного правила обрезан до [`RULE_L1_TOKENS`] (строка [`RULES_SHORT_NOTE`]).
    rules_cut: bool,
}

/// Правило раздела для урезания под предел вывода: место в приоритете ([`rule_order`]),
/// строка L0 ([`rule_l0_block`]) и обрезан ли его текст в разделе (с «…»).
struct RuleShort {
    rank: usize,
    l0: String,
    /// Строка пути без фразы `<путь> … n:<id>` (шаг (1″) [`pack_limited`]).
    bare: String,
    cut: bool,
}

/// Текст правила для раздела Rules — тело заметки без frontmatter.
fn rule_body(text: &str) -> &str {
    text.get(zam_markdown::parse(text).body_start..)
        .unwrap_or(text)
}

/// L1 правила (M6 §8, решение контролёра по ревью 8a): текст правила с маскированными секретами
/// до `budget_cap(RULE_L1_TOKENS)` (M6 §4.2: без модели — с запасом оценки); `true` — обрезан с
/// «…». Обвязка без потери смысла (задача 8c): строки абзаца — одной строкой ([`unwrap_lines`]),
/// первый заголовок `# `, слова которого все есть в пути правила, опущен ([`heading_repeats`]).
/// Тот же текст считает `GET /index/status` (`rules.l1_tokens`, `rules_heavy`).
pub(crate) fn rule_l1(text: &str, path: &str) -> (String, bool) {
    let body = zam_markdown::mask_text(rule_body(text));
    let body = unwrap_lines(body.trim());
    // правило из одного заголовка-повтора остаётся с заголовком: пустого текста не бывает
    let body = match body.split_once('\n') {
        Some((first, rest)) if heading_repeats(first, path) && !rest.trim().is_empty() => {
            rest.trim_start().to_string()
        }
        _ => body,
    };
    truncate_marked(
        body.trim(),
        zam_store::budget_cap(RULE_L1_TOKENS),
        &count_tokens,
    )
}

/// Строка правила раздела Rules: `<путь> [надёжность] n:<id>` — без метки подсказки и слова
/// «правило» (под заголовком `### Правила` они лишние; так же у строки L0, [`rule_l0_block`]).
fn rule_head(path: &str, certainty: Option<&str>, reference: &str) -> String {
    hint_line("", path, &[certainty.unwrap_or_default()], reference)
        .trim_start_matches(HINT_PREFIX)
        .trim_start()
        .to_string()
}

/// Заголовок `# …` повторяет путь правила: все его слова (буквы и цифры, без регистра) есть среди
/// слов пути (`process/code-review` — `code`, `review`). Такой заголовок — обвязка: путь в строке
/// правила уже называет его.
fn heading_repeats(line: &str, path: &str) -> bool {
    let Some(h) = line.strip_prefix("# ") else {
        return false;
    };
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let p = words(path);
    let hw = words(h);
    !hw.is_empty() && hw.iter().all(|w| p.contains(w))
}

/// Абзацы Markdown — одной строкой (задача 8c): мягкий перенос внутри абзаца — пробел (так его и
/// читает Markdown), пустые строки между блоками опускаются. Не склеиваются: заголовки, элементы
/// списков, цитаты, таблицы, строки с отступом от 4 пробелов, строки с жёстким переносом (два
/// пробела или `\` в конце), подчёркивания setext (`===`, `---`) и разделители (`---`, `***`,
/// `___`) и всё внутри блока кода ```/~~~ — они остаются отдельными строками как есть.
///
/// Известные ограничения (разбор построчный, без полного парсера CommonMark): строка таблицы без
/// ведущего `|` склеивается с соседней; табуляция в отступе считается одним символом (строка с
/// отступом в один таб склеивается); ограждение кода из 4 бэктиков закрывается первой строкой с
/// 3 бэктиками внутри него; ленивое продолжение цитаты (`>` только в первой строке) склеивается
/// с ней. Все случаи сохраняют текст, меняется только перенос строки.
fn unwrap_lines(text: &str) -> String {
    // подчёркивание setext или разделитель: строка из одного символа `=`, `-`, `*`, `_`
    // (пробелы между ними допустимы), `=` — от одного, прочие — от трёх
    let rule_line = |t: &str| {
        let c: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
        match c.first() {
            Some('=') => c.iter().all(|&x| x == '='),
            Some(&f @ ('-' | '*' | '_')) => c.len() >= 3 && c.iter().all(|&x| x == f),
            _ => false,
        }
    };
    let starts_block = |l: &str| {
        let t = l.trim_start();
        let lead = l.len() - t.len();
        lead >= 4
            || rule_line(t)
            || t.starts_with('#')
            || t.starts_with('>')
            || t.starts_with('|')
            || t.starts_with("```")
            || t.starts_with("~~~")
            || ["- ", "* ", "+ "].iter().any(|m| t.starts_with(m))
            || {
                let d = t.bytes().take_while(u8::is_ascii_digit).count();
                d > 0 && matches!(t.as_bytes().get(d..d + 2), Some([b'.' | b')', b' ']))
            }
    };
    let mut out: Vec<String> = Vec::new();
    // последняя строка открыта для продолжения абзаца
    let mut open = false;
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(f) = fence {
            out.push(line.trim_end().to_string());
            if t.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if t.is_empty() {
            open = false;
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(&t[..3]);
            out.push(line.trim_end().to_string());
            open = false;
            continue;
        }
        let hard = line.ends_with("  ") || line.ends_with('\\');
        if open && !starts_block(line) {
            let last = out.last_mut().expect("открытая строка есть");
            last.push(' ');
            last.push_str(line.trim());
        } else {
            out.push(line.trim_end().to_string());
        }
        // заголовок, строка таблицы и подчёркивание/разделитель абзац не продолжают; элемент
        // списка — продолжает (ленивое продолжение Markdown)
        open = !hard && !t.starts_with('#') && !t.starts_with('|') && !rule_line(t);
    }
    out.join("\n")
}

/// Приоритет правил (ревью 8a, I1; M6 §8): надёжность (`axiom` → `theorem` → прочие), затем
/// давность создания (старые — основа проекта, новые чаще уточняют), затем путь. Детерминирован
/// при неизменной базе и не зависит от имён: `security/*` и `process/*` не уходят первыми из-за
/// алфавита. Отбор `RULES_MAX` и урезание под предел вывода идут с конца этого порядка.
fn rule_order(a: &zam_store::Node, b: &zam_store::Node) -> std::cmp::Ordering {
    let rank = |n: &zam_store::Node| match n.certainty {
        Some(Certainty::Axiom) => 0,
        Some(Certainty::Theorem) => 1,
        _ => 2,
    };
    (rank(a), a.created_at, &a.path).cmp(&(rank(b), b.created_at, &b.path))
}

/// Строка L0 правила под пределом канала: `<путь> — <первая фраза L0> [<cert>] n:<id>` (под
/// заголовком `### Правила` метка `ZeroAIMemory:` и слово «правило» лишние — экономия канала);
/// фраза — до [`RULE_L0_TOKENS`], длиннее — с «…».
fn rule_l0_block(n: &zam_store::Node) -> String {
    let mut title = n.path.clone();
    if let Some(l0) = n.l0.as_deref().map(one_line).filter(|s| !s.is_empty()) {
        let l0 = zam_markdown::mask_text(&l0);
        let l0 = rule_l0_phrase(&l0, zam_store::budget_cap(RULE_L0_TOKENS));
        if !l0.is_empty() {
            title.push_str(" — ");
            title.push_str(&l0);
        }
    }
    let certainty = certainty_str(n.certainty);
    hint_line(
        "",
        &title,
        &[certainty.as_deref().unwrap_or_default()],
        &format!("n:{}", n.id),
    )
    // под заголовком `### Правила` метка подсказки лишняя — ≈ 14 байт строки у Codex
    .trim_start_matches(HINT_PREFIX)
    .trim_start()
    .to_string()
}

/// Строка правила без фразы — `<путь> … [<cert>] n:<id>` (шаг (1″) [`pack_limited`]: место под
/// обязательные строки; «…» — текст правила опущен, полностью — `memory_read n:<id>`).
fn rule_bare_block(n: &zam_store::Node) -> String {
    let certainty = certainty_str(n.certainty);
    hint_line(
        "",
        &format!("{} …", n.path),
        &[certainty.as_deref().unwrap_or_default()],
        &format!("n:{}", n.id),
    )
    .trim_start_matches(HINT_PREFIX)
    .trim_start()
    .to_string()
}

/// Фраза L0 правила: первая фраза до `cap` токенов (длиннее — обрезка с «…»); если за ней в
/// L0 есть ещё текст — точка в конце заменяется на «…» (повторное ревью 8a, I-b: вторая фраза
/// часто несёт исключение или запрет, её потеря не молчит).
fn rule_l0_phrase(l0: &str, cap: usize) -> String {
    let l0 = l0.trim();
    let first = first_sentence(l0);
    let (mut out, cut) = truncate_marked(first, cap, &count_tokens);
    if !cut && first.len() < l0.len() {
        // точка или «…» в конце первой фразы — одна пометка «…», не «……»
        let base = out.trim_end_matches(['.', '…']).to_string();
        let marked = format!("{base}…");
        out = if count_tokens(&marked) <= cap {
            marked
        } else {
            truncate_marked(&base, cap.saturating_sub(1), &count_tokens).0
        };
        if !out.ends_with('…') {
            out.push('…');
        }
    }
    out
}

/// Первая фраза текста: до `.`, `!`, `?` или `…` с пробелом или концом после; нет — весь текст.
/// Известное ограничение: точка сокращения с пробелом («т. е.», «e.g. ») считается концом фразы.
fn first_sentence(s: &str) -> &str {
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '.' | '!' | '?' | '…') && it.peek().is_none_or(|(_, n)| n.is_whitespace())
        {
            return &s[..i + c.len_utf8()];
        }
    }
    s
}

/// Строка-пояснение раздела Rules, если хоть одно правило показано строкой L0 (повторное ревью
/// 8a): в стиле прочих пометок подсказок.
const RULES_SHORT_NOTE: &str =
    "ZeroAIMemory: правила сокращены («…» — обрезано) — полностью memory_read n:<id>";

/// Событие `session_tokens` резерва Rules дайджеста и пакета восстановления (M6 §8).
pub const RULES_TOKEN_EVENT: &str = "rules";

/// Строка о правилах, что не показаны (сверх `RULES_MAX` или не вошли в предел вывода).
fn rules_hidden_line(n: usize) -> String {
    format!("ZeroAIMemory: не показано — правил: {n} (memory_tree Rules)")
}

/// Дайджест `session_start`: строки [`Lines`] и разделы-кандидаты.
type Digest = (Lines, Vec<(Option<&'static str>, Vec<Cand>)>);

/// Заголовок мини-карты кода дайджеста (M6 §8).
const SECTION_CODE_MAP: &str = super::code::DIGEST_MAP_TITLE;

/// Мини-карта кода дайджеста: центральные символы карты решения строками `<путь>:<line> <kind>
/// <qual>` в пределах `recall.digest_code_map_tokens` (с заголовком, `budget_cap`); 0 — пусто.
async fn code_digest_map(
    st: &AppState,
    req: &RecallReq,
    headers: &HeaderMap,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Vec<String> {
    let budget = st.cfg.recall.digest_code_map_tokens;
    if budget == 0 {
        return Vec::new();
    }
    let Some((i, guid)) = code_target(health.targets(), req, headers) else {
        return Vec::new();
    };
    if !health.alive[i] {
        return Vec::new();
    }
    let db = health.targets()[i].db.clone();
    let Some(lines) = deadline
        .step(super::code::digest_map_lines(&db, guid.as_str(), budget))
        .await
    else {
        return Vec::new();
    };
    lines
}

/// Цель с картой кода решения запроса: `(индекс цели, GUID)` — решение в области
/// ([`scoped_solution`]); карта хранится в базе владельца решения (у участника — чужая цель).
fn code_target(
    targets: &[SearchTarget],
    req: &RecallReq,
    headers: &HeaderMap,
) -> Option<(usize, SolutionGuid)> {
    let guid = scoped_solution(targets, req, headers)?;
    let i = targets.iter().position(|t| match &t.solution {
        Some(info) => info.guid == guid,
        None => t.spaces.iter().any(|s| {
            s.kind == SpaceKind::Solution && s.solution_guid.as_deref() == Some(guid.as_str())
        }),
    })?;
    Some((i, guid))
}

/// Строка карты кода дайджеста (M5.6 §10, [`super::code::code_line`]); нет решения, карты или
/// срок вышел — `None`.
async fn code_digest_line(
    req: &RecallReq,
    headers: &HeaderMap,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Option<String> {
    let (i, guid) = code_target(health.targets(), req, headers)?;
    if !health.alive[i] {
        return None;
    }
    let db = health.targets()[i].db.clone();
    deadline
        .step(super::code::code_line(&db, guid.as_str()))
        .await
        .flatten()
}

/// Имена символов из `task` для раздела `### Код`: слова вида `Тип::метод` или `a.b` (кандидаты
/// `qual`, первыми), затем идентификаторы [`zam_code::focus_idents`]; без повторов.
fn task_idents(task: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in task.split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | ';')) {
        let w = w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if w.contains("::") && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    for w in zam_code::focus_idents(task) {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Строки раздела `### Код` пакета под задачу (M5.6 §10): символы карты решения с именами или
/// `qual` из `task` (точные `qual` первыми), затем символы путей из `task`
/// (`code_symbols_named`), до [`BRIEF_CODE_MAX`]; строка — `<путь>:<line> <kind> <sig или
/// qual>`; вместе с заголовком — не больше `code.brief_tokens` (остаток общего бюджета
/// соблюдает упаковка). Нет решения, карты или срок вышел — пусто; сбой чтения — WARN.
async fn code_brief_lines(
    st: &AppState,
    req: &RecallReq,
    headers: &HeaderMap,
    task: &str,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Vec<String> {
    let budget = st.cfg.code.brief_tokens;
    let Some((i, guid)) = code_target(health.targets(), req, headers) else {
        return Vec::new();
    };
    if budget == 0 || !health.alive[i] {
        return Vec::new();
    }
    let (names, paths) = (task_idents(task), task_paths(task));
    if names.is_empty() && paths.is_empty() {
        return Vec::new();
    }
    let db = health.targets()[i].db.clone();
    let Some(r) = deadline
        .step(zam_store::code::code_symbols_named(
            &db,
            guid.as_str(),
            &names,
            &paths,
            BRIEF_CODE_MAX,
        ))
        .await
    else {
        return Vec::new();
    };
    let hits = match r {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "recall: символы карты кода не прочитаны");
            return Vec::new();
        }
    };
    let mut used = count_tokens(&format!("\n{SECTION_CODE}\n"));
    // сумма строк — против `budget_cap` (M6 §4.2: без модели — с запасом оценки)
    let budget = zam_store::budget_cap(budget);
    let mut out = Vec::new();
    for h in hits {
        let what = h
            .sig
            .as_deref()
            .map(one_line)
            .unwrap_or_else(|| h.qual.clone());
        let line = format!("{}:{} {} {what}", h.path, h.line, h.kind.as_str());
        let cost = count_tokens(&format!("{line}\n"));
        if used + cost <= budget {
            used += cost;
            out.push(line);
        }
    }
    out
}

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
        // отсечка карточек поиска (M6 §4.2) — не для подсказок: у них свой порог балла
        "brief_min_rel": 0.0,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let area = req.area.as_deref();
    let Some(v) = inner_search(st, p, headers, area, body, health, deadline).await? else {
        return Ok(Vec::new());
    };
    let min = prompt_min_score(st, &v);
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

/// Порог балла подсказки к `prompt` по ответу внутреннего поиска `search`: поиск деградировал до
/// FTS (`degraded`) — `recall.prompt_min_score_fts` (не задан — `prompt_min_score`), иначе
/// `recall.prompt_min_score` (M6 §9).
fn prompt_min_score(st: &AppState, search: &Value) -> f64 {
    if search["degraded"].as_bool() == Some(true) {
        st.cfg
            .recall
            .prompt_min_score_fts
            .unwrap_or(st.cfg.recall.prompt_min_score)
    } else {
        st.cfg.recall.prompt_min_score
    }
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
        None,
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
    rule: &CoverageRule,
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
    // порог покрытия ослабленного FTS — `recall.post_tool_coverage` (M6 §9)
    let cover = CoverageQuery::new(&terms, rule);
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
                    entry_ids: Vec::new(),
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
            if relaxed && !cover.enough(&format!("{}\n{}", e.title, h.text)) {
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
        target: c.tref.into(),
        part: RECALL_PART.to_string(),
    }
}

/// Уже показанные этой сессии подсказки (любая ревизия): в текущей эпохе, а идеи — в любой
/// (M6 §7.2: одна идея — один показ за сессию). Сбой чтения — WARN, без фильтра.
async fn shown(
    s: Option<&RequestSession>,
    targets: &[SearchTarget],
    cands: &[&Cand],
) -> HashSet<SeenKey> {
    let Some(s) = s else {
        return HashSet::new();
    };
    let idea = EntryKind::Idea.as_str();
    let (ideas, rest): (Vec<&Cand>, Vec<&Cand>) = cands.iter().partition(|c| c.kind == idea);
    let keys = |cs: &[&Cand]| -> Vec<SeenKey> { cs.iter().map(|c| seen_key(targets, c)).collect() };
    let (rest, ideas) = (keys(&rest), keys(&ideas));
    let r = async {
        let mut m = seen_revs(&s.db, &s.touch.id, s.touch.epoch, &rest).await?;
        m.extend(seen_revs_any_epoch(&s.db, &s.touch.id, &ideas).await?);
        Ok::<_, StoreError>(m)
    };
    match r.await {
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
/// разделу (`entries` — `recall.digest_entries`, M6 §8), у разделов без заголовка — строки
/// словаря или общий предел события `limit`.
fn section_limit(
    event: Event,
    title: Option<&str>,
    cands: &[Cand],
    limit: usize,
    ideas_max: usize,
    entries: usize,
) -> usize {
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
        Some(_) => entries,
        None if cands.iter().any(|c| c.reason == REASON_GLOSSARY) => TERM_HINTS_MAX,
        None if cands.iter().any(|c| c.reason == REASON_IDEA) => ideas_max,
        None => limit,
    }
}

/// Раскладка отобранных разделов в разделы упаковки и подсказки каждого (у строк, что не
/// подсказки записей — уведомления, словарь, открытые вопросы, карта кода, — пустой список: ни
/// `session_seen`, ни `usage`). Уведомления — первыми; в пакете восстановления — после задач,
/// ответов и решений (§9.2), а если других разделов нет — в конце. Открытые вопросы — после
/// ответов (в пакете — вместе с уведомлениями), за ними — раздел [`SECTION_IDEAS`] (`ideas`,
/// строка-указание — в заголовке: она выводится, только если вошла хоть одна идея), строки
/// словаря, проверенных команд и карты кода и мини-карта [`SECTION_CODE_MAP`] — после правил.
/// `rules_reserved` — раздел Rules — резерв сверх бюджета (дайджест и пакет восстановления,
/// M6 §8; у `brief` — L0 Rules в бюджете).
fn arrange<'c>(
    recovery: bool,
    rules_reserved: bool,
    lines: Lines,
    ideas: Vec<&'c Cand>,
    chosen: Vec<(Option<&'static str>, Vec<&'c Cand>)>,
) -> (Vec<Section>, Vec<Vec<&'c Cand>>) {
    let Lines {
        notices,
        mut open_line,
        after_rules,
        code_line,
        code_map,
        rules_hidden,
        rules_cut,
        ..
    } = lines;
    let mut out = Arranged::default();
    let mut ideas = ideas;
    let mut notices = Some(notices);
    if !recovery {
        out.line(None, notices.take().unwrap_or_default(), KEEP_MANDATORY);
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
        let mut sec = Section::new(
            title.map(str::to_string),
            cands.iter().map(|c| c.block.clone()).collect(),
        );
        sec.reserved = rules_reserved && title == Some(SECTION_RULES);
        sec.keep = keep_of(title);
        sec.omit = omit_of(title);
        if title == Some(SECTION_TRAPS_DIGEST) {
            // первая ловушка — выше обязательных строк: шаг (2) её не снимает (решения контролёра
            // по 421b75d, 2б)
            sec.keep_head = Some(KEEP_FIRST_TRAP);
        }
        if title == Some(SECTION_RULES) && rules_cut && !cands.is_empty() {
            sec.blocks.push(RULES_SHORT_NOTE.to_string());
        }
        if title == Some(SECTION_RULES) && rules_hidden > 0 {
            sec.blocks.push(rules_hidden_line(rules_hidden));
        }
        out.packed.push(sec);
        out.picked.push(cands);
        if title == Some(SECTION_ANSWERS)
            && !recovery
            && let Some(line) = open_line.take()
        {
            out.line(Some(SECTION_OPEN_QUESTIONS), vec![line], KEEP_POINTERS);
            out.omit_last(OMIT_OPEN_QUESTIONS);
        }
        if title == Some(SECTION_ANSWERS) && !recovery && !ideas.is_empty() {
            let mut sec = Section::new(
                Some(format!(
                    "{SECTION_IDEAS}\n{IDEAS_HINT}{}",
                    if ideas.iter().any(|c| c.block.contains(REOPEN_MARK_TAIL)) {
                        IDEAS_HINT_REOPEN
                    } else {
                        ""
                    }
                )),
                ideas.iter().map(|c| c.block.clone()).collect(),
            );
            sec.keep = KEEP_MANDATORY;
            sec.omit = Some(("идей".into(), "memory_search kinds=[idea]".into()));
            out.packed.push(sec);
            out.picked.push(std::mem::take(&mut ideas));
        }
        if title == Some(SECTION_RULES) {
            // строки словаря и проверенных команд — по разделу на строку: свой адрес снятого
            for (l, omit) in &after_rules {
                out.line(None, vec![l.clone()], KEEP_POINTERS);
                out.omit_last(*omit);
            }
            if let Some(l) = &code_line {
                let mut sec = Section::new(None, vec![l.clone()]);
                sec.reserved = rules_reserved;
                sec.keep = KEEP_POINTERS;
                out.packed.push(sec);
                out.picked.push(Vec::new());
            }
            if !code_map.is_empty() {
                out.line(Some(SECTION_CODE_MAP), code_map.clone(), 0);
            }
        }
    }
    // пакет восстановления без раздела после трёх первых: уведомления и вопросы не теряются
    out.notices(&mut notices, &mut open_line);
    (out.packed, out.picked)
}

/// Стойкость к пределу вывода (ревью 8a, C1; [`Section::keep`]): обязательные строки ТС-2 —
/// уведомления, задачи, ответы, идеи.
const KEEP_MANDATORY: u8 = 2;
/// Первая (самая новая) ловушка: шаг (2) снимает её последней — после обязательных строк.
const KEEP_FIRST_TRAP: u8 = 3;
/// Строки-указатели: открытые вопросы, строки после правил (словарь, проверенные команды, карта
/// кода) и ловушки; при снятии — с конца, то есть открытые вопросы — последними.
const KEEP_POINTERS: u8 = 1;

/// Стойкость раздела дайджеста: обязательные — [`KEEP_MANDATORY`], ловушки — [`KEEP_POINTERS`],
/// прочие (решения, окружение, корни, revisit, changelog, разделы `brief`) — 0.
fn keep_of(title: Option<&str>) -> u8 {
    match title {
        Some(SECTION_TASKS | SECTION_ANSWERS) => KEEP_MANDATORY,
        Some(SECTION_TRAPS_DIGEST) => KEEP_POINTERS,
        _ => 0,
    }
}

/// Адрес снятой строки открытых вопросов.
const OMIT_OPEN_QUESTIONS: Omit = ("строк открытых вопросов", "memory_search kinds=[question]");

/// Адрес снятого блока `(что, где полностью)` — строка «не показано — <что>: N (<где>)».
type Omit = (&'static str, &'static str);
/// Адрес снятой строки словаря.
const OMIT_GLOSSARY: Omit = ("строк словаря", "memory_search kinds=[term]");
/// Адрес снятой строки проверенных команд — заметки решения.
const OMIT_VERIFIED: Omit = ("строк проверенных команд", "memory_tree");

/// Заголовок раздела ловушек дайджеста.
const SECTION_TRAPS_DIGEST: &str = "### Ловушки и сбои";

/// Строка о блоках раздела, снятых пределом вывода (повторное ревью 8a, 2в): что и где полностью.
fn omit_of(title: Option<&str>) -> Option<(String, String)> {
    let (what, whole) = match title? {
        SECTION_TASKS => ("задач", "memory_search kinds=[task]"),
        SECTION_ANSWERS => ("ответов человека", "memory_search kinds=[question]"),
        SECTION_DECISIONS => ("решений", "memory_search kinds=[insight]"),
        SECTION_TRAPS_DIGEST => ("ловушек", "memory_search kinds=[insight]"),
        "### Пересмотреть (revisit)" => {
            ("записей revisit", "memory_search kinds=[insight]")
        }
        "### Последние изменения" => {
            ("изменений", "memory_search kinds=[changelog]")
        }
        "### Решение" => ("корней решения", "memory_tree"),
        SECTION_ENVIRONMENT => ("записей окружения", "memory_vault"),
        _ => return None,
    };
    Some((what.to_string(), whole.to_string()))
}

/// Накопитель [`arrange`].
#[derive(Default)]
struct Arranged<'c> {
    packed: Vec<Section>,
    picked: Vec<Vec<&'c Cand>>,
}

impl Arranged<'_> {
    /// Раздел строк, что не подсказки записей.
    fn line(&mut self, title: Option<&str>, blocks: Vec<String>, keep: u8) {
        let mut sec = Section::new(title.map(str::to_string), blocks);
        sec.keep = keep;
        self.packed.push(sec);
        self.picked.push(Vec::new());
    }

    /// Строка о снятом у последнего раздела: `(что, где)`.
    fn omit_last(&mut self, omit: (&str, &str)) {
        if let Some(s) = self.packed.last_mut() {
            s.omit = Some((omit.0.to_string(), omit.1.to_string()));
        }
    }

    /// Уведомления и строка открытых вопросов, если ещё не выведены.
    fn notices(&mut self, notices: &mut Option<Vec<String>>, open_line: &mut Option<String>) {
        if let Some(n) = notices.take() {
            self.line(None, n, KEEP_MANDATORY);
        }
        if let Some(l) = open_line.take() {
            self.line(Some(SECTION_OPEN_QUESTIONS), vec![l], KEEP_POINTERS);
            self.omit_last(OMIT_OPEN_QUESTIONS);
        }
    }
}

/// `POST /recall`.
pub async fn recall(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
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
    if req
        .output_limit
        .is_some_and(|l| !(1..=OUTPUT_LIMIT_MAX).contains(&l.max))
    {
        return Err(ApiError::BadRequest(format!(
            "output_limit.max: от 1 до {OUTPUT_LIMIT_MAX}"
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
    let mut session = match seen_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        req.session_id.as_deref(),
    )
    .await
    {
        // неявная сессия MCP (M6 §7.1) — без отметок, фильтра повторов и учёта
        Ok(s) => explicit_only(s),
        // некорректный id — 400, несуществующий пользователь (`X-Zam-User`) — 404
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "recall: база сессии недоступна, без учёта повторов");
            None
        }
    };
    // новая эпоха не записалась (M6 §7.2): ответ без фильтра повторов текущей эпохи
    let mut epoch_failed = false;
    if let Some(s) = &mut session {
        // известное из тела дописывается в сессию
        if let Some(g) = &req.solution {
            s.touch.solution_guid = g.trim().parse().ok();
        }
        if area.is_some() {
            s.touch.area = area.clone();
        }
        // новая эпоха `session_seen` (M6 §7.2) — до чтения показанного и отметок ответа
        let new_epoch = req.event == Event::Recovery
            || (req.event == Event::SessionStart
                && req
                    .source
                    .as_deref()
                    .is_some_and(|src| EPOCH_SOURCES.contains(&src.trim())));
        if new_epoch {
            #[cfg(feature = "test-hooks")]
            let r = if s.touch.id.starts_with(TEST_EPOCH_FAIL_PREFIX) {
                Err(StoreError::Corrupt("тестовый сбой новой эпохи".into()))
            } else {
                bump_epoch(&s.db, &s.touch.id).await
            };
            #[cfg(not(feature = "test-hooks"))]
            let r = bump_epoch(&s.db, &s.touch.id).await;
            match r {
                Ok(e) => s.touch.epoch = e,
                Err(e) => {
                    tracing::warn!(error = %e, "recall: новая эпоха сессии не записана");
                    epoch_failed = true;
                }
            }
        }
    }
    let user = target_user_id(&st, &p, &headers)?;
    // R5: настройки пользователя (`hooks.<тип>`, `ideas.proactive`) одним чтением; сбой
    // чтения — всё включено (подсказки не валятся)
    let flags = match user_flags(&st.system, user).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "recall: настройки пользователя не прочитаны");
            UserFlags::default()
        }
    };
    let hooks = flags.hooks;
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
    // карта кода (M5.6 §10): сервер с картой и тип подсказок `code_map` пользователя
    // `session_start` — всегда хук; `recovery`/`brief` — хук, если прислал `code_map`
    // (ревью задачи 6, M2: явные вызовы REST/MCP/CLI тип подсказок не гасит, §10)
    let from_hook = req.event == Event::SessionStart || req.code_map.is_some();
    let code_on = st.cfg.code.enabled
        && (!from_hook || (hooks.enabled("code_map") && req.code_map != Some(false)));
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
    let mut code_section: Vec<String> = Vec::new();
    // идеи (M5.7 §6): раздел дайджеста и подсказка `prompt`, `server && user`
    let ideas_on = enabled
        && matches!(req.event, Event::SessionStart | Event::Prompt)
        && !recovery
        && ideas_enabled(&st, &flags);
    let mut idea_cands: Vec<Cand> = Vec::new();
    let (mut lines, sections, repeatable) = match req.event {
        _ if !enabled => (Lines::default(), Vec::new(), true),
        // дайджест повторяется: после `/compact` и `resume` Claude Code шлёт тот же
        // `session_id`, а контекст агента уже потерян (показ всё равно отмечается)
        Event::SessionStart | Event::Recovery => {
            let (l, s) = digest(
                &st,
                &p,
                &sh,
                &req,
                recovery,
                code_on,
                &mut health,
                &mut deadline,
            )
            .await?;
            if ideas_on {
                idea_cands = digest_ideas(&st, &p, session.as_ref(), &health, &mut deadline).await;
            }
            (l, s, true)
        }
        Event::Prompt => {
            // строки словаря — перед подсказками insight; у каждого свой предел
            let prompt = req.prompt.as_deref().unwrap_or_default();
            let chars = crate::glossary::HINT_TEXT_CHARS;
            let terms = term_cands(&st, prompt, chars, &mut health, &mut deadline).await;
            let c = prompt_hints(&st, &p, &sh, &req, &mut health, &mut deadline).await?;
            // идеи — отдельным поиском после основного, своей строкой и своим пределом
            let ideas = if ideas_on {
                prompt_ideas(&st, &p, &sh, &req, session.as_ref(), &mut health, &deadline).await?
            } else {
                Vec::new()
            };
            (
                Lines::default(),
                vec![(None, terms), (None, c), (None, ideas)],
                false,
            )
        }
        Event::PostTool => {
            let rule = st.cfg.recall.post_tool_coverage;
            let c = error_hints(&req, &rule, &mut health, &mut deadline).await?;
            (Lines::default(), vec![(None, c)], true)
        }
        // у субагента свой контекст: пакет повторяется (показ отмечается)
        Event::Brief => {
            let t = task.as_deref();
            let s = brief(&st, &p, &sh, &req, t, &mut health, &mut deadline).await?;
            if let (true, Some(t)) = (code_on, t) {
                code_section =
                    code_brief_lines(&st, &req, &sh, t, &mut health, &mut deadline).await;
            }
            (Lines::default(), s, true)
        }
    };
    // без повторов: показанное сессии, повторы между разделами, исключённые цели; ответ на
    // вопрос показывается сессии один раз и в повторяемом дайджесте, а в пакете восстановления —
    // снова (контекст потерян, §9.2)
    let all: Vec<&Cand> = sections.iter().flat_map(|(_, c)| c).collect();
    let seen = if recovery || brief_event || epoch_failed {
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
    let ideas_max = st.cfg.ideas.prompt_max as usize;
    let entries = st.cfg.recall.digest_entries;
    let mut dup: HashSet<TargetRef> = HashSet::new();
    let chosen: Vec<(Option<&'static str>, Vec<&Cand>)> = sections
        .iter()
        .map(|(title, cands)| {
            let per = section_limit(req.event, *title, cands, limit, ideas_max, entries);
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
    // идеи дайджеста — не раздел `sections`: свой отбор уже сделан ([`digest_ideas`])
    let ideas: Vec<&Cand> = idea_cands
        .iter()
        .filter(|c| health.alive[c.target])
        .filter(|c| dup.insert(c.tref))
        .collect();
    // Rules дайджеста и пакета восстановления — резерв сверх бюджета (M6 §8)
    let rules_reserved = matches!(req.event, Event::SessionStart | Event::Recovery);
    let rules_short = std::mem::take(&mut lines.rules_short);
    let rules_hidden = lines.rules_hidden;
    let (mut packed, mut picked) = arrange(recovery, rules_reserved, lines, ideas, chosen);
    if !code_section.is_empty() {
        // раздел `### Код` — после `### Файлы`; строки карты не подсказки записей (без учёта)
        packed.push(Section::new(Some(SECTION_CODE.to_string()), code_section));
        picked.push(Vec::new());
    }
    // упаковка разделов суммами — против `budget_cap` (M6 §4.2); резерв Rules — вне бюджета,
    // его размер — отдельным полем `rules_tokens` (§17 п. 3)
    // предел канала хука (ревью 8a, C1): Rules — первыми, остальное — в остатке
    let limit = output_limit(&req, origin.is_some());
    let (text, taken) = pack_limited(
        &mut packed,
        &mut picked,
        &rules_short,
        rules_hidden,
        zam_store::budget_cap(budget),
        limit,
    );
    let rules_tokens = packed.iter().position(|s| s.reserved).map_or(0, |ri| {
        count_tokens(&reserved_text(std::slice::from_ref(&packed[ri])))
    });
    let shown_now: Vec<&Cand> = taken
        .iter()
        .filter_map(|&(si, bi)| picked.get(si).and_then(|p| p.get(bi)).copied())
        .collect();
    let collect_ms = ms(t0);

    // учёт показа — если полный срок ещё не вышел; иначе собранное отдаётся без учёта
    let rt = Instant::now();
    let record_ms = if req.record == Some(false) {
        None
    } else if rt < total_deadline {
        // выданное сессии по событию (§9.5): пустой ответ — не выдача; вызов из MCP учитывает
        // сам инструмент событием `mcp` (M6 §7.1) — без двойного счёта
        // Rules — отдельной строкой учёта `rules` (M6 §8, §17 п. 3), событие — без резерва Rules:
        // строки складываются без двойного счёта
        let mut tokens: Vec<TokenCount> = Vec::new();
        if !text.is_empty() && origin.is_none() {
            let all = count_tokens(&text) as u64;
            let rules = (rules_tokens as u64).min(all);
            tokens.push(TokenCount {
                event: token_event(req.event, recovery).into(),
                calls: 1,
                tokens: all - rules,
                reread_calls: 0,
            });
            if rules > 0 {
                tokens.push(TokenCount {
                    event: RULES_TOKEN_EVENT.into(),
                    calls: 1,
                    tokens: rules,
                    reread_calls: 0,
                });
            }
        }
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
        "rules_tokens": rules_tokens,
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
    use zam_search::recall::prepared_terms;

    // основы слов (бывш. `idea_stem_match_forms_and_short_words`) — тест `zam_search::recall`
    // `stem_word_match_forms_and_short_words` (задача 9 M6)

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
        let lines = Lines {
            notices,
            open_line: Some("1 открытых: «в»".into()),
            ..Lines::default()
        };
        let (packed, picked) = arrange(true, true, lines, Vec::new(), chosen);
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
        let lines = Lines {
            notices: vec!["n".into()],
            open_line: Some("q".into()),
            // адрес снятой строки — со строкой, не по её префиксу (ревью 8a, круг 3)
            after_rules: vec![
                ("Словарь".to_string(), OMIT_GLOSSARY),
                (
                    "Проверенные команды: a (проверено 2026-10-28)".into(),
                    OMIT_VERIFIED,
                ),
            ],
            ..Lines::default()
        };
        let (packed, _) = arrange(false, true, lines, Vec::new(), chosen);
        // раздел Rules — резерв сверх бюджета (M6 §8), прочие — в бюджете
        let reserved: Vec<bool> = packed.iter().map(|s| s.reserved).collect();
        assert_eq!(reserved, [false, false, false, false, true, false, false]);
        let omit = |k: usize| packed[k].omit.clone();
        let own = |o: Omit| Some((o.0.to_string(), o.1.to_string()));
        assert_eq!(omit(5), own(OMIT_GLOSSARY));
        assert_eq!(omit(6), own(OMIT_VERIFIED));
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

    /// Повторное ревью 8a, I-b: вторая фраза L0 не теряется молча — первая кончается «…».
    #[test]
    fn rule_l0_phrase_marks_dropped_second_sentence() {
        assert_eq!(rule_l0_phrase("A. Исключение: B.", 40), "A…");
        assert_eq!(rule_l0_phrase("Одна фраза.", 40), "Одна фраза.");
        assert_eq!(rule_l0_phrase("Без точки", 40), "Без точки");
        // первая фраза на «…» — одна пометка, не «……» (ревью 8a, круг 3)
        assert_eq!(
            rule_l0_phrase("Сначала так… Потом иначе.", 40),
            "Сначала так…"
        );
        assert_eq!(rule_l0_phrase("Так.… Иначе.", 40), "Так…");
        let long = "слово ".repeat(80);
        let out = rule_l0_phrase(&long, 10);
        assert!(out.ends_with('…') && count_tokens(&out) <= 10, "{out}");
    }

    /// Задача 8c: абзац — одной строкой; списки, цитаты, таблицы, заголовки, жёсткий перенос и
    /// блок кода — как есть; пустые строки опускаются.
    #[test]
    fn unwrap_lines_joins_paragraphs_only() {
        let text = "# Заголовок\n\nПервая строка\nвторая строка.\n\n- пункт один\n  продолжение\n- пункт \
                    два\n1. нумерованный\n> цитата\n| a | b |\n| c | d |\nжёсткий  \nперенос\n\n```\nкод\n\
                    строка\n```\nхвост\nабзаца";
        assert_eq!(
            unwrap_lines(text),
            "# Заголовок\nПервая строка вторая строка.\n- пункт один продолжение\n- пункт два\n\
             1. нумерованный\n> цитата\n| a | b |\n| c | d |\nжёсткий\nперенос\n```\nкод\nстрока\n```\n\
             хвост абзаца"
        );
    }

    /// Задача 8c: заголовок опускается, только если все его слова есть в пути правила.
    #[test]
    fn rule_l1_drops_heading_repeating_path() {
        assert!(heading_repeats("# Secrets", "security/secrets"));
        assert!(heading_repeats("# Code review", "process/code-review"));
        assert!(!heading_repeats("# Миграции базы", "backend/migrations"));
        assert!(!heading_repeats("Secrets", "security/secrets"));
        assert!(!heading_repeats("## Secrets", "security/secrets"));
        let (t, cut) = rule_l1(
            "---\nl0: x\n---\n# Secrets\n\nSecrets never go\ninto logs.\n",
            "security/secrets",
        );
        assert_eq!((t.as_str(), cut), ("Secrets never go into logs.", false));
        let (t, _) = rule_l1(
            "# Миграции базы\n\nТолько\nдобавляют.\n",
            "backend/migrations",
        );
        assert_eq!(t, "# Миграции базы\nТолько добавляют.");
        // правило из одного заголовка, повторяющего путь, остаётся с заголовком (ревью 8c, п. 9)
        assert_eq!(rule_l1("# Tests\n", "process/tests").0, "# Tests");
        assert_eq!(rule_l1("# Tests\n\n\n", "process/tests").0, "# Tests");
        // буквы любых письменностей и цифры — слова; пунктуация — разделитель
        assert!(heading_repeats("# 日本 2", "docs/日本-2"));
        assert!(heading_repeats("# V2 API", "backend/api_v2"));
        assert!(!heading_repeats("# 日本 3", "docs/日本-2"));
        assert!(!heading_repeats("# ", "docs/x"));
    }

    /// Ревью 8c, п. 8 и 10: setext и разделители, `~~~`, отступ 4 пробела, `\` в конце строки,
    /// нумерация `1)` — отдельными строками; маскировка секрета и склейка вместе.
    #[test]
    fn unwrap_lines_block_edges() {
        assert_eq!(
            unwrap_lines("Заголовок\n=========\nтекст\nдалее\n\nВторой\n---\nабзац"),
            "Заголовок\n=========\nтекст далее\nВторой\n---\nабзац"
        );
        assert_eq!(unwrap_lines("а\n***\nб\n_ _ _\nв"), "а\n***\nб\n_ _ _\nв");
        assert_eq!(
            unwrap_lines("до\n~~~\nкод\n  строка\n~~~\nпосле\nхвост"),
            "до\n~~~\nкод\n  строка\n~~~\nпосле хвост"
        );
        assert_eq!(
            unwrap_lines("абзац\n    код с отступом"),
            "абзац\n    код с отступом"
        );
        assert_eq!(
            unwrap_lines("строка\\\nперенос\nдальше"),
            "строка\\\nперенос дальше"
        );
        assert_eq!(
            unwrap_lines("1) первое\nвторое\n2) третье"),
            "1) первое второе\n2) третье"
        );
        // ключ AWS маскируется, переносы склеиваются (порядок `rule_l1`: маска, затем склейка)
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let (t, _) = rule_l1(&format!("Ключ {key}\nне хранить.\n"), "security/keys");
        assert!(!t.contains(&key) && !t.contains('\n'), "{t}");
        assert!(t.ends_with("не хранить."), "{t}");
    }

    /// Ревью 8c, п. 10: строка правила с надёжностью — `<путь> [th] n:<id>`.
    #[test]
    fn rule_head_with_certainty() {
        assert_eq!(rule_head("a/b", Some("theorem"), "n:1"), "a/b [th] n:1");
        assert_eq!(rule_head("a/b", None, "n:1"), "a/b n:1");
        assert_eq!(
            rule_head("a/b — фраза", Some("axiom"), "n:2"),
            "a/b — фраза [ax] n:2"
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
