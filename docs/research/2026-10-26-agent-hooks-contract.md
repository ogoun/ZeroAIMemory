# Контракт хуков и MCP агентов (Claude Code, Codex) — для M4

Дата: 2026-10-26. Источники: https://code.claude.com/docs/en/hooks, https://code.claude.com/docs/en/mcp, https://learn.chatgpt.com/docs/hooks (бывш. developers.openai.com/codex/hooks). ТЗ §10.3 требует сверить JSON-контракт хуков с актуальной документацией на M4 — это итог сверки.

## Claude Code

### Настройки хуков (`.claude/settings.json`)
- `hooks` → событие → массив групп `{matcher, hooks: [{type: "command", command, args?, timeout}]}`.
- `timeout` — **секунды** (по умолчанию 600 у `command`, 30 у `UserPromptSubmit`).
- `args` задан — команда запускается без shell (exec form); без `args` — через shell.
- `matcher`: только буквы, цифры, `_ - , |` и пробел — точные имена через `|` (`Write|Edit|Bash`); иначе — JS regex без якорей; пусто/`*` — все.
- `${CLAUDE_PROJECT_DIR}` — корень проекта сессии.
- `async: true` — фоновый хук (не управляет поведением).
- **Не подтверждено:** какой shell исполняет `command` на Windows — проверить на машине владельца; для `zam` безопаснее exec form (`command: "zam"`, `args: ["hook", "pre-tool"]`).

### События и stdin (один JSON-объект)
Общие поля: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode` (не у всех), `prompt_id`, `agent_id`/`agent_type` (внутри субагента).

| Событие | Специфичные поля |
|---|---|
| `SessionStart` | `source`: `startup` \| `resume` \| `clear` \| `compact` \| `fork`; `model` |
| `UserPromptSubmit` | `prompt` |
| `PreToolUse` | `tool_name`, `tool_input`, `tool_use_id` |
| `PostToolUse` | `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, `duration_ms?` |
| `PostToolUseFailure` | `tool_name`, `tool_input`, `error` (строка; у Bash первая строка `Exit code N`), `is_interrupt?`, `duration_ms?`; не срабатывает на отказах до выполнения |
| `Stop` | `stop_hook_active`, `last_assistant_message` |
| `SubagentStop` | `stop_hook_active`, `agent_id`, `agent_type`, `last_assistant_message` |
| `PreCompact` | `trigger`: `manual` \| `auto`; `custom_instructions` |

`tool_input`: Write — `file_path` (абсолютный), `content`; Edit — `file_path`, `old_string`, `new_string`, `replace_all`; Bash — `command`, `description`, `timeout`, `run_in_background`. `tool_response` у Bash: `stdout`, `stderr`, `interrupted`, `isImage`; кода выхода в нём нет — ошибка Bash приходит событием `PostToolUseFailure`.

### Вывод
- Контекст агенту: `{"hookSpecificOutput": {"hookEventName": "<событие>", "additionalContext": "…"}}` — принимают SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PostToolUseFailure, Stop, SubagentStop и др.; текст оборачивается в системное напоминание.
- Простой stdout — контекст только для SessionStart и UserPromptSubmit; для Pre/PostToolUse нужен JSON.
- Stop: `{"decision": "block", "reason": "…"}` — продолжить с инструкцией `reason`; при `stop_hook_active == true` не блокировать повторно (у Claude Code ещё предел 8 продолжений подряд). Мягкий вариант — `additionalContext`.
- Коды выхода: 0 — успех (stdout — JSON или текст); 2 — блокирующая ошибка (stderr — причина) на блокирующих событиях; прочие — неблокирующая ошибка. Хуки ZeroAIMemory всегда выходят с 0 (fail-open, §10.3).
- Предел `additionalContext` и stdout — 10 000 символов (сверх — в файл с превью 2000).

### `.mcp.json` (удалённый MCP по streamable HTTP)
```json
{"mcpServers": {"zeroaimemory": {"type": "http", "url": "http://memory.lan:7700/mcp",
  "headers": {"Authorization": "Bearer ${ZAM_TOKEN}", "X-Zam-Solution": "<guid>"}}}}
```
`type` обязателен (`http`, алиас `streamable-http`); `${VAR}` и `${VAR:-default}` подставляются в `url` и `headers` (кроме переменных учётных данных Anthropic/AWS/NPM). Проектный сервер подтверждается пользователем при первом запуске.

## Codex CLI
- Хуки: `<repo>/.codex/hooks.json` или `[hooks]` в `.codex/config.toml` (проектный слой — только если доверен); формат как у Claude Code: событие → `{matcher?, hooks: [{type: "command", command, commandWindows?, timeout}]}`.
- События: SessionStart, SessionEnd, PreToolUse, PermissionRequest, PostToolUse, PreCompact, PostCompact, UserPromptSubmit, SubagentStart, SubagentStop, Stop, Interrupt. **Нет `PostToolUseFailure`** — ошибку Bash хук видит в `PostToolUse.tool_response`.
- stdin и вывод — те же имена (`hookSpecificOutput.additionalContext`, Stop — `decision: "block"` + `reason`); предел контекста ≈ 2500 токенов (`additionalContextLimit`).
- Изменённый хук ждёт повторного подтверждения в `/hooks` (доверие по хешу).

## Выводы для M4
1. Хуки `zam` пишутся в exec form (`command` + `args`) — без зависимости от shell на Windows.
2. Ответ всех хуков — JSON с `hookSpecificOutput` (единый путь для всех событий), кроме `checkpoint`: `decision: "block"` только при `stop_hook_active != true`.
3. `post-tool` обрабатывает `PostToolUseFailure.error` (Claude Code) и `PostToolUse.tool_response.stderr` (Codex и Bash без сбоя).
4. Предел вывода — 10 000 символов (Claude Code) и ~2500 токенов (Codex): бюджеты хуков §10.3 (1500 токенов дайджеста) укладываются.
5. Проверить вручную на машине владельца: запуск `command` на Windows и блокировка exit 2 на PreToolUse (нам не нужна — всегда 0).
