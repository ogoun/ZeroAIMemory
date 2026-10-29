//! Шаблоны по умолчанию блока инструкции агента (§14.2) и полный протокол `GET /agent-guide`.
//!
//! Блок v3 (M4.5 §11): правило — одна строка, не больше 500 токенов bge-m3 в худшем случае
//! теста (ru и en); пояснения — в протоколе (`guide.{ru,en}.md`), а не в блоке.
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
Токен — `ZAM_TOKEN`. Решение: {solution}.
Без MCP — `zam`; протокол — `zam guide`.

- Перед задачей — `memory_search` (исследование — `insight_kinds=[experiment,failure]`) → `[cache]` или `memory_read` пачкой (`tier=L1`/`section`) → сводку в `memory_cache_put`.
- `memory_log <kind>` пишет, `memory_update_entry` с `rev` правит (409 — перечитай).
- Ловушка, сбой → `insight` gotcha|failure с триггерами (не про проект — scope=global); изменение → `changelog`; устарело → supersede/retract.
- Задача → `task`: claim перед работой, `status` с `next_step`.
- Вопрос человеку → `question`; его ответ в чате → `answer`.
- Код — с `refs.files`; новое понятие → `term`, пиши каноническими терминами.
- `axiom` — только по прямому указанию пользователя; несогласен — `hypothesis`/`theorem` с `contradicts` и скажи ему; `certainty` меняй с `reason`.
- Секреты, хосты → `memory_vault`; заметке — `l0`. {rules}
- Без хуков `memory_recall`: новый тип файла, ошибка; после сжатия — `recovery`; субагенту — `brief`.
";

/// Шаблон по умолчанию, английский.
pub const BLOCK_EN: &str = "\
## ZeroAIMemory memory
ZeroAIMemoryUrl={url}
Token — `ZAM_TOKEN`. Solution: {solution}.
No MCP — `zam`; protocol — `zam guide`.

- Before a task — `memory_search` (research — `insight_kinds=[experiment,failure]`) → `[cache]` or `memory_read` in one call (`tier=L1`/`section`) → summary into `memory_cache_put`.
- `memory_log <kind>` writes, `memory_update_entry` with `rev` edits (409 — reread).
- Gotcha, failure → `insight` gotcha|failure with triggers (not about the project — scope=global); change → `changelog`; outdated → supersede/retract.
- Task → `task`: claim before work, `status` with `next_step`.
- Question for the human → `question`; their answer in chat → `answer`.
- Code — with `refs.files`; new concept → `term`, write with canonical terms.
- `axiom` — only on the user's direct instruction; if you disagree — `hypothesis`/`theorem` with `contradicts` and tell the user; change `certainty` with a `reason`.
- Secrets, hosts → `memory_vault`; give notes an `l0`. {rules}
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
