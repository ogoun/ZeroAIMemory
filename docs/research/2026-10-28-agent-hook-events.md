# События хуков агентов для M4.5 «Рабочая память»

Дата: 2026-10-28. Все ссылки открыты 2026-10-28 (WebFetch/WebSearch). Метка «не подтверждено» — в источнике нет прямого ответа. Текущее состояние кода: `zam hook` знает `session-start | prompt | pre-tool | post-tool | checkpoint` (crates/zam-cli/src/cmd/hook/mod.rs), агенты `claude | codex`. Базовый контракт M4 — docs/research/2026-10-26-agent-hooks-contract.md; здесь только новые и уточнённые факты.

## 1. Claude Code (https://code.claude.com/docs/en/hooks)

События (33 в таблице): SessionStart, Setup, UserPromptSubmit, UserPromptExpansion, PreToolUse, PermissionRequest, PermissionDenied, PostToolUse, PostToolUseFailure, PostToolBatch, Notification, MessageDisplay, SubagentStart, SubagentStop, TaskCreated, TaskCompleted, Stop, StopFailure, TeammateIdle, InstructionsLoaded, ConfigChange, CwdChanged, DirectoryAdded, FileChanged, WorktreeCreate, WorktreeRemove, PreCompact, PostCompact, PreModelSwitch, PostModelSwitch, Elicitation, ElicitationResult, SessionEnd.

| Событие | Matcher | Входные поля (сверх общих `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `agent_id`/`agent_type` внутри субагента) | Внедрение текста |
|---|---|---|---|
| SessionStart | `startup`, `resume`, `clear`, `compact`, `fork` | `source`, `model?`, `agent_type?`, `session_title?` | `hookSpecificOutput.additionalContext` или обычный stdout (код 0): «в начале разговора, до первого промпта». Срабатывает и после compact (`source=compact`), и при resume |
| UserPromptSubmit | нет (на каждый промпт, в т.ч. автоматические ходы) | `prompt`, `session_title?` | `additionalContext` или stdout; заменить промпт нельзя; `decision: block` / exit 2 блокирует. Тайм-аут по умолчанию 30 с |
| SubagentStart | тип агента (`general-purpose`, `Explore`, `Plan`, свои) | `agent_id`, `agent_type` | **Да, в контекст субагента**: «can inject context into the subagent»; `additionalContext` добавляется «at the start of its conversation, before its first prompt». Создание заблокировать нельзя. Повторный запуск для того же субагента вставляет текст снова, только если его там нет |
| SubagentStop | тип агента | `stop_hook_active`, `agent_id`, `agent_type`, `agent_transcript_path` (вложенная папка `subagents/`), `last_assistant_message`, `background_tasks`, `session_crons` | `decision` и `additionalContext` (обратная связь, продолжающая работу); exit 2 не даёт субагенту остановиться |
| PreCompact | `manual`, `auto` | `trigger`, `custom_instructions` (у `manual` — текст `/compact`, иначе null) | Блокировка: exit 2 или `decision: block`. `systemMessage` и `continue` отбрасываются. `additionalContext` для PreCompact в документации **не подтверждён** |
| PostCompact | `manual`, `auto` | `trigger`, `compact_summary` (сводка, созданная compact) | Решений нет; `systemMessage`/`continue` отброшены; внедрение контекста не документировано. Для возврата памяти после compact использовать SessionStart с `source=compact` |
| PostToolUse | имя инструмента | `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, `duration_ms?`, `mcp_server?` (для MCP) | `additionalContext` «рядом с результатом инструмента»; `updatedToolOutput` заменяет результат |
| PostToolUseFailure | имя инструмента | `tool_name`, `tool_input`, `error`, `is_interrupt?`, `duration_ms` | `additionalContext` (по отчёту 2026-10-26) |

Общее:
- Лимит: `additionalContext`, `systemMessage`, `initialUserMessage` и обычный stdout — по 10 000 символов каждый; сверх — файл в каталоге сессии, в контекст идёт путь и превью 2000 символов, читать файл Claude не просят.
- SessionStart дополнительно принимает `initialUserMessage`, `watchPaths`, `sessionTitle`, `reloadSkills`.
- Совет документации: писать фактами, а не императивными «системными» командами, иначе сработает защита от prompt injection и текст покажут пользователю.
- Переменные окружения хука: `CLAUDE_PROJECT_DIR` (корень проекта, фиксирован), `CLAUDE_ENV_FILE` (только SessionStart/Setup/CwdChanged/FileChanged; в PowerShell-хуке недоступна), `CLAUDE_PLUGIN_ROOT`/`CLAUDE_PLUGIN_DATA`, `CLAUDE_CODE_REMOTE`, `CLAUDE_EFFORT`. Хуки наследуют окружение родителя без `OTEL_*`.
- `CLAUDE_CODE_SESSION_ID`: на официальной странице хуков **не подтверждено**. По вторичному источнику (issue #56879, май 2026) появилась в v2.1.132 в окружении Bash-инструмента; для хуков надёжный идентификатор — поле `session_id` из stdin.
- Известная ошибка (вторичный источник, issue #50467): PreCompact на auto-compaction не срабатывал в v2.1.105–2.1.114. Не полагаться на него как на единственный путь.

## 2. OpenAI Codex CLI (https://learn.chatgpt.com/docs/hooks, ранее developers.openai.com/codex/hooks, 308-редирект)

- Конфиг: `hooks.json` или `[hooks]` в `config.toml`: `~/.codex/`, `<repo>/.codex/`; плагины — `hooks/hooks.json`. Проектный слой работает только в доверенном проекте; хуки нужно подтвердить в `/hooks`. Хуки включены по умолчанию (`[features] hooks = false` выключает; `codex_hooks` — устаревший алиас).
- События: SessionStart, SessionEnd, PreToolUse, PermissionRequest, PostToolUse, PreCompact, PostCompact, UserPromptSubmit, SubagentStart, SubagentStop, Stop, Interrupt. Нет PostToolUseFailure.
- Matcher — regex; игнорируется для UserPromptSubmit/Stop/Interrupt. Инструменты: `apply_patch` совпадает с `apply_patch`, `Edit` и `Write` (в stdin `tool_name` всегда `apply_patch`). PreCompact/PostCompact: `manual`/`auto`. SessionStart: `startup|resume|clear|compact|fork`. SubagentStart/Stop: тип агента.
- Общие поля stdin: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, чаще всего `permission_mode`; в событиях хода ещё `turn_id`. SubagentStart: `agent_id`, `agent_type`. PreCompact/PostCompact: `trigger` (без summary). SubagentStop: `agent_transcript_path`, `last_assistant_message`.
- Внедрение контекста: SessionStart, SubagentStart, UserPromptSubmit — stdout или `hookSpecificOutput.additionalContext`; PreToolUse (только JSON) и PostToolUse — `additionalContext`. Для PermissionRequest, PreCompact, PostCompact, SubagentStop, Stop, SessionEnd инъекция не документирована. После compact корневой сессии выполняются SessionStart-хуки с `source: "compact"` до следующего запроса к модели.
- SubagentStart в Codex: `additionalContext` документирован, но попадает ли он именно в контекст субагента, страница прямо не говорит — **не подтверждено**; `continue:false` запуск не останавливает.
- Лимит: около 2500 токенов на сообщение хука (`additionalContextLimit`, 0 — без предела); излишек — в файл `<temp>/hook_outputs/<session_id>/<uuid>.txt`. Тайм-аут по умолчанию 600 с.
- Переменные окружения: `PLUGIN_ROOT`, `PLUGIN_DATA` и совместимые `CLAUDE_PLUGIN_ROOT/DATA`; идентификатора сессии в env нет, брать `session_id` из stdin. Рабочий каталог — `cwd` сессии. Хуки инструментов — «полезная защита, но не полная граница».
- PR openai/codex#19905 (вторичный): `decision: block` в PreCompact не блокирует compaction.

## 3. Другие агенты

| Агент | Что есть | Вывод для M4.5 |
|---|---|---|
| Cursor (https://cursor.com/docs/agent/hooks) | `.cursor/hooks.json`; события sessionStart, sessionEnd, preToolUse, postToolUse, postToolUseFailure, subagentStart, subagentStop, beforeReadFile, afterFileEdit, beforeSubmitPrompt, preCompact, stop и др. `additional_context` принимают только sessionStart (в начальный системный контекст), postToolUse, postToolUseFailure. subagentStart: только `permission`/`user_message`; preCompact: только `user_message`; beforeSubmitPrompt: `continue`/`user_message`. Env: `CURSOR_PROJECT_DIR`, `CURSOR_TRANSCRIPT_PATH`, алиас `CLAUDE_PROJECT_DIR` | Старт сессии и ошибка→исправление через хуки; субагент, compact, чтение через MCP и правила |
| Gemini CLI (https://geminicli.com/docs/hooks/) | `settings.json`; SessionStart (inject context), BeforeAgent (контекст), AfterTool (контекст), PreCompress (только уведомление), BeforeTool, AfterAgent, BeforeModel/AfterModel, BeforeToolSelection, Notification, SessionEnd. Env: `GEMINI_PROJECT_DIR`, `GEMINI_SESSION_ID`, `GEMINI_CWD`, алиас `CLAUDE_PROJECT_DIR`. Субагенты в хуках не описаны (не подтверждено); поле `additionalContext` по имени на странице не названо | Старт, prompt, post-tool через хуки; compact и субагент через fallback |
| Cline (https://docs.cline.bot/sdk/plugins; страница customization/hooks отсылает сюда) | Плагины SDK: beforeRun (`appendContext`: текст в первом запросе модели), beforeTool/afterTool (`appendContext`), beforeModel, afterModel, afterRun, onEvent. Хуков compact и session start нет. Классические Cline hooks (TaskStart/PreCompact): **не подтверждено** | Старт и tool через SDK-плагин (это не `zam hook`); остальное через MCP и правила |
| Windsurf / Devin Desktop (https://docs.devin.ai/desktop/cascade/hooks, ранее docs.windsurf.com) | `hooks.json` (`.devin/hooks.json`, `~/.codeium/windsurf/hooks.json`); 12 событий: pre/post_read_code, pre/post_write_code, pre/post_run_command, pre/post_mcp_tool_use, pre_user_prompt, post_cascade_response(+_with_transcript), post_setup_worktree. Внедрения контекста нет: агент видит лишь stderr после exit 2 у pre-хука. Старта сессии, субагента, compact нет | Только MCP и файл правил; хуки лишь для журнала и блокировки |
| GitHub Copilot (https://docs.github.com/en/copilot/reference/hooks-configuration) | Файлы в `.github/hooks/` (для coding agent — в ветке по умолчанию). sessionStart (`additionalContext`: да), userPromptSubmitted (`modifiedPrompt` только SDK; `additionalContext` неясно), preToolUse (нет), postToolUse и postToolUseFailure (да), subagentStart (да, «prepended to the subagent's prompt»), subagentStop (block/`modifiedResponse`), agentStop, preCompact (только уведомление), errorOccurred, sessionEnd. Инструкции AGENTS.md и `.github/copilot-instructions.md` грузятся на старте | Почти полный набор хуков; compact через fallback. Различия CLI и cloud coding agent по событиям не подтверждены |

Агентам без хуков или без инъекции: MCP-инструменты ядра (recall/remember/сессия) плюс блок правил в AGENTS.md / CLAUDE.md / `.cursor/rules` / `.github/copilot-instructions.md` (`zam init` уже пишет такой блок в CLAUDE.md и AGENTS.md).

## 4. Сводная таблица: событие ядра M4.5 × агент

Обозначения: H — хук с инъекцией контекста; h — хук без инъекции (запись, журнал, побочное действие); MCP — fallback: вызов инструмента ядра по правилу в AGENTS.md.

| Событие ядра | Claude Code | Codex | Cursor | Gemini CLI | Copilot | Cline | Windsurf |
|---|---|---|---|---|---|---|---|
| Старт сессии | H: SessionStart `startup` | H: SessionStart | H: sessionStart | H: SessionStart | H: sessionStart | H: SDK beforeRun | MCP + правила |
| Старт субагента | H: SubagentStart, `additionalContext` прямо в субагента (matcher по `agent_type`) | H?: SubagentStart (адресат не подтверждён) | h: subagentStart без контекста; MCP | не подтверждено; MCP | H: subagentStart (в начало промпта субагента) | MCP | MCP |
| Перед compact | h: PreCompact (`trigger`, `custom_instructions`; инъекции не подтверждено; сохранить состояние на сервер); страховка Stop/checkpoint | h: PreCompact (только `trigger`) | h: preCompact | h: PreCompress | h: preCompact | MCP | MCP |
| После compact / resume | H: SessionStart `compact` и `resume`; PostCompact даёт `compact_summary`, без инъекции | H: SessionStart `compact`/`resume` | H: sessionStart (resume); compact не подтверждено; MCP | H: SessionStart (startup/resume/clear); compact нет | H: sessionStart; после compact не подтверждено | MCP | MCP |
| Чтение файла | H: PostToolUse (matcher `Read`), текст до 10 000 симв. | H: PostToolUse; отдельного read-инструмента не подтверждено | h: beforeReadFile (только `permission`); H: postToolUse; MCP | H: AfterTool | H: postToolUse | H: SDK afterTool | h: pre/post_read_code; MCP |
| Ошибка → исправление | H: PostToolUseFailure + PostToolUse(Edit/Write), состояние в `.zeroaimemory/cache` | H: PostToolUse (`tool_response`; события отказа нет); правка = `apply_patch` | H: postToolUseFailure + afterFileEdit(h) | H: AfterTool | H: postToolUseFailure + postToolUse | H: SDK afterTool | h: post_run_command; MCP |

## 5. Выводы для плана

1. Подтверждено: SubagentStart у Claude Code кладёт `additionalContext` в контекст самого субагента; у Copilot, по таблице документации, в начало его промпта; Cursor не позволяет; Codex адресат не подтверждён. Для остальных нужен fallback: правило «вызови recall при старте» в определении субагента или AGENTS.md и подсказка в промпте родителя.
2. Восстановление после compact у Claude Code и Codex делать через SessionStart с matcher `compact` (PostCompact контекст не внедряет). Состояние на сервер сохранять на PreCompact/Stop (в PreCompact инъекции нет, он годится для побочного действия).
3. Надёжный идентификатор сессии: `session_id` из stdin (Claude, Codex, Gemini; у Gemini ещё `GEMINI_SESSION_ID`). `CLAUDE_CODE_SESSION_ID` официально не подтверждён.
4. Лимит вывода: Claude 10 000 символов на поле, Codex около 2500 токенов; дайджест 1500 токенов M4.5 укладывается в оба. Текст писать фактами, не командами.
5. Новые команды адаптера: расширить `session-start` (matcher `compact|resume`), добавить `subagent-start`, `pre-compact`, `post-compact`. Для Cursor и Copilot формат вывода другой (Cursor: snake_case `additional_context`), значит `--agent` расширять отдельными форматтерами.
6. Codex: инструмент правки в matcher — `apply_patch` (совпадает также с `Edit`/`Write`); отдельный read-инструмент в документации не найден.

## Источники
- https://code.claude.com/docs/en/hooks (Claude Code hooks reference, 2026-10-28)
- https://learn.chatgpt.com/docs/hooks (Codex hooks; прежний адрес https://developers.openai.com/codex/hooks, 2026-10-28)
- https://github.com/openai/codex/pull/19905 (PreCompact/PostCompact в Codex; вторичный, по результатам поиска)
- https://cursor.com/docs/agent/hooks (2026-10-28)
- https://geminicli.com/docs/hooks/ (2026-10-28)
- https://docs.cline.bot/sdk/plugins (2026-10-28)
- https://docs.devin.ai/desktop/cascade/hooks (ранее https://docs.windsurf.com/windsurf/cascade/hooks, 2026-10-28)
- https://docs.github.com/en/copilot/reference/hooks-configuration (2026-10-28)
- https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/use-hooks (по результатам поиска, страница не открывалась)
- https://claudeissues.com/issue/56879-docs-environment-variables-claude-code-session-id-not-documented-in-env-vars-ref (вторичный: CLAUDE_CODE_SESSION_ID)
- https://claudeissues.com/issue/50467-precompact-hook-never-fires-on-auto-compaction-v2-1-105-v2-1-114-refs-13572 (вторичный: сбой PreCompact auto)
- docs/research/2026-10-26-agent-hooks-contract.md (внутренний, база M4)
