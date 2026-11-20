//! Проактивное вспоминание `POST /recall` (§10.3, §12; задача 8 M4) — подсказки хукам агента
//! (задача 10) и инструменту MCP `memory_recall` (задача 9).
//!
//! ```text
//! POST /recall {event, session_id?, solution?, area?, prompt?, error?, tool?, budget_tokens?,
//!               agent_block_version?, task?, parent_id?, timeout_ms?, limit?}
//!   → 200 {text, items: [{id, kind, title, certainty, reason, age_days?}], tokens,
//!          rules_tokens, timings, warnings, hooks?, holdout?}
//! ```
//!
//! - `text` — готовый Markdown для `additionalContext`; пусто — тишина. Каждая подсказка — строка
//!   `ZeroAIMemory: <заголовок> [<вид>·<cert>] i:<id>` (M6 §4.3; у заметки — `n:<id>`, у
//!   changelog — `c:<id>`), под ней с отступом может идти пояснение (L1 правила, фрагмент).
//!   `items` — показанные подсказки по порядку: `id` — та же ссылка, `kind` — `insight`,
//!   `changelog`, `note` или `folder`, `reason` — откуда подсказка (`rules`, `solution`,
//!   `gotcha`, `failure`, `revisit`, `changelog`, `search`, `error_regex`, `error_fts`; у идей —
//!   `idea` и `idea_review`),
//!   `age_days` — возраст записи в сутках (M4.5 §7; нет у строк окружения, файлов, терминов и
//!   триггеров `error_regex`: время там неизвестно); `tokens` — размер `text` (счётчик
//!   выдачи), `rules_tokens` — размер раздела-резерва Rules (M6 §8, §17 п. 3; 0 — резерва нет),
//!   `timings {collect_ms, record_ms, total_ms}`;
//! - **контрольная группа A/B** (M6 §12): сессия с `holdout` (своим, родителя `parent_id`
//!   строки сессии или `parent_id` тела — [`zam_store::session_holdout`]) получает у хуков
//!   пустой `text`, `items: []`, `tokens: 0` и поле `holdout: true`; сбор идёт как обычно (время
//!   ответа сравнимо с основной группой), показ не отмечается (`session_seen`, `usage`), учёт —
//!   вызов события с 0 токенов. Вызов из MCP (агент зовёт сам) — как обычно;
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
//!   `reason: task`; за ними — строка `Открыто задач: N — «…», «…», «…»` (M6.6 §4.2: `N` — все
//!   живые `open` решения и Knowledge, до [`OPEN_TASKS_TITLES`] заголовков незахваченных,
//!   `priority`, затем новые; не подсказка записи; раздел появляется и без строк задач; снимается
//!   пределом канала раньше обязательных строк), `### Ответы человека` (M4.5 §4: вопросы,
//!   отвеченные за [`ANSWERS_DAYS`]
//!   суток от начала текущих суток UTC и ещё не показанные этой сессии — `session_seen`; строка
//!   `ZeroAIMemory: ответ на «<вопрос>»: <ответ> — q:<id> → i:<решение>`, `reason: answer`) и
//!   `### Открытые вопросы` (`N открытых: «…», «…», «…»`, до [`OPEN_QUESTIONS_TITLES`]
//!   заголовков, не подсказка записи), `### Окружение` (M4.5 §8: до [`ENVIRONMENT_MAX`] записей
//!   Vault `host`/`environment`, `<имя> — <purpose>; <address>; ограничения: … — v:<id>`, части —
//!   без хвостовых `.`, `;`, `,` (M6.6 §4.3); без учёта показа; в `recovery` и `brief` нет),
//!   Rules (текст каждой заметки, до
//!   [`RULES_MAX`] заметок, текст — до [`RULE_L1_TOKENS`] токенов), «L1 решения» — корневые узлы
//!   пространства решения, строка на корень — путь и L0 до [`SOLUTION_ROOT_L0_TOKENS`] (до
//!   [`SOLUTION_ROOT_MAX`]), действующие (`active`) `gotcha`/`failure` решения и Knowledge
//!   (M6.6 §4.1: ловушки решения — первыми, одно место из `recall.digest_entries` — самой новой
//!   ловушке Knowledge, [`DIGEST_KNOWLEDGE_TRAPS`]; внутри области — новые первыми; строка —
//!   заголовок, без фрагмента), записи `revisit` и последние записи
//!   changelog решения — индексы `<ссылка> «<заголовок>» · Nt` (M6 §8; разделов — до
//!   `recall.digest_entries`); сразу после правил — строка
//!   `Словарь: N терминов — memory_search kinds=[term]` (N > 0, термины решения и Knowledge;
//!   слово согласовано с числом — «1 термин», «74 термина») и
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
//!   раз за сессию (`session_seen`); словарь — из кэша [`crate::glossary::GlossaryCache`]; за
//!   подсказками поиска — до [`TASK_HINTS_MAX`] подсказки-задачи (M6.6 §3.4: заголовок открытой
//!   задачи решения или Knowledge покрыт промптом — `ZeroAIMemory: задача «…» [<статус>] k:<id>`,
//!   `reason: task`). Тишина (M6.6 §3): пересланное сообщение агента
//!   ([`zam_core::text::is_relayed_message`]) — без строк словаря; запрос без предметных слов
//!   ([`zam_search::recall::subject_terms`] пуст: «согласен», «продолжай») — только строки
//!   словаря; короткий запрос (до [`PROMPT_SHORT_WORDS`] предметных слов) — подсказка поиска
//!   только с лексическим совпадением (канал `fts`, `alias` или `trigram`). Последней — строка
//!   очереди разбора идей (M6.6 §3.5, режим `review` ниже) — и к запросу без предметных слов.
//!   Подробно — подмодуль `prompt`;
//! - **идеи** (M5.7 §6; `[ideas] proactive` сервера и `ideas.proactive` пользователя): в
//!   дайджесте `session_start` — раздел `### Идеи` сразу после строки открытых вопросов
//!   ([`digest_ideas`]: строка-указание и до `ideas.digest_max` строк
//!   `ZeroAIMemory: идея «…» [<priority>] — w:<id>`, `reason: idea`; только пространство
//!   решения и только первый `session_start` сессии; нет в `recovery` и `brief`); к `prompt` —
//!   до `ideas.prompt_max` строк отдельного лёгкого поиска идей после основного
//!   ([`prompt_ideas`]; общие идеи Knowledge — [`knowledge_ideas_related`]; лёгкий поиск —
//!   [`idea_light_search`], M6 §16: каналы FTS до `max(4 × top_k, 20)`, без сниппетов; слова
//!   запроса на стоп-основу [`IDEA_STOP_STEMS`] идею не подтверждают). Кандидаты —
//!   [`zam_store::idea_candidate`]: «от агента», `proposed`, `declined`, `done`, `deferred` до
//!   `horizon` и `accepted` с живой задачей не предлагаются никогда; `accepted` со снятой или
//!   удалённой задачей (§16) — после `new`/`deferred`, строка с пометкой «задача снята|удалена,
//!   можно предложить вернуть»; показ — `session_seen`, раз за сессию;
//! - **режим идей `review`** (M6.5 §7; `ideas.mode` владельца базы решения — в общем решении
//!   его, а не участника; по умолчанию `[ideas] default_mode` = `review`, `propose` — всё как в
//!   M5.7 выше): на месте `### Идеи` — раздел [`SECTION_IDEAS_REVIEW`] со строкой
//!   «Непросмотренных идей: N…» ([`IDEAS_REVIEW_HINT`]) и до `ideas.digest_max` строк: очередь
//!   разбора [`zam_store::review_queue`] (идеи человека `new`, `high` первыми, затем самые старые;
//!   `reason: idea_review`), за ней кандидаты предложения M5.7 (`deferred` с наступившим
//!   `horizon`, `accepted` со снятой задачей) с указанием `action=propose` в строке; очереди нет —
//!   раздел M5.7 из них одних. Подсказка к `prompt` у идеи решения `new` — «к задаче подходит
//!   идея на разбор w:<id>» (`reason: idea_review`); у владельца базы решения ответ `prompt`
//!   несёт и строку очереди разбора (M6.6 §3.5): первая идея очереди, не показанная в текущей
//!   эпохе сессии в её текущей ревизии (дайджестом той же эпохи, пакетом, прежней подсказкой) и
//!   не в подсказках-идеях этого ответа, — `ZeroAIMemory: новая идея на разбор «…» w:<id>` и
//!   ` (ещё N)` при нескольких, одна на ответ, без зависимости от близости к запросу; голова
//!   очереди читается до поиска. Строка плана [`PLAN_LINE`] — сразу за
//!   задачами дайджеста и пакета восстановления: пункты плана из идей (задачи `open |
//!   in_progress | blocked` с ребром `derived_from` на идею) и следующий — только `open`, по
//!   `data.priority`, затем самый старый ([`zam_store::planned_from_ideas`]; открытых нет —
//!   [`PLAN_LINE_NO_NEXT`] без «следующая»); нет пунктов — строки нет. Идеи
//!   выключены (`ideas.proactive`) — ни раздела, ни подсказок, ни строки плана;
//! - **пакет разбора идей `idea_review`** (M6.5 §7; MCP `memory_recall`, `zam idea queue`;
//!   явный вызов: без гейта `hooks.<тип>` и `ideas.proactive`, без контрольной группы A/B, не
//!   фильтруется показанным, показ отмечается, `usage.source = agent`): до `limit` (1..=
//!   [`IDEA_REVIEW_LIMIT_MAX`], по умолчанию [`IDEA_REVIEW_LIMIT`]) идей очереди — заголовок,
//!   приоритет, тело, история (прежние разборы и доводы человека), до
//!   [`IDEA_REVIEW_SIMILAR_MAX`] похожих записей решения и строка шагов разбора
//!   ([`review_pack`]); бюджет — `budget_tokens` или [`IDEA_REVIEW_BUDGET`], срок по умолчанию —
//!   `max(recall.max_timeout_ms, recall.timeout_ms)`; в конце — «Показано идей на разбор: k из
//!   N.» и, если идеи не вошли в бюджет, строка «не показано — идей на разбор: M (…)»; режим
//!   `propose`, нет права записи, нет решения или пустая очередь — одна строка-уведомление
//!   (решение передано, но его нет в области, — «нет доступа» или «не зарегистрировано», M6.6
//!   §5.2);
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
//! - **срок сбора**: у хука — бюджет сервера `recall.timeout_ms` (200), у явного вызова —
//!   `max(recall.max_timeout_ms, recall.timeout_ms)` (по умолчанию 2000; `collect_timeout`,
//!   M6.6 §5.1). Хук — запрос без происхождения MCP с событием `session_start | prompt |
//!   post_tool` или с полем `code_map` (хуки `session-start`, `subagent-start`); явные — MCP
//!   (все события) и `recovery`, `brief`, `idea_review` из REST и `zam` без `code_map`. Шаги
//!   сбора идут до срока минус [`ACCOUNTING_RESERVE_MS`] (40 мс, не больше половины срока), по
//!   превышении отдаётся собранное и `warnings: ["recall timeout"]`; учёт показов — после сбора и
//!   только до полного срока: вышел — собранное отдаётся без `session_seen` и `usage`
//!   (`timings.record_ms: null`) и с тем же предупреждением. Поле тела `timeout_ms` (M6 §16 п. 19,
//!   CR-7) задаёт срок этого запроса вместо умолчания: 1..=`max(recall.max_timeout_ms,
//!   recall.timeout_ms)`, иначе 400. Сбой внутреннего поиска `prompt` — тишина и
//!   `warnings: ["recall search failed"]`;
//! - `record: false` (по умолчанию `true`) — ответ без учёта: ни `session_seen`, ни `usage`,
//!   `timings.record_ms: null` (хук `session-start` с выключенным в клиенте дайджестом берёт
//!   только `hooks`; задача 10, fix round 2);
//! - **эпоха `session_seen`** (M6 §7.2): `session_start` с `source` `compact | resume | clear`
//!   и `event: recovery` начинают новую эпоху сессии ([`EPOCH_SOURCES`], `bump_epoch`) до сбора:
//!   выданное в прежних эпохах больше не считается показанным (повторы подсказок, ответы на
//!   вопросы), а показ этого ответа отмечается в новой. Не зависит от `record` и настроек
//!   подсказок: эпоха — состояние контекста агента. Показ идей — без эпохи: раздел `### Идеи` и
//!   подсказки-идеи `prompt` видят показ любой эпохи; кроме строки очереди разбора к `prompt`
//!   (M6.6 §3.5): она — по текущей эпохе и ревизии идеи, после сжатия голова очереди приходит
//!   снова (пакет восстановления раздела идей не несёт). Сбой — WARN, эпоха прежняя, и этот
//!   ответ ничего не прячет как показанное (фильтра повторов текущей эпохи нет): лучше
//!   повторить, чем потерять выданное до сжатия;
//! - **пакет восстановления** (M4.5 §9.2) — `event: recovery` (MCP, агенты без
//!   `SessionStart compact`) или `session_start` с `source` `compact | resume`: бюджет по
//!   умолчанию `recall.recovery_tokens` (2000), разделы по порядку — `### Задачи` (захваченные
//!   сессией первыми, строка открытых задач — как в дайджесте), `### Ответы человека` **без**
//!   фильтра `session_seen` (контекст агента
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
//!   ловушки решения и Knowledge (порядок дайджеста на [`BRIEF_ENTRIES_MAX`] мест), `### Задачи`
//!   (захваченные `parent_id` или сессией — первыми; без строки открытых задач).
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
//!   recovery | brief | idea_review` (иначе 400; `pre_tool` — только локально в хуке, §10.3).
//!
//! Маршрут — служебный, рядом с `/search` и `/read`: `/recall` пишет только `usage` и
//! `session_seen`, а не память, поэтому `memory_writes` не растёт (решение контроллера R12).
//!
//! Код идей — раздел дайджеста, подсказки к `prompt`, строка плана и пакет `idea_review` — в
//! подмодуле `ideas` (M6.6 §11.1); подсказки к запросу `prompt` — разделы ответа, правила тишины
//! и подсказка-задача — в подмодуле `prompt` (M6.6 §3).
//!
//! [`knowledge_ideas_related`]: ideas::knowledge_ideas_related
//! [`idea_light_search`]: ideas::idea_light_search
//! [`prompt_ideas`]: ideas::prompt_ideas
mod ideas;
mod prompt;

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
    CoverageQuery, CoverageRule, HINT_PREFIX, Section, error_regex_matches, error_terms, hint_line,
    normalize_error, one_line, pack_sections, reserved_text,
};
use zam_search::truncate_marked;
use zam_store::{
    Certainty, Entry, EntryFilter, EntryKind, EntryStatus, FtsQuery, NodeKind, RECALL_PART,
    SeenItem, SeenKey, SpaceKind, StoreError, TargetFilter, TokenCount, TriggerKind, UsageEvent,
    UsageKind, UsageSource, active_triggers, bump_epoch, count_tokens, list_children, list_entries,
    mark_seen_with_tokens, node_content, record_usages, search_fts_channels, seen_revs,
    seen_revs_any_epoch, session_holdout, touch_session,
};

use super::json_body;
use super::search::targets::{ScopeReq, SearchTarget, dedup_keep_order, resolve_targets};
use super::search::{Health, LightSearch, SOLUTION_HEADER};
use super::sessions::{McpOrigin, RequestSession, explicit_only, seen_session};
use super::user_settings::{UserFlags, cached_user_flags};
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, SESSION_HEADER};
use crate::auth::{Principal, target_user_id};
use crate::config::{IdeasMode, RecallSection};
use crate::error::ApiError;

// имена идей — по прежним путям `api::recall::…` (MCP, тесты)
pub use ideas::{
    IDEA_REVIEW_BUDGET, IDEA_REVIEW_EMPTY, IDEA_REVIEW_LIMIT, IDEA_REVIEW_LIMIT_MAX,
    IDEA_REVIEW_NO_ACCESS, IDEA_REVIEW_NO_SOLUTION, IDEA_REVIEW_OFF, IDEA_REVIEW_READ_ONLY,
    IDEA_REVIEW_SIMILAR_MAX, IDEA_REVIEW_STEPS, IDEA_REVIEW_UNREGISTERED, IDEA_STOP_STEMS,
    IDEAS_REVIEW_HINT, PLAN_LINE, PLAN_LINE_NO_NEXT, SECTION_IDEAS_REVIEW,
};
use ideas::{
    REASON_IDEA, REASON_IDEA_REVIEW, digest_ideas, ideas_enabled, ideas_section_head, plan_line,
    review_pack, review_shown_line, solution_ideas_mode,
};
#[cfg(feature = "test-hooks")]
pub use ideas::{
    TEST_IDEA_LIGHT_OFF, TEST_IDEA_SEARCH_TIMES, TEST_SECOND_PASS_QUERIES, TEST_SLOW_IDEA_MARK,
    TEST_SLOW_QUEUE_MARK,
};
pub use prompt::TASK_HINTS_MAX;
use prompt::prompt_sections;
// порог короткого запроса — один на сервер и модель калибровки xtask (`zam_search::recall`);
// здесь — по пути `api::recall::…`, как `PROMPT_MAX` и `TERM_HINTS_MAX`
pub use zam_search::recall::PROMPT_SHORT_WORDS;

/// Предупреждение: сбор подсказок не уложился в срок (у хука — `recall.timeout_ms`, у явного
/// вызова — `max(recall.max_timeout_ms, recall.timeout_ms)`).
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
/// `reason` задачи: раздел `### Задачи` дайджеста и пакетов, подсказка-задача к `prompt`
/// (M6.6 §3.4).
const REASON_TASK: &str = "task";
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
    /// Пакет разбора идей (M6.5 §7): очередь идей `new` с похожими записями — MCP и
    /// `zam idea queue`.
    IdeaReview,
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
            Event::Recovery | Event::Brief | Event::IdeaReview => None,
        }
    }

    /// Источник показа в `usage`: у пакета восстановления — `session_start` (так с задачи 9),
    /// у пакета под задачу — `brief` (в `usage.source` нет CHECK, миграция не нужна), у пакета
    /// разбора идей — `agent`: его зовёт сам агент, не хук.
    fn source(self) -> UsageSource {
        match self {
            Event::SessionStart | Event::Recovery => UsageSource::SessionStart,
            Event::Prompt => UsageSource::Prompt,
            Event::PostTool => UsageSource::PostTool,
            Event::Brief => UsageSource::Brief,
            Event::IdeaReview => UsageSource::Agent,
        }
    }
}

/// Предел поля `timeout_ms` и срок явного вызова, мс: `max(recall.max_timeout_ms,
/// recall.timeout_ms)` — не меньше срока сервера: при `recall.timeout_ms` больше `max_timeout_ms`
/// (сетки замера ставят 60000) клиент может попросить и сам срок сервера (ревью задачи 19 M6, M-3).
fn timeout_field_max(cfg: &RecallSection) -> u64 {
    cfg.max_timeout_ms.max(cfg.timeout_ms)
}

/// Срок сбора запроса, мс (M6.6 §5.1, замечание 20): поле `timeout_ms` (`field`) важнее; без него
/// у хука — `recall.timeout_ms` (бюджет хуков), у явного вызова — [`timeout_field_max`]. Хук —
/// запрос без происхождения MCP (`mcp`) с событием `session_start | prompt | post_tool` или с полем
/// `code_map` (его шлют хуки `session-start` и `subagent-start`). Явные — все вызовы MCP и
/// `recovery`, `brief`, `idea_review` из REST и `zam` без `code_map`: агент ждёт пакет сам, а
/// поиск `brief` на CPU в 200 мс не укладывался.
fn collect_timeout(
    field: Option<u64>,
    event: Event,
    code_map: bool,
    mcp: bool,
    cfg: &RecallSection,
) -> u64 {
    field.unwrap_or_else(|| {
        let hook = !mcp
            && (code_map || matches!(event, Event::SessionStart | Event::Prompt | Event::PostTool));
        if hook {
            cfg.timeout_ms
        } else {
            timeout_field_max(cfg)
        }
    })
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
        Event::IdeaReview => "idea_review",
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
    /// Срок сбора этого запроса, мс (M6 §16 п. 19, CR-7): 1..=`max(recall.max_timeout_ms,
    /// recall.timeout_ms)`, иначе 400; нет — у хука `recall.timeout_ms`, у явного вызова (MCP;
    /// `recovery`, `brief`, `idea_review` без `code_map`) — верхний предел ([`collect_timeout`],
    /// M6.6 §5.1).
    timeout_ms: Option<u64>,
    /// Идей в пакете `idea_review` (M6.5 §7): 1..=[`IDEA_REVIEW_LIMIT_MAX`], иначе 400; нет —
    /// [`IDEA_REVIEW_LIMIT`]. У прочих событий не действует.
    limit: Option<u32>,
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

/// Срок сбора подсказок ([`collect_timeout`]).
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

/// Обход баз области (ревью задачи 3, M6): живые цели (`health.alive`) с непустым списком
/// пространств `spaces_of(i, цель)` — `(i, база, пространства)` по порядку целей. Живость
/// берётся на момент вызова: шаг обхода (`health.soft`) исключает только свою цель, так что
/// это то же, что проверка на каждом шаге.
fn live_targets(
    health: &Health<'_>,
    spaces_of: impl Fn(usize, &SearchTarget) -> Vec<Id>,
) -> Vec<(usize, zam_store::Db, Vec<Id>)> {
    health
        .targets()
        .iter()
        .enumerate()
        .filter(|(i, _)| health.alive[*i])
        .filter_map(|(i, t)| {
            let ids = spaces_of(i, t);
            (!ids.is_empty()).then(|| (i, t.db.clone(), ids))
        })
        .collect()
}

/// Пространства цели `i` из списка `(цель, пространство)`.
fn spaces_of(spaces: &[(usize, Id)], i: usize) -> Vec<Id> {
    spaces
        .iter()
        .filter(|(t, _)| *t == i)
        .map(|(_, s)| *s)
        .collect()
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
        from_idea: None,
    }
}

/// Предел `next_step` и `blocker` в строке задачи дайджеста, символов.
const TASK_LINE_TEXT_CHARS: usize = 200;
/// Давность ответа на вопрос для `### Ответы человека`, дней (M4.5 §4).
pub const ANSWERS_DAYS: i64 = 14;
/// Заголовков открытых вопросов в дайджесте.
pub const OPEN_QUESTIONS_TITLES: usize = 3;
/// Заголовков в строке открытых задач `Открыто задач: N — «…», …` раздела `### Задачи`
/// (M6.6 §4.2, решение координатора 7).
pub const OPEN_TASKS_TITLES: usize = 3;
/// Предел заголовка в строке открытых задач, символов.
const OPEN_TASK_TITLE_CHARS: usize = 80;
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

/// Текст агенту без секретов и до `max` символов: маска — до обрезки (ревью задачи 3, M4),
/// иначе ключ, разрезанный границей, маска не узнала бы и его начало ушло бы в выдачу.
fn masked_clip(s: &str, max: usize) -> String {
    clip(&zam_markdown::mask_text(s), max)
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
        let n = masked_clip(n, TASK_LINE_TEXT_CHARS);
        s.push_str(&format!(" — далее: {n}"));
    }
    s.push_str(&format!(" — {}", entry_ref(e)));
    if e.status == Some(EntryStatus::Blocked)
        && let Some(b) = &t.blocked_by
    {
        let b = masked_clip(b, TASK_LINE_TEXT_CHARS);
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
            let mut c = entry_cand(*i, e, REASON_TASK, "");
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
    let q = masked_clip(&e.title, TASK_LINE_TEXT_CHARS);
    let ans = masked_clip(&a.brief(&qd.options), TASK_LINE_TEXT_CHARS);
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
    for (i, db, ids) in live_targets(health, |i, _| spaces_of(spaces, i)) {
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
            .map(|(_, t)| format!("«{}»", masked_clip(t, TASK_LINE_TEXT_CHARS)))
            .collect();
        format!("{open_n} открытых: {}", list.join(", "))
    });
    Ok((cands, line))
}

/// Строка открытых задач раздела `### Задачи` (M6.6 §4.2, замечание 28а): `N` — живые задачи
/// `open` пространств `spaces` (решение и Knowledge), заголовки — до [`OPEN_TASKS_TITLES`] задач
/// без живого захвата (захваченные уже строками раздела, [`task_cands`]) по
/// [`zam_store::OpenTask::order_key`]: `priority`, затем новые. От сессии не зависит —
/// детерминизм дайджеста сохраняется. Открытых нет — `None`.
async fn open_tasks_line(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
) -> Result<Option<String>, ApiError> {
    // число и заголовки всех баз: заголовки сливаются по общему ключу порядка
    let (mut n, mut tasks) = (0usize, Vec::<zam_store::OpenTask>::new());
    for (i, db, ids) in live_targets(health, |i, _| spaces_of(spaces, i)) {
        let Some(r) = deadline
            .step(zam_store::open_tasks(&db, &ids, OPEN_TASKS_TITLES))
            .await
        else {
            break;
        };
        if let Some((k, t)) = health.soft(i, r)? {
            n += k;
            tasks.extend(t);
        }
    }
    tasks.sort_by_key(zam_store::OpenTask::order_key);
    let titles: Vec<&str> = tasks
        .iter()
        .take(OPEN_TASKS_TITLES)
        .map(|t| t.title.as_str())
        .collect();
    Ok((n > 0).then(|| open_tasks_text(n, &titles)))
}

/// Текст строки открытых задач: `Открыто задач: N — «…», «…», «…»`; заголовок — без секретов и
/// до [`OPEN_TASK_TITLE_CHARS`] символов ([`masked_clip`]). Заголовков нет (все открытые
/// захвачены) — `Открыто задач: N`.
fn open_tasks_text(n: usize, titles: &[&str]) -> String {
    if titles.is_empty() {
        return format!("Открыто задач: {n}");
    }
    let list: Vec<String> = titles
        .iter()
        .map(|t| format!("«{}»", masked_clip(t, OPEN_TASK_TITLE_CHARS)))
        .collect();
    format!("Открыто задач: {n} — {}", list.join(", "))
}

/// Часть строки окружения без хвостовых `.`, `;`, `,` и пробелов (M6.6 §4.3, замечание 16а):
/// склейка «; » не даёт «стенд.; адрес»; многоточие `…` (в том числе от [`clip`]) остаётся.
fn environment_part(s: &str) -> &str {
    s.trim_end_matches(|c: char| matches!(c, '.' | ';' | ',') || c.is_whitespace())
}

/// Строка окружения: `ZeroAIMemory: <имя> — <purpose>; <address>; ограничения: <limits> —
/// v:<id>`; пустые поля опускаются, хвостовая пунктуация частей срезается
/// ([`environment_part`]). Берёт только открытые поля: секрета и шифротекста здесь нет.
fn environment_block(m: &zam_store::VaultMeta) -> String {
    let field = |k: &str| {
        m.data
            .get(k)
            .and_then(Value::as_str)
            .map(|v| masked_clip(v, TASK_LINE_TEXT_CHARS))
            .map(|v| environment_part(&v).to_string())
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
/// `plan` — строка плана из идей ([`plan_line`]; режим `review`, идеи включены).
#[allow(clippy::too_many_arguments)]
async fn digest(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    recovery: bool,
    code_on: bool,
    plan: bool,
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
    // открытые задачи (M6.6 §4.2) — строкой раздела задач, в дайджесте и пакете восстановления
    let open_tasks = open_tasks_line(health, deadline, &insight_spaces).await?;
    // строка плана из идей (M6.5 §7) — за задачами
    let plan_line = if plan {
        plan_line(p, health, deadline, sol).await
    } else {
        None
    };
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
    // снимок сессии (M6 §8а): раздел выводится после ответов и идей (в пакете восстановления —
    // после решений и уведомлений), [`arrange`]
    let snapshot = snapshot_part(
        st,
        req,
        headers,
        targets,
        // задачи снимка — только решения запроса: личное Knowledge общее для всех решений
        &sol.into_iter().collect::<Vec<_>>(),
        session.as_deref(),
        recovery,
        user,
        deadline,
    )
    .await;

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
    let entries = st.cfg.recall.digest_entries;
    let traps = trap_cands(health, deadline, &insight_spaces, sol, entries).await?;
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
        open_tasks,
        plan_line,
        // длину очереди разбора ставит [`recall`] вместе с идеями дайджеста
        ideas_queue: 0,
        after_rules,
        code_line,
        code_map,
        rules_short,
        rules_hidden,
        rules_cut,
        snapshot,
    };
    Ok((lines, sections))
}

/// Раздел «Последнее известное состояние» (M6 §8а, [`super::snapshot::last_state`]): дайджест —
/// только в решении запроса и только снимок оборванной сессии; сбой чтения — WARN и без раздела (дайджест не валится), срок сбора
/// вышел — без раздела.
#[allow(clippy::too_many_arguments)]
async fn snapshot_part(
    st: &AppState,
    req: &RecallReq,
    headers: &HeaderMap,
    targets: &[SearchTarget],
    spaces: &[(usize, Id)],
    session: Option<&str>,
    recovery: bool,
    user: Id,
    deadline: &mut Deadline,
) -> Option<super::snapshot::Shown> {
    if st.cfg.recall.snapshot_tokens == 0 {
        return None;
    }
    let guid = scoped_solution(targets, req, headers);
    if guid.is_none() && !recovery {
        return None;
    }
    let own = match st.users.get(user).await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "recall: база сессий для снимка недоступна");
            return None;
        }
    };
    let mut by_target: Vec<(usize, Vec<Id>)> = Vec::new();
    for &(i, sp) in spaces {
        match by_target.iter_mut().find(|(t, _)| *t == i) {
            Some((_, v)) => v.push(sp),
            None => by_target.push((i, vec![sp])),
        }
    }
    let q = super::snapshot::ShowReq {
        own: &own,
        solution: guid.as_ref().map_or("", |g| g.as_str()),
        session,
        task_dbs: by_target
            .into_iter()
            .map(|(i, v)| (targets[i].db.clone(), v))
            .collect(),
        recovery,
        now: zam_store::time::now(),
        cfg: &st.cfg.recall,
    };
    match deadline.step(super::snapshot::last_state(q)).await? {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "recall: снимок сессии не прочитан");
            None
        }
    }
}

/// Мест ловушек Knowledge среди первых `entries` мест раздела ловушек (M6.6 §4.1, решение
/// координатора 6): ловушки проекта — первыми, но общие ловушки не пропадают совсем.
pub const DIGEST_KNOWLEDGE_TRAPS: usize = 1;

/// Порядок ловушек (M6.6 §4.1, замечание 27): ловушки решения `sol` (новые первыми) занимают
/// первые `entries − DIGEST_KNOWLEDGE_TRAPS` мест, следующие — самые новые ловушки Knowledge
/// `kn`, затем остальные решения и остальные Knowledge: `sol[..k] ++ kn[..1] ++ sol[k..] ++
/// kn[1..]`. Ловушек решения меньше `k` — их места занимает Knowledge, как прежде. Первое
/// место — всегда ловушке решения, если она есть (и при `entries = 1`, **[решение
/// исполнителя, задача 3]**): ради неё правка и делалась, и её держит [`KEEP_FIRST_TRAP`].
fn trap_order<T>(sol: Vec<T>, kn: Vec<T>, entries: usize) -> Vec<T> {
    let k = entries.saturating_sub(DIGEST_KNOWLEDGE_TRAPS).max(1);
    let mut out = Vec::with_capacity(sol.len() + kn.len());
    let (mut sol, mut kn) = (sol.into_iter(), kn.into_iter());
    out.extend(sol.by_ref().take(k));
    out.extend(kn.by_ref().take(DIGEST_KNOWLEDGE_TRAPS));
    out.extend(sol);
    out.extend(kn);
    out
}

/// Действующие `gotcha`/`failure` пространств `spaces` (решение `sol` и Knowledge) в порядке
/// [`trap_order`] под `entries` мест раздела; внутри области — новые первыми.
async fn trap_cands(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
    sol: Option<(usize, Id)>,
    entries: usize,
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
    let (own, kn): (Vec<_>, Vec<_>) = traps
        .into_iter()
        .partition(|(i, e)| sol == Some((*i, e.space_id)));
    Ok(trap_order(own, kn, entries)
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
    let sol = solution_space(targets);
    let mut insight_spaces: Vec<(usize, Id)> = sol.into_iter().collect();
    insight_spaces.extend(personal_space(targets, SpaceKind::Knowledge));
    let mut sections: Vec<(Option<&'static str>, Vec<Cand>)> = Vec::new();
    sections.push((Some(SECTION_RULES), rules_l0(health, deadline).await?));
    let Some(task) = task else {
        // ловушки мини-дайджеста — то же правило под его [`BRIEF_ENTRIES_MAX`] мест
        let traps = trap_cands(health, deadline, &insight_spaces, sol, BRIEF_ENTRIES_MAX).await?;
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
    // термины — по заданию без обрезки по длине: `find_in_head` сам берёт первые
    // BRIEF_TASK_MAX_CHARS символов, а маску кода строит и за краем (ревью задачи 5, M1)
    let whole = req.task.as_deref().map_or(task, str::trim);
    let terms = term_cands(st, whole, BRIEF_TASK_MAX_CHARS, health, deadline).await;
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
    let Some(v) = inner_search(st, p, headers, area, body, None, health, deadline).await? else {
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
    for (i, db, spaces) in live_targets(health, |_, t| entry_spaces(t)) {
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
/// строка открытых вопросов, строка открытых задач, строки после правил (словарь, проверенные
/// команды, строка карты кода) и мини-карта кода `### Карта кода`.
#[derive(Default)]
struct Lines {
    notices: Vec<String>,
    open_line: Option<String>,
    /// Строка открытых задач ([`open_tasks_line`]): в разделе задач, до строки плана.
    open_tasks: Option<String>,
    /// Строка плана из идей ([`plan_line`]): за разделом задач.
    plan_line: Option<String>,
    /// Длина очереди разбора (строка [`IDEAS_REVIEW_HINT`]); 0 — очереди нет.
    ideas_queue: usize,
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
    /// «Последнее известное состояние» — снимок сессии (M6 §8а, [`super::snapshot`]).
    snapshot: Option<super::snapshot::Shown>,
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
/// и Knowledge области; «1 термин», «74 термина» — [`zam_core::text::plural_ru`], M6.6 §4.3);
/// `None` — терминов нет или срок вышел.
async fn glossary_line(
    st: &AppState,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Option<String> {
    let mut n = 0;
    for (_, db, spaces) in live_targets(health, |_, t| entry_spaces(t)) {
        match deadline.step(st.glossary.index(&db)).await? {
            Ok(idx) => n += idx.count_in(&spaces),
            Err(e) => tracing::warn!(error = %e, "recall: словарь не прочитан"),
        }
    }
    (n > 0).then(|| {
        let word = zam_core::text::plural_ru(n as u64, "термин", "термина", "терминов");
        format!("Словарь: {n} {word} — memory_search kinds=[term]")
    })
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
    for (i, db, spaces) in live_targets(health, |_, t| entry_spaces(t)) {
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
/// `light` — облегчение поиска ([`LightSearch`]; только подсказки-идеи, [`idea_light_search`]),
/// `None` — обычный поиск.
///
/// [`idea_light_search`]: ideas::idea_light_search
#[allow(clippy::too_many_arguments)]
async fn inner_search(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    area: Option<&str>,
    body: Value,
    light: Option<LightSearch>,
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
    let search = super::search::search_with(
        State(st.clone()),
        p.clone(),
        h,
        None,
        Ok(Bytes::from(body.to_string())),
        light,
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
/// словаря, подсказки-задачи `prompt` ([`TASK_HINTS_MAX`], M6.6 §3.4), идеи или общий предел
/// события `limit`.
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
    // пакет разбора идей: очередь уже отобрана по `limit` ([`review_pack`])
    if event == Event::IdeaReview {
        return usize::MAX;
    }
    match title {
        Some(SECTION_RULES) => RULES_MAX,
        Some(SECTION_ENVIRONMENT) => ENVIRONMENT_MAX,
        Some("### Решение") => SOLUTION_ROOT_MAX,
        Some(_) => entries,
        None if cands.iter().any(|c| c.reason == REASON_GLOSSARY) => TERM_HINTS_MAX,
        None if cands.iter().any(|c| c.reason == REASON_TASK) => TASK_HINTS_MAX,
        None if cands
            .iter()
            .any(|c| matches!(c.reason, REASON_IDEA | REASON_IDEA_REVIEW)) =>
        {
            ideas_max
        }
        None => limit,
    }
}

/// Раскладка отобранных разделов в разделы упаковки и подсказки каждого (у строк, что не
/// подсказки записей — уведомления, словарь, открытые вопросы, карта кода, — пустой список: ни
/// `session_seen`, ни `usage`). Уведомления — первыми; в пакете восстановления — после задач,
/// ответов и решений (§9.2), а если других разделов нет — в конце. Открытые вопросы — после
/// ответов (в пакете — вместе с уведомлениями), за ними — раздел `### Идеи` (`ideas`,
/// строка-указание — в заголовке [`ideas_section_head`]: она выводится, только если вошла хоть
/// одна идея; есть идея очереди разбора — раздел [`SECTION_IDEAS_REVIEW`] со строкой
/// [`IDEAS_REVIEW_HINT`], M6.5 §7), строки словаря, проверенных команд и карты кода и мини-карта
/// [`SECTION_CODE_MAP`] — после правил. Строка открытых задач — сразу за задачами, с тем же
/// заголовком [`SECTION_TASKS`]: упаковка его не повторяет, а если строки задач не вошли или их
/// нет, заголовок выводится у неё (M6.6 §4.2); за ней — строка плана из идей. Раздел пакета
/// `idea_review` — со строкой шагов разбора [`IDEA_REVIEW_STEPS`] в заголовке.
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
        open_tasks,
        plan_line,
        ideas_queue,
        after_rules,
        code_line,
        code_map,
        rules_hidden,
        rules_cut,
        snapshot,
        ..
    } = lines;
    let mut snapshot = snapshot;
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
        // пакет разбора идей: строка шагов — под заголовком, выводится вместе с ним
        let head = match title {
            Some(SECTION_IDEAS_REVIEW) => {
                Some(format!("{SECTION_IDEAS_REVIEW}\n{IDEA_REVIEW_STEPS}"))
            }
            t => t.map(str::to_string),
        };
        let mut sec = Section::new(head, cands.iter().map(|c| c.block.clone()).collect());
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
        if title == Some(SECTION_TASKS)
            && let Some(l) = &open_tasks
        {
            // строка раздела задач — всегда с его заголовком: вошли строки задач — заголовок не
            // повторяется (`pack_sections` продолжает раздел), не вошли или их нет — он у строки
            // (ревью задачи 3, M2); снимается раньше обязательных строк (M6.6 §4.2)
            out.line(Some(SECTION_TASKS), vec![l.clone()], KEEP_POINTERS);
            out.omit_last(OMIT_OPEN_TASKS);
        }
        if title == Some(SECTION_TASKS)
            && let Some(l) = &plan_line
        {
            out.line(None, vec![l.clone()], KEEP_POINTERS);
            out.omit_last(OMIT_PLAN);
        }
        if title == Some(SECTION_ANSWERS)
            && !recovery
            && let Some(line) = open_line.take()
        {
            out.line(Some(SECTION_OPEN_QUESTIONS), vec![line], KEEP_POINTERS);
            out.omit_last(OMIT_OPEN_QUESTIONS);
        }
        if title == Some(SECTION_ANSWERS) && !recovery && !ideas.is_empty() {
            // очередь разбора (M6.5 §7) — свой заголовок и строка-указание
            let (head, review) = ideas_section_head(&ideas, ideas_queue);
            let mut sec = Section::new(Some(head), ideas.iter().map(|c| c.block.clone()).collect());
            sec.keep = KEEP_MANDATORY;
            sec.omit = Some(if review {
                (OMIT_REVIEW_QUEUE.0.into(), OMIT_REVIEW_QUEUE.1.into())
            } else {
                ("идей".into(), "memory_search kinds=[idea]".into())
            });
            out.packed.push(sec);
            out.picked.push(std::mem::take(&mut ideas));
        }
        // снимок сессии — после ответов и идей; в пакете восстановления — после решений и
        // уведомлений: задачи, ответы, доступы и идеи он не вытесняет (M6 §8а)
        if (title == Some(SECTION_ANSWERS) && !recovery)
            || (title == Some(SECTION_DECISIONS) && recovery)
        {
            if recovery {
                out.notices(&mut notices, &mut open_line);
            }
            if let Some(sn) = snapshot.take() {
                out.line(Some(&sn.title), sn.blocks, 0);
                out.omit_last(OMIT_SNAPSHOT);
            }
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
/// Строки-указатели: открытые задачи и вопросы, строки после правил (словарь, проверенные
/// команды, карта кода) и ловушки; при снятии — с конца, то есть открытые задачи и вопросы —
/// последними.
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

/// Адрес снятой строки снимка сессии: сам снимок — на странице сессии WebUI.
const OMIT_SNAPSHOT: Omit = ("строк снимка сессии", "WebUI «Сессии»");

/// Адрес снятой строки открытых вопросов.
const OMIT_OPEN_QUESTIONS: Omit = ("строк открытых вопросов", "memory_search kinds=[question]");

/// Адрес снятой строки плана из идей.
const OMIT_PLAN: Omit = ("строк плана из идей", "memory_search kinds=[task]");

/// Адрес снятой строки открытых задач (M6.6 §4.2).
const OMIT_OPEN_TASKS: Omit = ("строк открытых задач", "memory_search kinds=[task]");

/// Адрес снятых пределом вывода идей очереди разбора: раздела дайджеста [`SECTION_IDEAS_REVIEW`]
/// и строки очереди к `prompt` (M6.6 §3.5; ревью задачи 14, M2) — пакет разбора.
const OMIT_REVIEW_QUEUE: Omit = ("идей на разбор", "memory_recall event=idea_review");

/// Адрес идей пакета `idea_review`, снятых пределом вывода канала ([`omit_of`]): тот же пакет с
/// меньшим `limit`.
const OMIT_IDEAS_REVIEW: Omit = (
    "идей на разбор",
    "memory_recall event=idea_review с меньшим limit",
);

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
        SECTION_IDEAS_REVIEW => OMIT_IDEAS_REVIEW,
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
    let max_timeout = timeout_field_max(&st.cfg.recall);
    if req
        .timeout_ms
        .is_some_and(|t| !(1..=max_timeout).contains(&t))
    {
        return Err(ApiError::BadRequest(format!(
            "timeout_ms: от 1 до {max_timeout} (max(recall.max_timeout_ms, recall.timeout_ms))"
        )));
    }
    if req.event == Event::IdeaReview
        && req
            .limit
            .is_some_and(|l| !(1..=IDEA_REVIEW_LIMIT_MAX).contains(&l))
    {
        return Err(ApiError::BadRequest(format!(
            "limit: от 1 до {IDEA_REVIEW_LIMIT_MAX}"
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
    // контрольная группа агентного A/B (M6 §12): хукам — пустой `text` (сбор идёт как обычно,
    // учёт — вызов без токенов); инструмент MCP агент зовёт сам — ему как обычно. Сбой чтения
    // — WARN, не контрольная группа
    // пакет разбора идей — явный вызов агента или `zam idea queue`, не подсказка хука: без A/B
    let holdout = match (&session, origin.is_none() && req.event != Event::IdeaReview) {
        (Some(s), true) => session_holdout(&s.db, &s.touch.id, req.parent_id.as_deref())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "recall: признак holdout сессии не прочитан");
                false
            }),
        _ => false,
    };
    let user = target_user_id(&st, &p, &headers)?;
    // R5: настройки пользователя (`hooks.<тип>`, `ideas.proactive`) одним чтением (кэш процесса,
    // M6.6 §5.3); сбой чтения — всё включено (подсказки не валятся)
    let flags = match cached_user_flags(&st, user).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "recall: настройки пользователя не прочитаны");
            UserFlags::defaults(st.cfg.ideas.default_mode)
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
    } else if req.event == Event::IdeaReview {
        IDEA_REVIEW_BUDGET
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
    // сбор — до срока минус резерв на учёт показа; учёт — только до полного срока; срок — из
    // тела (нативный клиент, CR-7), у хука — `recall.timeout_ms`, у явного вызова — предел поля
    let timeout = collect_timeout(
        req.timeout_ms,
        req.event,
        req.code_map.is_some(),
        origin.is_some(),
        &st.cfg.recall,
    );
    let total_deadline = t0 + Duration::from_millis(timeout);
    let mut deadline = Deadline {
        at: t0 + Duration::from_millis(timeout - ACCOUNTING_RESERVE_MS.min(timeout / 2)),
        expired: false,
    };
    // тестовая задержка шага сбора под сроком (CR-7): только с фичей test-hooks
    #[cfg(feature = "test-hooks")]
    {
        let ms = st.recall_delay_ms.load(std::sync::atomic::Ordering::SeqCst);
        if ms > 0 {
            let _ = deadline
                .step(tokio::time::sleep(Duration::from_millis(ms)))
                .await;
        }
    }

    // сбор кандидатов
    let mut code_section: Vec<String> = Vec::new();
    // идеи (M5.7 §6): раздел дайджеста и подсказка `prompt`, `server && user`
    let ideas_on = enabled
        && matches!(req.event, Event::SessionStart | Event::Prompt)
        && !recovery
        && ideas_enabled(&st, &flags);
    // режим идей (M6.5 §7) — владельца базы решения, одним чтением на запрос и только когда
    // нужен: раздел и подсказки идей, строка плана (и в пакете восстановления), пакет разбора
    let plan_on = enabled
        && matches!(req.event, Event::SessionStart | Event::Recovery)
        && ideas_enabled(&st, &flags);
    let mode = if ideas_on || plan_on || req.event == Event::IdeaReview {
        solution_ideas_mode(&st, &targets, user, &flags).await
    } else {
        flags.ideas_mode
    };
    let review = mode == IdeasMode::Review;
    let mut idea_cands: Vec<Cand> = Vec::new();
    // строка очереди разбора к `prompt` (M6.6 §3.5): свой отбор показанного — по эпохе и
    // ревизии ([`ideas::review_queue_head`]), а не общий фильтр идей «любая эпоха»
    let mut queue_cands: Vec<Cand> = Vec::new();
    let (mut lines, sections, repeatable) = match req.event {
        _ if !enabled => (Lines::default(), Vec::new(), true),
        // дайджест повторяется: после `/compact` и `resume` Claude Code шлёт тот же
        // `session_id`, а контекст агента уже потерян (показ всё равно отмечается)
        Event::SessionStart | Event::Recovery => {
            let (mut l, s) = digest(
                &st,
                &p,
                &sh,
                &req,
                recovery,
                code_on,
                plan_on && review,
                &mut health,
                &mut deadline,
            )
            .await?;
            if ideas_on {
                let (c, n) =
                    digest_ideas(&st, &p, session.as_ref(), &health, &mut deadline, review).await;
                idea_cands = c;
                l.ideas_queue = n;
            }
            (l, s, true)
        }
        // словарь, поиск, задача, идеи — у каждого раздела свой предел (подмодуль `prompt`)
        Event::Prompt => {
            let s = session.as_ref();
            let (h, d) = (&mut health, &mut deadline);
            let (secs, queue) =
                prompt_sections(&st, &p, &sh, &req, s, ideas_on, review, h, d).await?;
            queue_cands = queue;
            (Lines::default(), secs, false)
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
        // пакет разбора идей повторяется: агент зовёт его сам (показ отмечается)
        Event::IdeaReview => {
            let (l, c) = review_pack(&st, &p, &sh, &req, mode, &mut health, &mut deadline).await?;
            (l, vec![(Some(SECTION_IDEAS_REVIEW), c)], true)
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
        Event::SessionStart | Event::Recovery | Event::Brief | Event::IdeaReview => usize::MAX,
        Event::Prompt => PROMPT_MAX,
        Event::PostTool => POST_TOOL_MAX,
    };
    let ideas_max = st.cfg.ideas.prompt_max as usize;
    let entries = st.cfg.recall.digest_entries;
    let mut dup: HashSet<TargetRef> = HashSet::new();
    let mut chosen: Vec<(Option<&'static str>, Vec<&Cand>)> = sections
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
    // строка очереди — последним разделом `prompt`, показанное уже отсеяно своим правилом
    if !queue_cands.is_empty() {
        let per = section_limit(req.event, None, &queue_cands, limit, ideas_max, entries);
        let queue: Vec<&Cand> = queue_cands
            .iter()
            .filter(|c| health.alive[c.target])
            .filter(|c| dup.insert(c.tref))
            .take(per)
            .collect();
        chosen.push((None, queue));
    }
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
    // пакет разбора идей: длина очереди и собранные идеи — для строки «показано k из N»
    let review_total = lines.ideas_queue;
    let review_picked = if req.event == Event::IdeaReview {
        chosen.iter().map(|(_, c)| c.len()).sum()
    } else {
        0
    };
    let (mut packed, mut picked) = arrange(recovery, rules_reserved, lines, ideas, chosen);
    // строка очереди, снятая пределом вывода канала, — с адресом очереди, а не общим «строк:
    // 1 (memory_recall)» (ревью задачи 14, M2)
    let is_queue = |c: &Cand| queue_cands.iter().any(|q| std::ptr::eq(c, q));
    if let Some(si) = picked.iter().position(|p| p.iter().any(|c| is_queue(c))) {
        packed[si].omit = Some((OMIT_REVIEW_QUEUE.0.into(), OMIT_REVIEW_QUEUE.1.into()));
    }
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
    let mut text = text;
    if review_picked > 0 {
        let shown = shown_now
            .iter()
            .filter(|c| c.reason == REASON_IDEA_REVIEW)
            .count();
        let tail = review_shown_line(
            shown,
            review_picked,
            review_total,
            limit.is_some(),
            !text.is_empty(),
        );
        // под пределом вывода клиента (ревью задачи 4, раунд 2) — только если строка в него
        // помещается: предел важнее справочной строки
        let next = format!("{text}{tail}");
        if limit.is_none_or(|l| l.fits(&next)) {
            text = next;
        }
    }
    // контрольная группа (M6 §12): ничего не выдано — ни текста, ни отметок показа
    let (text, rules_tokens, shown_now) = if holdout {
        (String::new(), 0, Vec::new())
    } else {
        (text, rules_tokens, shown_now)
    };
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
        if holdout {
            // учёт контрольной группы: вызов события, выдано 0
            tokens.push(TokenCount {
                event: token_event(req.event, recovery).into(),
                calls: 1,
                tokens: 0,
                reread_calls: 0,
            });
        } else if !text.is_empty() && origin.is_none() {
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
    if holdout {
        out["holdout"] = json!(true);
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

    /// M6.6 §4.1 (замечание 27): ловушки решения — первыми, одно место из `entries` — самой
    /// новой Knowledge (`sol[..n-1] ++ kn[..1] ++ sol[n-1..] ++ kn[1..]`); у решения меньше
    /// `n − 1` ловушек — места занимает Knowledge; первое место — всегда решения, если у него
    /// есть ловушки (и при `entries = 1`).
    #[test]
    fn trap_order_solution_first_knowledge_slot() {
        let s = |n: usize| (0..n).map(|k| format!("s{k}")).collect::<Vec<_>>();
        let kn = |n: usize| (0..n).map(|k| format!("k{k}")).collect::<Vec<_>>();
        assert_eq!(
            trap_order(s(5), kn(6), 5)[..5],
            ["s0", "s1", "s2", "s3", "k0"]
        );
        assert_eq!(
            trap_order(s(5), kn(2), 5),
            ["s0", "s1", "s2", "s3", "k0", "s4", "k1"]
        );
        assert_eq!(
            trap_order(s(2), kn(4), 5),
            ["s0", "s1", "k0", "k1", "k2", "k3"]
        );
        assert_eq!(trap_order(s(0), kn(3), 5), ["k0", "k1", "k2"]);
        assert_eq!(trap_order(s(6), kn(0), 5), s(6));
        // мини-дайджест `brief` — 3 места
        assert_eq!(trap_order(s(5), kn(2), 3)[..3], ["s0", "s1", "k0"]);
        assert_eq!(trap_order(s(2), kn(2), 1), ["s0", "k0", "s1", "k1"]);
        assert_eq!(DIGEST_KNOWLEDGE_TRAPS, 1);
    }

    /// M6.6 §4.2: строка открытых задач — число и заголовки (секреты замаскированы, длинный —
    /// до 80 символов с «…»); без заголовков (все открытые захвачены) — только число.
    #[test]
    fn open_tasks_text_format() {
        // фиктивный ключ AWS — склейкой во время выполнения
        let key = format!("{}{}", "AK", "IAABCDEFGHIJKLMNOP");
        let secret = format!("Ротация ключа {key}");
        let long = "длинный заголовок ".repeat(8);
        let text = open_tasks_text(5, &["Первая", &secret, &long]);
        assert!(
            text.starts_with("Открыто задач: 5 — «Первая», «Ротация ключа "),
            "{text}"
        );
        assert!(
            !text.contains(&key) && text.contains(zam_markdown::SECRET_MARK),
            "{text}"
        );
        let last = text.rsplit_once(", «").unwrap().1;
        let last = last.strip_suffix('»').unwrap();
        assert!(last.ends_with('…') && last.chars().count() <= 80, "{last}");
        assert!(long.starts_with(last.trim_end_matches('…')), "{last}");
        assert_eq!(open_tasks_text(1, &[]), "Открыто задач: 1");
        assert_eq!(OPEN_TASKS_TITLES, 3);
    }

    /// M6.6 §4.2: строка открытых задач — сразу за задачами (до строки плана), всегда с
    /// заголовком `### Задачи` (ревью задачи 3, M2): вошли строки задач — заголовок один, над
    /// ними; не вошли или их нет — заголовок у строки. Стойкость указателя (снимается раньше
    /// обязательных строк), адрес снятой — `kinds=[task]`; в пакете восстановления — так же.
    #[test]
    fn open_tasks_line_under_tasks_heading() {
        let t = cand("задача");
        let own = |o: Omit| Some((o.0.to_string(), o.1.to_string()));
        let open = "Открыто задач: 2 — «а»";
        let words = |s: &str| s.split_whitespace().count();
        for recovery in [false, true] {
            let chosen = vec![
                (Some(SECTION_TASKS), vec![&t]),
                (Some(SECTION_ANSWERS), Vec::new()),
            ];
            let lines = Lines {
                open_tasks: Some(open.into()),
                plan_line: Some("План".into()),
                ..Lines::default()
            };
            let (packed, picked) = arrange(recovery, true, lines, Vec::new(), chosen);
            let k = packed.iter().position(|s| s.blocks == [open]).unwrap();
            assert_eq!(packed[k - 1].title.as_deref(), Some(SECTION_TASKS));
            assert_eq!(
                packed[k].title.as_deref(),
                Some(SECTION_TASKS),
                "{recovery}"
            );
            assert!(packed[k].keep == KEEP_POINTERS && packed[k].keep < KEEP_MANDATORY);
            assert_eq!(packed[k].omit, own(OMIT_OPEN_TASKS));
            assert!(picked[k].is_empty());
            assert_eq!(packed[k + 1].blocks, ["План"]);
            // строка задачи вошла — заголовок один, строка открытых задач продолжает раздел
            let (text, _) = pack_sections(&packed, 10_000, &words, &|_| true);
            assert_eq!(
                text,
                format!("{SECTION_TASKS}\nZeroAIMemory: задача\n{open}\nПлан\n"),
                "{recovery}"
            );
            // строка задачи не вошла в бюджет, строка открытых задач вошла — заголовок у неё
            let mut big = packed.clone();
            big[k - 1].blocks = vec!["задача ".repeat(50)];
            let (text, _) = pack_sections(&big, 10, &words, &|_| true);
            assert_eq!(
                text,
                format!("{SECTION_TASKS}\n{open}\nПлан\n"),
                "{recovery}"
            );
        }
        let chosen = vec![(Some(SECTION_TASKS), Vec::new())];
        let lines = Lines {
            open_tasks: Some(open.into()),
            ..Lines::default()
        };
        let (packed, _) = arrange(false, true, lines, Vec::new(), chosen);
        let k = packed.iter().position(|s| s.blocks == [open]).unwrap();
        assert_eq!(packed[k].title.as_deref(), Some(SECTION_TASKS));
        let (text, _) = pack_sections(&packed, 10_000, &count_tokens, &|_| true);
        assert_eq!(text, format!("{SECTION_TASKS}\n{open}\n"));
    }

    /// Ревью задачи 3, M4: маска — до обрезки. Ключ, который обрезка разрезала бы на границе,
    /// маска уже не узнала бы, и его начало ушло бы в дайджест; с маской сначала — только метка.
    #[test]
    fn masked_clip_hides_key_cut_at_boundary() {
        // фиктивный ключ AWS — склейкой во время выполнения
        let key = format!("{}{}", "AK", "IAABCDEFGHIJKLMNOP");
        let s = format!("{} {key}", "з".repeat(185));
        // прежний порядок «обрезка, затем маска» оставлял начало ключа
        let old = zam_markdown::mask_text(&clip(&s, TASK_LINE_TEXT_CHARS));
        assert!(old.contains(&key[..8]), "{old}");
        let out = masked_clip(&s, TASK_LINE_TEXT_CHARS);
        assert!(!out.contains(&key[..4]), "{out}");
        assert!(out.contains(zam_markdown::SECRET_MARK), "{out}");
        assert!(out.chars().count() <= TASK_LINE_TEXT_CHARS, "{out}");
        // без секрета — та же обрезка, что у `clip`
        let plain = "слово ".repeat(60);
        assert_eq!(masked_clip(&plain, 80), clip(&plain, 80));
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

    /// M6.6 §5.1: срок сбора без поля — хук (без MCP, событие хука или поле `code_map`) —
    /// `recall.timeout_ms`, явный вызов — `max(recall.max_timeout_ms, recall.timeout_ms)`; поле
    /// `timeout_ms` важнее всего.
    #[test]
    fn collect_timeout_hook_or_explicit() {
        let mut cfg = crate::config::ServerConfig::default().recall;
        (cfg.timeout_ms, cfg.max_timeout_ms) = (200, 2000);
        let (hook, explicit) = (200, 2000);
        use Event::*;
        // (событие, code_map, MCP, срок)
        let table = [
            (SessionStart, false, false, hook),
            (Prompt, false, false, hook),
            (PostTool, false, false, hook),
            (Recovery, false, false, explicit),
            (Brief, false, false, explicit),
            (IdeaReview, false, false, explicit),
            // хук `subagent-start` (и `session-start`) шлёт `code_map`
            (Brief, true, false, hook),
            (Recovery, true, false, hook),
            (SessionStart, true, false, hook),
            // MCP — всегда явный вызов
            (SessionStart, false, true, explicit),
            (Prompt, false, true, explicit),
            (PostTool, false, true, explicit),
            (Recovery, false, true, explicit),
            (Brief, false, true, explicit),
            (IdeaReview, false, true, explicit),
            (Brief, true, true, explicit),
        ];
        for (event, code_map, mcp, want) in table {
            assert_eq!(
                collect_timeout(None, event, code_map, mcp, &cfg),
                want,
                "{event:?} code_map={code_map} mcp={mcp}"
            );
            assert_eq!(
                collect_timeout(Some(700), event, code_map, mcp, &cfg),
                700,
                "поле важнее: {event:?} code_map={code_map} mcp={mcp}"
            );
        }
        // срок сервера больше предела поля (сетки замера) — явный вызов ждёт его
        cfg.timeout_ms = 60_000;
        assert_eq!(collect_timeout(None, Brief, false, true, &cfg), 60_000);
        assert_eq!(collect_timeout(None, Prompt, false, false, &cfg), 60_000);
    }
}
