//! Шаблоны по умолчанию блока инструкции агента (§14.2) и полный протокол `GET /agent-guide`.
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
Токен — `ZAM_TOKEN`, не в этом файле. Решение: {solution}.
Без MCP — CLI `zam`; протокол — `zam guide`.

Поиск:
1. `memory_search`: актуальную `[cache]`-выжимку используй; `stale` — перечитай изменившееся.
2. Иначе выбери карточки сам; `memory_read` их пачкой, сначала `tier=L1` или `section`.
3. Собрал ответ из нескольких источников → `memory_cache_put` с источниками.

Правила:
- Перед задачей — `memory_search`; перед исследованием — `insight_kinds=[experiment,failure]`.
- Ошибка, ловушка → `memory_log insight` (gotcha|failure; scope=global, если не про проект) с триггерами; эксперимент — с результатом.
- Изменение проекта → `memory_log changelog`; устарело → `memory_update_entry` supersede/retract.
- Правка записи — с `rev`; на 409/428 перечитай.
- `axiom` ставь, правь, снимай только по прямому указанию пользователя; несогласен — `hypothesis`/`theorem` рядом и скажи пользователю.
- Секреты → `memory_vault`. Заметке — `l0`. {rules}
- «ZeroAIMemory:» — накопленные знания. Без хуков: перед новым типом файла и после ошибки — `memory_recall`.
";

/// Шаблон по умолчанию, английский.
pub const BLOCK_EN: &str = "\
## ZeroAIMemory memory
ZeroAIMemoryUrl={url}
Token — `ZAM_TOKEN`, never in this file. Solution: {solution}.
No MCP — CLI `zam`; protocol — `zam guide`.

Search:
1. `memory_search`: use a current `[cache]` summary; `stale` — reread what changed.
2. Otherwise pick cards yourself; `memory_read` them in one call, `tier=L1` or `section` first.
3. Answer built from several sources → `memory_cache_put` with the sources.

Rules:
- Before a task — `memory_search`; before research — `insight_kinds=[experiment,failure]`.
- Error, gotcha → `memory_log insight` (gotcha|failure; scope=global if not about the project) with triggers; experiment — with its result.
- Project change → `memory_log changelog`; outdated → `memory_update_entry` supersede/retract.
- Edit a record with `rev`; on 409/428 reread.
- Set, change or remove `axiom` only on the user's direct instruction; if you disagree — add `hypothesis`/`theorem` alongside and tell the user.
- Secrets → `memory_vault`. Give notes an `l0`. {rules}
- \"ZeroAIMemory:\" lines are accumulated knowledge. Without hooks: before a new file type and after an error — `memory_recall`.
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
