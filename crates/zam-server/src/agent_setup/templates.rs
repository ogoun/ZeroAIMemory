//! Шаблоны по умолчанию блока инструкции агента (§14.2) и полный протокол `GET /agent-guide`.
//!
//! Блок v6 (M4.5 §11, M5.5 — строка об отчётах `zam report`, M5.6 — строка о карте кода и
//! ссылках `путь#Тип::метод`, M5.7 — строка об идеях): правило — одна строка, не больше 500
//! токенов bge-m3 в худшем случае теста (ru и en); пояснения — в протоколе
//! (`guide.{ru,en}.md`), а не в блоке. Ради строки v5 в протокол перенесены «исследование —
//! `insight_kinds=[experiment,failure]`», «не про проект — `scope=global`» и «пиши
//! каноническими терминами»; ради строки v6 — «(409 — перечитай)», «заметке — `l0`», «перед
//! работой» и «в чате», строки задач и вопросов слиты (порядок предложения идеи приходит с
//! разделом `### Идеи` дайджеста). Строки ru и en синхронны.
//!
//! Шаблон — текст между маркерами `<!-- zeroaimemory:begin vN -->` и `<!-- zeroaimemory:end -->`
//! (маркеры добавляет [`super::render_block`]). Подстановки:
//! - `{url}` — внешний адрес сервера (`server.public_url` или адрес запроса);
//! - `{solution}` — решение: «имя» и GUID, если оно выбрано, иначе указание на `zam init`;
//! - `{rules}` — можно ли агенту писать в Rules.
//!
//! Токен в блок не подставляется **[решение контроллера, задача 11 M4]**: блок попадает в
//! репозиторий, а `zam` и MCP берут токен из `ZAM_TOKEN` или `~/.zeroaimemory/config.toml`.

/// Шаблон по умолчанию (одинаков для Claude Code, AGENTS.md и Codex), русский.
pub const BLOCK_RU: &str = "\
## Память ZeroAIMemory
ZeroAIMemoryUrl={url}
Решение: {solution}.
Без MCP — `zam`; протокол — `zam guide`.

- Перед задачей — `memory_search` → `[cache]` или `memory_read` пачкой → сводку в `memory_cache_put`.
- `memory_log <kind>` пишет, `memory_update_entry` с `rev` правит.
- Ловушка, сбой → `insight` gotcha|failure с триггерами; изменение → `changelog`; устарело → supersede/retract.
- Итог исследования, эксперимента, сравнения — пользователю `zam report`, себе — `insight`.
- Задача → `task` (claim, `status` с `next_step`); вопрос человеку → `question`, его ответ → `answer`.
- Идея → `idea`; начинать её — только после ответа человека «начать».
- Код — `refs.files`, функция — `путь#Тип::метод`; карта — `zam code map`, символ — `zam code sym`; новое понятие → `term`.
- `axiom` — только по прямому указанию пользователя; несогласен — `hypothesis`/`theorem` с `contradicts` и скажи ему; `certainty` меняй с `reason`.
- Секреты, хосты → `memory_vault`. {rules}
- Без хуков `memory_recall`: новый тип файла, ошибка; после сжатия — `recovery`; субагенту — `brief`.
";

/// Шаблон по умолчанию, английский.
pub const BLOCK_EN: &str = "\
## ZeroAIMemory memory
ZeroAIMemoryUrl={url}
Solution: {solution}.
No MCP — `zam`; protocol — `zam guide`.

- Before a task — `memory_search` → `[cache]` or `memory_read` in one call → summary into `memory_cache_put`.
- `memory_log <kind>` writes, `memory_update_entry` with `rev` edits.
- Gotcha, failure → `insight` gotcha|failure with triggers; change → `changelog`; outdated → supersede/retract.
- Outcome of research, an experiment, a comparison — for the user `zam report`, for yourself — `insight`.
- Task → `task` (claim, `status` with `next_step`); question for the human → `question`, their answer → `answer`.
- Idea → `idea`; start it only after the human answers “start”.
- Code — `refs.files`, a function — `path#Type::method`; map — `zam code map`, symbol — `zam code sym`; new concept → `term`.
- `axiom` — only on the user's direct instruction; if you disagree — `hypothesis`/`theorem` with `contradicts` and tell the user; change `certainty` with a `reason`.
- Secrets, hosts → `memory_vault`. {rules}
- Without hooks `memory_recall`: new file type, error; after compaction — `recovery`; for a subagent — `brief`.
";

/// `{solution}` без выбранного решения.
pub const SOLUTION_AUTO_RU: &str = "`.zeroaimemory/solution.toml` (создаёт `zam init`)";
pub const SOLUTION_AUTO_EN: &str = "`.zeroaimemory/solution.toml` (created by `zam init`)";

/// `{rules}`: запись агента в Rules разрешена / запрещена (запрещена — пусто: запись и так
/// получит 403 с объяснением, а блок экономит токены).
pub const RULES_WRITE_RU: &str = "Общие правила — в Rules.";
pub const RULES_READONLY_RU: &str = "";
pub const RULES_WRITE_EN: &str = "Shared rules go to Rules.";
pub const RULES_READONLY_EN: &str = "";

/// Полный протокол работы с памятью (`GET /api/v1/agent-guide`, `zam guide`): §9.3 и §14.
pub const GUIDE_RU: &str = include_str!("guide.ru.md");
pub const GUIDE_EN: &str = include_str!("guide.en.md");
