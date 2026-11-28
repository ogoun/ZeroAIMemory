# Интеграция ZeroAIMemory с другими агентами: матрица возможностей

Дата исследования: 2026-11-28. Все сведения взяты из официальной документации и официальных репозиториев (ссылки в каждом разделе). Что не удалось подтвердить первоисточником, помечено **«не подтверждено»**. Сведения с форумов и из сторонних парсеров помечены **«неофициально»**.

Обозначения наших событий (как в адаптере Claude/Codex): `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart`, `Stop`, `PreCompact`, `PostCompact`, `SessionEnd`. В примерах ниже имена подкоманд `zam hook <...> --agent <kind>` даны по образцу текущих (`pre-tool`, `prompt`, `checkpoint`, `post-tool`). Окончательные имена нужно сверить с `crates/zam-cli/src/cmd/init/merge.rs`.

## 1. Сводная матрица

| Агент (версия) | Инструкции | MCP (stdio) | Хуки | Журналы сессий | Headless | Рекомендуемый уровень |
|---|---|---|---|---|---|---|
| **Kimi Code CLI** 2.8.1 (2026-11-26) | `AGENTS.md`, `.kimi-code/AGENTS.md`, глобальный `~/.kimi-code/AGENTS.md`. CLAUDE.md — не подтверждено | да: `.kimi-code/mcp.json` (проект, после доверия) и `~/.kimi-code/mcp.json`, `mcpServers` | да, диалект Claude (20 событий), но **только на уровне пользователя**: `~/.kimi-code/config.toml` `[[hooks]]` или плагин | JSONL: `~/.kimi-code/sessions/<ws>/<sid>/agents/main/wire.jsonl`, схема не описана | `kimi -p … --output-format stream-json` | **полная**, хуки через пользовательский плагин `zam` |
| **Qwen Code** 0.29.0 (28 ноя) | `QWEN.md`, `.qwen/QWEN.local.md`, `~/.qwen/QWEN.md`, плюс `AGENTS.md` | да: `.qwen/settings.json` / `~/.qwen/settings.json`, `mcpServers` | да, почти 1:1 с Claude (23 события, `hookSpecificOutput.additionalContext`), уровень проекта | JSONL: `~/.qwen/projects/<cwd>/chats/<sessionId>.jsonl`, схема `ChatRecord` есть в исходниках | `qwen -p … -o json\|stream-json` | **полная**, самая дешёвая |
| **Google Antigravity** (App 2.36.0 от 2026-11-27, IDE, CLI `agy`) | `AGENTS.md`/`GEMINI.md`, `.agents/rules/*.md`, глобальные `~/.gemini/…`. CLAUDE.md — не подтверждено | да: `.agents/mcp_config.json`, `~/.gemini/config/mcp_config.json`, `mcpServers` | да, свой диалект: 6 событий, `injectSteps`, `decision`; `.agents/hooks.json` (общий для App, IDE и CLI) | JSONL: `<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl`, поля не описаны | только CLI: `agy -p … --output-format json\|stream-json` | **полная** (CLI и IDE одной реализацией), события беднее |
| **Cursor** (IDE; CLI `agent` 2026.11.19) | `.cursor/rules/*.mdc`, `AGENTS.md` (вложенные). CLI читает также `CLAUDE.md` в корне | да: `.cursor/mcp.json`, `~/.cursor/mcp.json`, `mcpServers` | да, свой диалект (18 событий агента), `.cursor/hooks.json`. **Подхватывает хуки Claude из `.claude/settings*.json`** | путь даёт хук (`transcript_path`); на диске `~/.cursor/projects/<slug>/agent-transcripts/…jsonl` (неофициально) | `agent -p … --output-format json\|stream-json --force` | **полная**, с защитой от двойного вызова |
| **GitHub Copilot**: VS Code (Copilot harness), Copilot CLI, cloud agent | `.github/copilot-instructions.md`, `.github/instructions/*.instructions.md`, `AGENTS.md`; CLI и cloud agent читают также `CLAUDE.md`, `GEMINI.md` | да: CLI — `.mcp.json`, `.github/mcp.json`, `~/.copilot/mcp-config.json`; VS Code (Copilot harness) — `~/.copilot/mcp-config.json` и корневой `.mcp.json`; cloud agent — JSON в настройках репозитория | да, общий формат harness: `.github/hooks/*.json`, `~/.copilot/hooks/`; VS Code — preview. **VS Code тоже читает `.claude/settings*.json`** | CLI: `~/.copilot/session-state/<id>/events.jsonl` и `session-store.db`, схема не описана; VS Code: локальная SQLite | CLI: `copilot -p … --allow-all-tools --output-format json` | CLI и VS Code — **полная** (одна реализация); cloud agent — **только инструкции** (сервер в LAN недоступен) |
| **OpenCode** v2.4.2 (26 ноя; ветка v1.19.6 тоже живая) | `AGENTS.md` (вверх до корня), глобальный `~/.config/opencode/AGENTS.md`; `CLAUDE.md` как запасной | да: `opencode.json(c)`, `mcp.servers` (V2), `command` — массив | хуков-команд нет; есть **плагины на TS/JS** (V2 API: `prompt`, `context`, `execute.before/after`, `permission`, `compaction`) | SQLite `~/.local/share/opencode/opencode.db`; выгрузка `opencode session export <id>` | `opencode run --format json --auto …` | сначала **MCP + инструкции**, потом полная через плагин-прослойку |

## 2. Kimi Code CLI (Moonshot)

Источники: репозиторий <https://github.com/MoonshotAI/kimi-code> (лицензия MIT), документация <https://moonshotai.github.io/kimi-code/en/>, журнал изменений <https://moonshotai.github.io/kimi-code/en/release-notes/changelog.html> (последняя версия 2.8.1 от 2026-11-26). Kimi Code пришёл на смену старому `kimi-cli` (`~/.kimi`), миграция описана в <https://moonshotai.github.io/kimi-code/en/guides/migration.html>. Старые справочные страницы kimi.com с путями `~/.kimi/…` устарели.

1. **Инструкции.** <https://moonshotai.github.io/kimi-code/en/customization/agents.html>
   - Проект: `AGENTS.md` или `.kimi-code/AGENTS.md`. Содержимое попадает в системный промпт через переменную `${agents_md}`.
   - Глобально: `$KIMI_CODE_HOME/AGENTS.md` (по умолчанию `~/.kimi-code/AGENTS.md`) и межинструментальный `~/.agents/AGENTS.md`.
   - Порядок слияния и чтение вложенных `AGENTS.md` по странице не подтверждены. Чтение CLAUDE.md не подтверждено.
2. **MCP.** <https://moonshotai.github.io/kimi-code/en/customization/mcp.html>
   - Пользователь: `~/.kimi-code/mcp.json`. Проект: `.kimi-code/mcp.json` в корне git-репозитория, работает только после доверия проекту. Одноимённые записи проекта перекрывают пользовательские.
   - Транспорты: stdio (`command`), HTTP (`url`), устаревший SSE (`"transport": "sse"`).
   - `startupTimeoutMs` по умолчанию 30000. Предел числа инструментов не указан. Управление в интерфейсе: `/mcp-config`, `/mcp`.
   ```json
   { "mcpServers": { "zam": { "command": "zam", "args": ["mcp"] } } }
   ```
3. **Хуки.** <https://moonshotai.github.io/kimi-code/en/customization/hooks.html>, <https://moonshotai.github.io/kimi-code/en/customization/plugins.html>
   - **События:** `SessionStart` (matcher `startup`/`resume`), `UserPromptSubmit`, `UserPromptQueued`, `TurnStarted`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `PermissionResult`, `SubagentStart`, `SubagentStop`, `TaskStarted`, `Stop`, `StopFailure`, `Interrupt`, `PreCompact`, `PostCompact` (`manual`/`auto`), `Notification`, `SessionHeartbeat`, `SessionEnd` (`exit`/`archive`).
   - **Где задаются:** `[[hooks]]` в `~/.kimi-code/config.toml` (ровно 4 поля: `event`, `matcher`, `command`, `timeout`; лишнее поле ломает загрузку всего конфига) или в плагине (`kimi.plugin.json`, массив `hooks`). **Хуков на уровне проекта нет, плагинов на уровне проекта тоже нет.**
   - **Вход (stdin):** `hook_event_name`, `session_id`, `session_title`, `client_type`, `cwd`. У `Stop` есть ещё `transcript_path`, `turn_id`, `response_text`, `stop_hook_active`. Поля `tool_name`/`tool_input` формально не описаны, но используются в примере.
   - **Ответ:** код 0 разрешает, stdout «может быть добавлен в контекст». Код 2 блокирует, причина берётся из stderr. Остальные коды и таймаут пропускают действие (fail-open). JSON-ответ: `hookSpecificOutput.permissionDecision: "deny"`, `additionalContext` (только `SessionStart`), `updatedInput` (`PreToolUse`), `updatedToolOutput` (`PostToolUse`). Блокировать могут только `PreToolUse`, `Stop` и `UserPromptSubmit`. Внедрение текста из `UserPromptSubmit` подтверждает исправление в 2.2.0.
   - **Соответствие нашим событиям:** все 10 есть один к одному (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart`, `Stop`, `PreCompact`, `PostCompact`, `SessionEnd`).
4. **Журналы.** <https://moonshotai.github.io/kimi-code/en/configuration/data-locations.html>
   - Корень: `~/.kimi-code` (Windows: `C:\Users\<name>\.kimi-code`), переносится через `KIMI_CODE_HOME`.
   - Сессия: `sessions/<workspaceId>/<sessionId>/`, где `workspaceId = wd_<slug>_<первые 12 символов sha256 корня проекта>` (у управляемых рабочих пространств `wm_<uuid>`; worktree делят каталог с основным checkout).
   - Файлы сессии: `agents/main/wire.jsonl` (полная запись общения, используется для resume и replay), у субагентов `agents/agent-N/wire.jsonl`, метаданные в `state.json` (title, last prompt, created/updated, forkedFrom).
   - Индекс сессий: `session_index.jsonl` в корне (`sessionId`, `sessionDir`, `workDir`, `projectRoot`).
   - **Схема записей `wire.jsonl` не описана — не подтверждено.** Нужны фикстуры с реальных прогонов.
5. **Headless.** <https://moonshotai.github.io/kimi-code/en/reference/kimi-command.html>
   - `kimi -p "<prompt>" --output-format text|stream-json` (в этом режиме подтверждения отключены), продолжение: `-S/--session <id>` и `-c/--continue`.
   - `session_id` печатается в stderr (text) или строкой `session.resume_hint` (stream-json). Код выхода 4 — сессия занята.
   - В хуке id берётся из `session_id`. Переменной окружения с id нет. В MCP id не передаётся.
   - Есть также `kimi acp` (Agent Client Protocol) для управления из редактора.
6. **Рекомендация: полная.** `zam init --agent kimi` пишет:
   - блок в `AGENTS.md` (общий с Codex);
   - `.kimi-code/mcp.json`;
   - хуки — только на уровне пользователя. Лучше пользовательский плагин `zam` (каталог с `kimi.plugin.json` и `hooks[]`, установка `/plugins install <path>`, копируется в `~/.kimi-code/plugins/managed/zam/`), чем правка `config.toml`: строгая схема `[[hooks]]` грозит сломать конфиг. Поскольку хук глобальный, `zam hook` должен молча выходить, если `cwd` не привязан к решению.

   Пример (TOML, если всё же писать в `config.toml`):
   ```toml
   [[hooks]]
   event = "SessionStart"
   command = "zam hook session-start --agent kimi"
   timeout = 10
   [[hooks]]
   event = "PreToolUse"
   matcher = "Bash|Write|Edit"   # имена инструментов Kimi — не подтверждено
   command = "zam hook pre-tool --agent kimi"
   timeout = 5
   ```
   **Риски:**
   - высокий темп выпусков (2.6→2.8 за неделю);
   - хуки и плагины не бывают проектными;
   - схема `wire.jsonl` не документирована;
   - плагин копируется при установке, поэтому после обновления `zam` нужна переустановка.

## 3. Qwen Code (Alibaba)

Источники: <https://github.com/QwenLM/qwen-code> (последняя стабильная v0.29.0 от 28 ноября; год на странице не указан), документация <https://qwenlm.github.io/qwen-code-docs/en/>. У сайта есть ветка «1.x» — это метка ветки документации, а не версия CLI. Qwen Code — форк Gemini CLI.

1. **Инструкции.** <https://qwenlm.github.io/qwen-code-docs/en/users/features/memory/> (обновлено 2026-11-13)
   - Файлы: `~/.qwen/QWEN.md` (глобально), `QWEN.md` в корне проекта, `.qwen/QWEN.local.md` (личный, загружается после проектного). Импорт: `@path`.
   - Существующий `AGENTS.md` тоже читается («Qwen reads that too»). Настройка `context.fileName` (строка или массив, по умолчанию `undefined`) задаёт другие имена.
   - CLAUDE.md не упомянут — не подтверждено.
2. **MCP.** <https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/> (2026-10-19)
   - Задаётся в `mcpServers` файла `.qwen/settings.json` (проект) или `~/.qwen/settings.json` (пользователь). Команда: `qwen mcp add -s project zam zam mcp`.
   - Поля: `command`, `args`, `env`, `cwd`, `timeout` (мс, по умолчанию 600000), `trust`, `includeTools`/`excludeTools`.
   - Транспорты: stdio, `httpUrl` (Streamable HTTP), `url` (устаревший SSE). Пределов числа инструментов в документации нет.
   ```json
   { "mcpServers": { "zam": { "command": "zam", "args": ["mcp"], "timeout": 30000 } } }
   ```
3. **Хуки.** <https://raw.githubusercontent.com/QwenLM/qwen-code/main/docs/users/features/hooks.md>, <https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/>
   - **Структура** — как у Claude: `"hooks": { "<Event>": [ { "matcher": "...", "hooks": [ { "type": "command", "command": "...", "timeout": 10 } ] } ] }`. Типы обработчиков: `command`, `http`, `prompt`.
   - **Где задаются:** `.qwen/settings.json`, `.qwen/settings.local.json`, `~/.qwen/settings.json`, системные файлы, frontmatter скиллов и субагентов. Проектные хуки требуют одобрения (`~/.qwen/hookApprovals.json`) и доверия к папке. `disableAllHooks` отключает все.
   - **События (23):** `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `UserPromptSubmit`, `UserPromptExpansion`, `SessionStart` (`startup`/`resume`/`clear`/`compact`), `SessionEnd`, `SessionDelete`, `MessageDisplay`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, `Notification`, `PermissionRequest`, `PermissionDenied`, `TodoCreated`, `TodoCompleted`, `InstructionsLoaded`, `FileChanged`.
   - **Вход:** `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `timestamp`, `permission_mode`, внутри субагента ещё `agent_id`. По событиям:
     - `UserPromptSubmit`: `prompt`;
     - `PreToolUse`: `tool_name`, `tool_input`, `tool_use_id`;
     - `PostToolUseFailure`: `error`, `is_interrupt`, `duration_ms`;
     - `SubagentStart`: `agent_id`, `agent_type`;
     - `PreCompact`: `trigger`;
     - `SessionEnd`: `reason`.
   - **Ответ:** код 0 — stdout разбирается как JSON. Код 2 — блокировка, stderr уходит модели. JSON-поля: `continue`, `stopReason`, `systemMessage`, `decision`, `reason`, `hookSpecificOutput{hookEventName, additionalContext, permissionDecision: allow|deny|ask}`.
   - **Совместимость с Claude:** матчеры принимают имена инструментов Claude (`Bash`, `Read`, `Write`, `Edit`); хукам выставляются `QWEN_PROJECT_DIR`, `CLAUDE_PROJECT_DIR`, `GEMINI_PROJECT_DIR`; есть `qwen hooks migrate --from-claude`. В v0.28.7 появилась загрузка модов Claude Code.
   - **Соответствие нашим событиям:** все 10 — один к одному.
4. **Журналы.** Исходники: <https://raw.githubusercontent.com/QwenLM/qwen-code/main/packages/core/src/services/chatRecordingService.ts>, <https://raw.githubusercontent.com/QwenLM/qwen-code/main/packages/core/src/config/storage.ts>
   - **Путь:** `<runtimeBaseDir>/projects/<sanitizeCwd(projectRoot)>/chats/<sessionId>.jsonl`, по умолчанию `~/.qwen/…`. Переопределяется через `QWEN_HOME` и `QWEN_RUNTIME_DIR`.
   - Комментарий в коде называет другой путь, `~/.qwen/tmp/<project_id>/chats/`. Из-за этого расхождения путь надёжнее брать из `transcript_path` хука.
   - **Запись `ChatRecord`:** `uuid`, `parentUuid` (дерево; ветки после rewind остаются), `sessionId`, `timestamp` (ISO 8601), `type` (`user|assistant|tool_result|system`), `subtype` (`chat_compression`, `custom_title`…), `message` (role и parts в формате API), `cwd`, `version`, `gitBranch`, `toolCallResult`, `usageMetadata`, `model`, `promptId`. Файл только дописывается.
   - Дополнительно есть структурные логи `~/.qwen/logs/sessions/<projectId>/<sessionId>/segments/<runId>.jsonl` (<https://qwenlm.github.io/qwen-code-docs/1.x/en/users/data-privacy/>).
5. **Headless.** <https://qwenlm.github.io/qwen-code-docs/en/users/features/headless/> (2026-11-13)
   - `qwen -p "<prompt>" -o json|stream-json`, `--input-format stream-json`, `--approval-mode yolo` или `-y`, `--continue`, `--resume`.
   - В JSON у каждого сообщения есть `session_id`. `--session-id` не документирован. `--safe-mode` отключает хуки и MCP.
   - В хуке id берётся из `session_id`.
6. **Рекомендация: полная, первая в очереди.** `zam init --agent qwen` пишет:
   - блок в `AGENTS.md` (Qwen его читает; отдельный `QWEN.md` не нужен);
   - `.qwen/settings.json`: слияние `mcpServers.zam` и `hooks` в формате Claude, с теми же матчерами `Write|Edit|Bash`.

   Ответ хука — тот же рендерер, что для Claude (`hookSpecificOutput`).

   **Риски:**
   - нужно одобрение проектных хуков (первый запуск интерактивный, а в headless-тестах — не подтверждено);
   - высокий темп выпусков;
   - путь к чатам в коде расходится с комментарием.

## 4. Google Antigravity (App, IDE, CLI `agy`)

Источники: индекс <https://antigravity.google/llms.txt>; страницы в виде `.md`: <https://antigravity.google/docs/hooks.md>, <https://antigravity.google/docs/rules.md>, <https://antigravity.google/docs/mcp.md>, <https://antigravity.google/docs/cli/headless.md>, <https://antigravity.google/docs/cli/commands/resume.md>, <https://antigravity.google/docs/cli/gcli-migration.md>, <https://antigravity.google/docs/changelog.md> (App v2.36.0 от 2026-11-27; версии IDE и CLI на странице нет — не подтверждено). Antigravity CLI (`agy`) — путь обновления с Gemini CLI.

1. **Инструкции.** Все найденные файлы объединяются, ничего не перекрывается.
   - **Рабочая папка** и её родители до корня репозитория: `AGENTS.md` или `GEMINI.md`, `.agents/AGENTS.md`, `.agents/GEMINI.md`, `.agents/rules/*.md` (нужен frontmatter `trigger`: `always_on`, `model_decision`, `glob`, `manual`; устаревшее `.agent/rules` тоже работает). Подпапки ниже рабочей не читаются.
   - **Глобально:** `~/.gemini/AGENTS.md`, `~/.gemini/GEMINI.md`, `~/.gemini/config/AGENTS.md`, `~/.gemini/config/GEMINI.md`, `~/.gemini/config/rules/*.md`.
   - **Пределы:** файл до 24 000 байт, бюджет глобальных и `always_on` правил — 20 000 токенов.
   - CLAUDE.md не упомянут — не подтверждено (App 2.30.0 умеет импортировать инструкции Claude Code, Codex и Cursor в плагин).
2. **MCP.**
   - Глобально: `~/.gemini/config/mcp_config.json`. Рабочая папка: `<workspace>/.agents/mcp_config.json`. Ещё плагины.
   - Транспорты: stdio и только Streamable HTTP (`serverUrl` или `url`). `httpUrl` и SSE не поддерживаются.
   - `timeoutSeconds`: подключение 300 с, вызов 180 с. Пределы числа инструментов в документации не указаны — не подтверждено.
   ```json
   { "mcpServers": { "zam": { "command": "zam", "args": ["mcp"] } } }
   ```
3. **Хуки.**
   - **Где задаются:** `.agents/hooks.json` (рабочая папка) и `~/.gemini/config/hooks.json` — общие для App, IDE и CLI; плюс `hooks.json` плагина (App и CLI). Ключ `hooks` в `~/.gemini/antigravity-cli/settings.json` не загружается.
   - **Структура:** верхний уровень — именованные наборы хуков, внутри них события. У инструментальных событий — `matcher` (регулярное выражение, `*` означает все) и массив `hooks`, у остальных — сразу список обработчиков. У обработчика: `type: "command"`, `command`, `timeout` (с, по умолчанию 30), `enabled`.
   - **События (всего 6):** `SessionStart`, `PreInvocation`, `PostInvocation`, `PreToolUse`, `PostToolUse`, `Stop`.
   - **Вход (camelCase):** `conversationId`, `workspacePaths`, `transcriptPath`, `artifactDirectoryPath`, `modelName`, `lastUserInput` (с 2.28.0). У инструментов: `toolCall.name`, `toolCall.args`, `stepIdx`; у `PostToolUse` ещё `error`. У `Stop`: `terminationReason`, `fullyIdle`.
   - **Ответ:**
     - `PreToolUse`: обязательное `decision` (`allow`, `auto_approve`, `deny`, `ask`, `force_ask`, `deny_unless_prior_grant`), `reason`, `overwrite`;
     - `SessionStart`, `PreInvocation`, `PostInvocation`: `injectSteps: [{ "systemMessage": { "systemMessage": "..." } }]` — **внедрение в контекст есть**, шаги сохраняются в истории, поэтому внедрять стоит только при изменениях;
     - `Stop`: `decision: "continue"` + `reason`.

     Коды выхода не документированы.
   - **Соответствие нашим событиям:**

     | Наше событие | Antigravity |
     |---|---|
     | `SessionStart` | `SessionStart` |
     | `UserPromptSubmit` | `PreInvocation` (текст — в `lastUserInput`, первый вызов хода — по `invocationNum`) |
     | `PreToolUse` | `PreToolUse` |
     | `PostToolUse` | `PostToolUse` |
     | `PostToolUseFailure` | `PostToolUse` с непустым `error` |
     | `Stop` | `Stop` |
     | `SubagentStart`, `PreCompact`, `PostCompact`, `SessionEnd` | **нет** |

   - Сторонние источники противоречат документации. Статья на Medium (<https://medium.com/google-cloud/a-developers-guide-to-agent-hooks-in-antigravity-cli-4c1440febd11>) говорит о сведении 11 старых хуков в 5. Страница SonarSource (<https://docs.sonarsource.com/sonarqube-cli/integrations/antigravity>) говорит, что внедрения в контекст нет. Официальная документация описывает `injectSteps`; противоречие не разрешено.
4. **Журналы.** `transcriptPath` = `<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl`.
   - `<app_data_dir>`: `~/.gemini/antigravity` (App), `~/.gemini/antigravity-cli` (CLI), `~/.gemini/antigravity-ide` (IDE).
   - **Поля JSONL не описаны — не подтверждено.**
   - Кэш CLI `~/.gemini/antigravity-cli/cache/last_conversations.json` сопоставляет путь рабочей папки с последним `conversationId` (UUID).
   - Пути Windows не указаны (вероятно `%USERPROFILE%\.gemini\…`) — не подтверждено.
5. **Headless** (только CLI).
   - `agy -p "<prompt>" --output-format text|json|stream-json`, `--input-format stream-json`, `--conversation <id>`, `-c`, `--dangerously-skip-permissions`, `--json-schema`.
   - JSON-конверт: `conversation_id`, `status`, `response`, `num_turns`, `usage`. События stream-json: `init`, `step_update`, `result`.
   - Работают ли хуки в `-p` — не подтверждено. В хуке id сессии — `conversationId`. Для App и IDE headless-режима нет. Есть `agy acp`.
6. **Рекомендация: полная, одна реализация на App, IDE и CLI** (общие `.agents/hooks.json` и `.agents/mcp_config.json`). `zam init --agent antigravity` пишет:
   - блок в `AGENTS.md`;
   - `.agents/mcp_config.json`;
   - `.agents/hooks.json` — пример ниже собран по описанию схемы, так как дословный пример получить не удалось.
   ```json
   {
     "zam": {
       "SessionStart":  [ { "type": "command", "command": "zam hook session-start --agent antigravity", "timeout": 10 } ],
       "PreInvocation": [ { "type": "command", "command": "zam hook prompt --agent antigravity", "timeout": 5 } ],
       "PreToolUse":    [ { "matcher": "run_command|write_to_file|replace_file_content",
                            "hooks": [ { "type": "command", "command": "zam hook pre-tool --agent antigravity", "timeout": 5 } ] } ],
       "PostToolUse":   [ { "matcher": "*", "hooks": [ { "type": "command", "command": "zam hook post-tool --agent antigravity" } ] } ],
       "Stop":          [ { "type": "command", "command": "zam hook checkpoint --agent antigravity" } ]
     }
   }
   ```
   Имена инструментов записи файлов (`write_to_file`, `replace_file_content`) не подтверждены; в документации есть только `run_command`, `view_file`, `browser_.*`.

   **Риски:**
   - продукт быстро меняется (2.17.0: настройки перенесены в `.gemini/config.json`; 2.33.0: на Windows игнорировался `hooks.json` с BOM — писать UTF-8 без BOM);
   - нет событий `SessionEnd`, `PreCompact`, `SubagentStart`;
   - `injectSteps` накапливаются в истории.

## 5. Cursor (IDE и CLI `agent`)

Источники: <https://cursor.com/docs/agent/hooks>, <https://cursor.com/docs/reference/third-party-hooks>, <https://cursor.com/docs/context/rules>, <https://cursor.com/docs/context/mcp>, <https://cursor.com/docs/cli/overview>, <https://cursor.com/docs/cli/using>, <https://cursor.com/docs/cli/headless>, <https://cursor.com/docs/cli/changelog> (CLI v2026.11.19). Версия IDE на страницах не указана — не подтверждено (на форуме упоминается 3.24.9). Даты у страниц документации нет.

1. **Инструкции.**
   - **IDE:** `.cursor/rules/**/*.mdc` (только `.mdc`; frontmatter `alwaysApply`, `globs`, `description`), `AGENTS.md` в корне и вложенные (объединяются с родительскими, более близкий важнее). Пользовательские и командные правила хранятся в интерфейсе или облаке, не в файлах. Порядок: Team → Project → User.
   - **CLI:** те же правила, плюс «also reads `AGENTS.md` and `CLAUDE.md` at the project root». Читает ли IDE CLAUDE.md — не подтверждено.
2. **MCP.**
   - Файлы: `.cursor/mcp.json` (проект) и `~/.cursor/mcp.json` (глобально; работает в IDE и CLI). CLI сам подхватывает `mcp.json`.
   - Поля: `type: "stdio"` (в таблице обязательное), `command`, `args`, `env`, `envFile`. Подстановки `${workspaceFolder}` и `${env:…}`.
   - Транспорты: stdio, SSE, Streamable HTTP. Предел числа инструментов (раньше говорили о 40) в текущей документации не указан — не подтверждено. В headless без `--force` или разрешений MCP-инструменты отклоняются.
   ```json
   { "mcpServers": { "zam": { "type": "stdio", "command": "zam", "args": ["mcp"] } } }
   ```
3. **Хуки.**
   - **Где задаются** (по убыванию приоритета): Enterprise (`/etc/cursor/hooks.json`, `C:\ProgramData\Cursor\hooks.json`), Team, `.cursor/hooks.json`, `~/.cursor/hooks.json`, затем хуки Claude: `.claude/settings.local.json`, `.claude/settings.json`, `~/.claude/settings.json`. Чтение хуков Claude включено по умолчанию (Settings → Agents → Third-Party Imports); в CLI его, по сообщению на форуме, нельзя отключить.
   - **Формат:** `{ "version": 1, "hooks": { "<event>": [ { "command": "...", "type"?, "timeout"?, "matcher"?, "loop_limit"?, "failClosed"? } ] } }`.
   - **События агента:** `sessionStart`, `sessionEnd`, `preToolUse`, `postToolUse`, `postToolUseFailure`, `subagentStart`, `subagentStop`, `beforeShellExecution`, `afterShellExecution`, `beforeMCPExecution`, `afterMCPExecution`, `beforeReadFile`, `afterFileEdit`, `beforeSubmitPrompt`, `preCompact`, `stop`, `afterAgentResponse`, `afterAgentThought`; плюс `workspaceOpen` и Tab-хуки.
   - **Вход (общие поля):** `conversation_id`, `generation_id`, `model`, `hook_event_name`, `cursor_version`, `workspace_roots`, `user_email`, `transcript_path`. Переменные окружения: `CURSOR_PROJECT_DIR`, `CURSOR_VERSION`, `CURSOR_TRANSCRIPT_PATH`, `CLAUDE_PROJECT_DIR`.
   - **Ответ по событиям:**
     - `sessionStart`: `additional_context` («к начальному системному контексту»), но хук исполняется без ожидания ответа — успеет ли контекст попасть в первый ход, не подтверждено;
     - `beforeSubmitPrompt`: только `continue` и `user_message` — **добавить контекст к запросу нельзя**;
     - `preToolUse`: `permission: allow|deny`, `updated_input`, `agent_message` (`ask` пока не исполняется);
     - `postToolUseFailure`: `additional_context`;
     - `subagentStart`: только `permission` — контекста нет;
     - `stop`: `followup_message` (автоматически отправляется, `loop_limit` 5);
     - `preCompact`: только наблюдение.

     Код 2 блокирует (как у Claude).
   - **Соответствие нашим событиям:**

     | Наше событие | Cursor |
     |---|---|
     | `SessionStart` | `sessionStart` |
     | `UserPromptSubmit` | `beforeSubmitPrompt` (без внедрения; подсказки — через `sessionStart` или `postToolUse`) |
     | `PreToolUse` | `preToolUse` (также `beforeShellExecution`, `beforeReadFile`) |
     | `PostToolUse` | `postToolUse` / `afterFileEdit` |
     | `PostToolUseFailure` | `postToolUseFailure` |
     | `SubagentStart` | `subagentStart` |
     | `Stop` | `stop` |
     | `PreCompact` | `preCompact` |
     | `PostCompact` | **нет** |
     | `SessionEnd` | `sessionEnd` |

   - **В CLI:** с января 2026 поддерживаются хуки сессии, stop, pre-compact и субагентов, читаются `settings.json` Claude; с апреля 2026 принимаются ответы в формате Claude.
   - **Облачные агенты** запускают только командные хуки репозитория, без `sessionStart`/`sessionEnd` и без `~/.cursor`.
4. **Журналы.**
   - Официально: путь приходит в `transcript_path` и `CURSOR_TRANSCRIPT_PATH` (`null`, если транскрипты отключены). В CLI транскрипты сохраняются на диск с января 2026; транскрипты headless — «Claude Code-compatible JSONL» (февраль 2026); субагенты — в `agent-transcripts/<parent>/subagents/`.
   - Неофициально (форум и парсер codeburn): `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl` (раньше плоские `.txt`), без вывода инструментов; метаданные чатов IDE лежат в SQLite `state.vscdb`.
   - Схема полей официально не описана.
5. **Headless.**
   - `agent -p "<prompt>" --output-format text|json|stream-json [--stream-partial-output] --force|--yolo`, `--resume <chatId>`, `--continue`, `agent ls`, ключ `CURSOR_API_KEY`.
   - События stream-json: `type` (`system`, `assistant`, `tool_call`, `result`) и `subtype`. Поле `session_id` на странице не показано — не подтверждено.
   - Id в хуке: `conversation_id`, он же `session_id` в `sessionStart`.
6. **Рекомендация: полная.** `zam init --agent cursor` пишет:
   - блок в `AGENTS.md` (IDE и CLI) или `.cursor/rules/zam.mdc` с `alwaysApply: true`;
   - `.cursor/mcp.json`;
   - `.cursor/hooks.json`:
   ```json
   { "version": 1, "hooks": {
     "sessionStart":       [ { "command": "zam hook session-start --agent cursor", "timeout": 10 } ],
     "beforeSubmitPrompt": [ { "command": "zam hook prompt --agent cursor", "timeout": 5 } ],
     "preToolUse":         [ { "matcher": "Shell|Write", "command": "zam hook pre-tool --agent cursor", "timeout": 5 } ],
     "postToolUse":        [ { "command": "zam hook post-tool --agent cursor" } ],
     "postToolUseFailure": [ { "command": "zam hook post-tool-failure --agent cursor" } ],
     "subagentStart":      [ { "command": "zam hook subagent-start --agent cursor" } ],
     "preCompact":         [ { "command": "zam hook checkpoint --agent cursor" } ],
     "stop":               [ { "command": "zam hook checkpoint --agent cursor" } ],
     "sessionEnd":         [ { "command": "zam hook session-end --agent cursor" } ]
   } }
   ```
   **Главный риск — двойной вызов.** Если в проекте уже есть хуки Claude (`.claude/settings.json`), Cursor запустит и их, причём с полезной нагрузкой в формате Cursor: на форуме описано, что PreToolUse-ворота при этом пропускают действие (fail-open), а на Windows команда собирается для PowerShell, но исполняется через bash. Нужно:
   - `zam hook --agent claude` при `CURSOR_VERSION` в окружении или `cursor_version` в payload молча выходит;
   - адаптер Cursor работает только из `.cursor/hooks.json`.

   Другие риски: частые выпуски CLI; поведение хуков без `matcher` (на форуме сообщают о пропуске).

## 6. GitHub Copilot (VS Code, Copilot CLI, cloud agent)

Источники:
- VS Code (страницы от 2026-11-25): <https://code.visualstudio.com/docs/copilot/customization/hooks> (preview), <https://code.visualstudio.com/docs/copilot/customization/custom-instructions>, <https://code.visualstudio.com/docs/agents/reference/mcp-configuration>, <https://code.visualstudio.com/docs/agents/overview>, <https://code.visualstudio.com/docs/agents/run/sessions/session-history>;
- GitHub Docs (без даты): <https://docs.github.com/en/copilot/reference/hooks-reference>, <https://docs.github.com/en/copilot/reference/custom-instructions-support>, <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers>, <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference>, <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference>, <https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/extend-coding-agent-with-mcp>.

Важная перемена 2026 года: в VS Code агент выбирает «harness» — Copilot, Claude, Codex или Local. Local устарел и будет удалён в феврале 2027. **Copilot harness — общий у VS Code, Copilot CLI и приложения GitHub Copilot**, с общими форматами хуков и MCP. Claude и Codex harness в VS Code используют собственные хуки (`.claude/settings.json`, AGENTS.md), так что наша текущая интеграция Claude и Codex там уже работает.

1. **Инструкции.**
   - `.github/copilot-instructions.md`, `.github/instructions/**/*.instructions.md` (`applyTo`) и `AGENTS.md` — на всех поверхностях.
   - `CLAUDE.md` и `GEMINI.md` — CLI и cloud agent (для Copilot Chat в VS Code не указаны).
   - Личные: `~/.copilot/copilot-instructions.md`, `~/.copilot/instructions/` (только CLI).
   - Порядок приоритета официально не задан («Don't rely on a universal precedence order»).
2. **MCP.**
   - **CLI:** `~/.copilot/mcp-config.json` (`copilot mcp add zam -- zam mcp`), `.github/mcp.json` (общий), `.mcp.json` в любой папке от `cwd` до корня репозитория (перекрывает `.github/mcp.json`). **Тот же файл `.mcp.json`, что у Claude Code.** `.vscode/mcp.json` CLI не читает.
   - Поля: `type: local|stdio|http|sse`, `command`, `args`, `env`, `tools: ["*"]`.
   - Проектные серверы грузятся только после доверия к папке; в `-p` — при доверии, либо с `GITHUB_COPILOT_PROMPT_MODE_WORKSPACE_MCP=true`.
   - **VS Code (Copilot harness):** `~/.copilot/mcp-config.json` и корневой `.mcp.json` (stdio и HTTP; в мастере настройки — при экспериментальном `chat.mcp.workspaceRootConfig.enabled`). `.vscode/mcp.json` с ключом `servers` — наследие Local. Предел 128 инструментов в текущей документации не найден — не подтверждено.
   - **Cloud agent:** JSON только в настройках репозитория на GitHub.com (Settings → Copilot → MCP servers); поддерживаются только tools; секреты с префиксом `COPILOT_MCP_`; агент работает в одноразовой Linux-песочнице GitHub с файрволом, **поэтому сервер в LAN ему недоступен**.
   ```json
   { "mcpServers": { "zam": { "type": "stdio", "command": "zam", "args": ["mcp"], "tools": ["*"] } } }
   ```
   Терпит ли Claude Code лишнее поле `tools` в общем `.mcp.json` — не подтверждено; проверить тестом.
3. **Хуки.**
   - **Где задаются:**
     - CLI: `.github/hooks/*.json`, `~/.copilot/hooks/*.json`, встроенный `hooks` в `.github/copilot/settings(.local).json` и `~/.copilot/settings.json`, плагины, политики (`/etc/github-copilot/policy.d/`, `C:\ProgramData\GitHub\Copilot\policy.d\`);
     - VS Code: то же и **`.claude/settings(.local).json`** (совместимость; отключение — `chat.useHooks: false`);
     - cloud agent: только `.github/hooks/*.json`, только `bash` или `command`.
   - **Формат:** `{ "version": 1, "hooks": { "<event>": [ { "type": "command", "command" | "bash" | "powershell": "...", "cwd"?, "env"?, "timeoutSec": 30 } ] } }`. Есть тип `http`. События в camelCase; PascalCase — совместимый псевдоним с полями в snake_case.
   - **События и вход:**

     | Событие | Входные поля |
     |---|---|
     | `sessionStart` | `source`, `initialPrompt` |
     | `sessionEnd` | `reason` |
     | `userPromptSubmitted` | `prompt` |
     | `userPromptTransformed` | — |
     | `preToolUse` | `toolName`, `toolArgs` |
     | `postToolUse` | `toolResult` |
     | `postToolUseFailure` | `error` |
     | `permissionRequest` | только CLI |
     | `agentStop` | `stopReason`, `stop_hook_active` |
     | `subagentStart` | `agentName` |
     | `subagentStop` | — |
     | `errorOccurred` | — |
     | `preCompact` | `trigger` |
     | `notification` | — |

     Общие поля: `sessionId`, `timestamp`, `cwd`. `transcriptPath` есть только в `agentStop`, `subagentStart`, `subagentStop`, `preCompact`.
   - **Ответ:**
     - `additionalContext` — в `sessionStart`, `subagentStart` (в начало первого сообщения субагента), `postToolUse`, `postToolUseFailure` (через код 2), `notification`;
     - `preToolUse`: `permissionDecision` (`allow|deny|ask`; в cloud agent `ask` = `deny`), `permissionDecisionReason`, `modifiedArgs`;
     - `agentStop`: `decision: "block"` + `reason`;
     - **`userPromptSubmitted` у командных хуков ничего не внедряет** (`modifiedPrompt` работает только в SDK);
     - `sessionEnd` и `preCompact` вывод не обрабатывают.

     Коды выхода: 0 — успех. 2 — предупреждение, но для `preToolUse` — отказ. Другой ненулевой код пропускает действие, кроме `preToolUse`, где это отказ (fail-closed). Таймаут пропускает действие.
   - **Соответствие нашим событиям:**

     | Наше событие | Copilot |
     |---|---|
     | `SessionStart` | `sessionStart` |
     | `UserPromptSubmit` | `userPromptSubmitted` (только наблюдение; подсказку отдавать через `postToolUse.additionalContext`) |
     | `PreToolUse` | `preToolUse` |
     | `PostToolUse` | `postToolUse` |
     | `PostToolUseFailure` | `postToolUseFailure` |
     | `SubagentStart` | `subagentStart` |
     | `Stop` | `agentStop` |
     | `PreCompact` | `preCompact` |
     | `PostCompact` | **нет** |
     | `SessionEnd` | `sessionEnd` |

   - Matcher для командных хуков не документирован — фильтровать инструменты внутри `zam hook`.
4. **Журналы.**
   - **CLI:** `~/.copilot/session-state/<sessionId>/events.jsonl` (журнал событий; рядом планы, контрольные точки, отслеживаемые файлы) и `~/.copilot/session-store.db` (SQLite, индекс и поиск). Windows: `C:\Users\<user>\.copilot\`. Каталог переносится через `COPILOT_HOME`. **Схема `events.jsonl` официально не описана.** По данным стороннего крейта (неофициально), до версии 1.0 был плоский `session-state/<uuid>.jsonl`.
   - **VS Code:** локальная SQLite (путь не документирован) и экспорт «Chat: Export Chat…» в JSON.
   - **Cloud agent:** локального журнала нет.
5. **Headless.**
   - **CLI:** `copilot -p "<prompt>" --allow-all-tools|--yolo --output-format json` (JSONL), `-s`, `--resume=<id>`, `--continue`, `--trust-folder` (доверие хукам и проектному MCP на один неинтерактивный запуск). Поле id сессии в JSON не описано — не подтверждено. В хуке id берётся из `sessionId`.
   - **VS Code и cloud agent:** headless для тестов нет; cloud agent запускается назначением задачи (issue) на GitHub.
6. **Рекомендация.**
   - **Copilot CLI и VS Code (Copilot harness) — полная, одной реализацией.** `zam init --agent copilot` пишет:
     - блок в `AGENTS.md`;
     - `.mcp.json` (уже общий с Claude) или `.github/mcp.json`;
     - `.github/hooks/zam.json`:
     ```json
     { "version": 1, "hooks": {
       "sessionStart":        [ { "type": "command", "command": "zam hook session-start --agent copilot", "timeoutSec": 10 } ],
       "userPromptSubmitted": [ { "type": "command", "command": "zam hook prompt --agent copilot", "timeoutSec": 5 } ],
       "preToolUse":          [ { "type": "command", "command": "zam hook pre-tool --agent copilot", "timeoutSec": 5 } ],
       "postToolUse":         [ { "type": "command", "command": "zam hook post-tool --agent copilot", "timeoutSec": 5 } ],
       "postToolUseFailure":  [ { "type": "command", "command": "zam hook post-tool-failure --agent copilot" } ],
       "subagentStart":       [ { "type": "command", "command": "zam hook subagent-start --agent copilot" } ],
       "preCompact":          [ { "type": "command", "command": "zam hook checkpoint --agent copilot" } ],
       "agentStop":           [ { "type": "command", "command": "zam hook checkpoint --agent copilot" } ],
       "sessionEnd":          [ { "type": "command", "command": "zam hook session-end --agent copilot" } ]
     } }
     ```
     Внимание: `preToolUse` закрывается при ошибке (fail-closed). Если `zam` недоступен или упал, Copilot откажет в вызове инструмента, поэтому `zam hook pre-tool` обязан выходить с кодом 0 при любой внутренней ошибке.
   - **Cloud agent — только инструкции:** сервер памяти в LAN из облака недостижим, а `.github/hooks` там исполняется в одноразовой песочнице.
   - **Риски:**
     - хуки VS Code в статусе preview;
     - VS Code подхватывает `.claude/settings.json` — тот же риск двойного вызова, что у Cursor;
     - схема `events.jsonl` не описана;
     - переход с Local harness на Copilot harness до февраля 2027.

## 7. OpenCode

Источники: <https://opencode.ai/docs/rules/>, <https://opencode.ai/docs/mcp-servers/>, <https://opencode.ai/docs/config/>, <https://opencode.ai/docs/plugins/>, <https://opencode.ai/docs/build/plugins/server/>, <https://opencode.ai/v2/docs/cli/commands/>, <https://opencode.ai/docs/troubleshooting/>, выпуски <https://github.com/sst/opencode/releases>: v2.4.2 от 26 ноября. Значок «Latest» стоит на v1.19.6 от 17 ноября, а документация V1 отдельно лежит на <https://opencode.ai/v1/docs/>. Значит, **параллельно живут две ветки с несовместимым API плагинов и форматом конфига MCP**.

1. **Инструкции.**
   - Порядок загрузки: глобальный `~/.config/opencode/AGENTS.md`, затем все `AGENTS.md` от рабочей папки вверх (вне домашнего каталога — до корня проекта), затем поле `instructions` в конфиге (пути, глобы, URL).
   - В папке без `AGENTS.md` читается `CLAUDE.md`; без глобального `AGENTS.md` — `~/.claude/CLAUDE.md` (восстановлено в v2.3.1).
   - Все файлы объединяются без разрешения конфликтов. Отключение: `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1` и др.
2. **MCP.**
   - Файлы: `opencode.json(c)` или `.opencode/opencode.json(c)` в проекте, `~/.config/opencode/opencode.json(c)` глобально. Команда: `opencode mcp add zam -- zam mcp` (`--global` — для всех проектов).
   - В V2 серверы лежат в `mcp.servers` («V2 does not place server names directly under `mcp`»).
   - Транспорты: stdio и Streamable HTTP. По умолчанию включён Code Mode (`codemode: false` отдаёт инструменты модели напрямую). Числовых пределов нет, есть предупреждение о расходе контекста.
   ```jsonc
   { "$schema": "https://opencode.ai/config.json",
     "mcp": { "servers": { "zam": { "type": "local", "command": ["zam", "mcp"] } } } }
   ```
   В V1 сервер задаётся как `"mcp": { "zam": { "type": "local", "command": [...], "enabled": true } }`. Это по памяти о старой документации; на странице V1 не перепроверено — не подтверждено.
3. **Хуки** — командных хуков нет, есть плагины.
   - **Плагины:** TS/JS в `.opencode/plugins/` или `~/.config/opencode/plugins/`, либо npm-пакеты в `"plugins": [...]`. Вид: `Plugin.define({ id, setup(ctx) })`; `ctx.location.directory`, `ctx.location.project`, `ctx.event.subscribe`.
   - **Хуки V2:**
     - `ctx.session.hook("prompt")` — допуск запроса пользователя: можно править текст, добавлять `files` и `metadata`; типизированного отказа нет;
     - `"context"` — перед каждым вызовом модели: `event.system.push({type:"text", text})`, правка `messages` и `tools`; изменения не сохраняются в истории;
     - `"compaction"`, `"fork"`, `"retry"`, `"title"`;
     - инструменты: `execute.before` (`tool`, `input`), `execute.after` (`status: completed|error`, `error.message`) — регистрация через `ctx.tool.hook(...)` предположительна, не подтверждено;
     - `permission.hook("evaluate")` — `effect: allow|ask|deny` и `message`; так блокируется действие;
     - устойчивый контекст: `ctx.session.instructions.entry.put({ sessionID, key, value })`.
   - События начала и конца сессии, простоя и субагентов — только через `ctx.event.subscribe`; имена событий не документированы — не подтверждено.
   - Хуки V1 (`"tool.execute.before"` и т. п.) в V2 не переводятся.
   - **Соответствие нашим событиям:**

     | Наше событие | OpenCode V2 |
     |---|---|
     | `UserPromptSubmit` | `prompt` |
     | контекст в каждый ход | `context` |
     | `PreToolUse` | `execute.before` + `permission evaluate` |
     | `PostToolUse`, `PostToolUseFailure` | `execute.after` |
     | `PreCompact` | `compaction` |
     | `SessionStart`, `SessionEnd`, `SubagentStart`, `Stop` | `event.subscribe` (не подтверждено) |
     | `PostCompact` | **нет** |

4. **Журналы.**
   - SQLite `~/.local/share/opencode/opencode.db`. Путь можно переопределить через `OPENCODE_DB` и узнать командой `opencode debug paths db`. В v2.4.0 части ответа ассистента хранятся отдельными строками.
   - Путь на Windows не документирован — не подтверждено.
   - Прямой разбор БД хрупок. Устойчивый путь: `opencode session export <id>` (JSON; схема не описана) или HTTP API `opencode serve`.
5. **Headless.**
   - `opencode run "<prompt>" --format json` (NDJSON) `--auto --session <ses_…> --continue --model --agent --archived`, `opencode session list --format json`, `opencode serve` (HTTP API и SDK).
   - Поле `sessionID` в событиях JSON подтверждают только сторонние парсеры — не подтверждено.
   - В плагине id сессии — `sessionID` в событиях.
6. **Рекомендация: сначала MCP + инструкции** (`AGENTS.md` + `opencode.jsonc` с `mcp.servers.zam`). Затем полная — через плагин-прослойку `.opencode/plugins/zam.ts`, который:
   - вызывает `zam hook <event> --agent opencode` и передаёт нормализованный JSON (или обращается к HTTP API сервера);
   - внедряет подсказки через `context` и `instructions.entry.put`.

   Писать `opencode.jsonc` нужно с сохранением комментариев (JSONC).

   **Риски:**
   - две ветки, V1 и V2, с разными API и конфигом;
   - плагин требует среды выполнения JS/TS внутри OpenCode (встроенный Bun — не подтверждено), что противоречит самодостаточности `zam` на Rust;
   - хранилище SQLite с меняющейся схемой.

## 8. Общие выводы для проекта M8

1. **AGENTS.md — общий знаменатель.** Его читают Codex, Kimi Code, Qwen Code, Antigravity, Cursor (IDE и CLI), Copilot (все поверхности) и OpenCode.
   - Предложение: сделать `AGENTS.md` каноническим носителем блока ZAM, а в `CLAUDE.md` писать только импорт `@AGENTS.md` (Claude Code поддерживает `@`-импорт).
   - Зачем: Cursor CLI, Copilot CLI и cloud agent читают **оба** файла, и сейчас блок попал бы в контекст дважды (лишние токены). OpenCode при наличии `AGENTS.md` не читает `CLAUDE.md`.
   - Ограничение: Antigravity режет файл правил на 24 000 байт; блок ZAM должен быть компактным.
2. **MCP — один генератор с тремя форматами.**
   - Семейство `mcpServers{command,args}` покрывает почти всех. Различаются путь и мелочи: `type: stdio` у Cursor, `type` + `tools` у Copilot, `timeout` у Qwen.
   - Пути: Claude `.mcp.json`, Kimi `.kimi-code/mcp.json`, Qwen `.qwen/settings.json` (слияние внутрь), Antigravity `.agents/mcp_config.json`, Cursor `.cursor/mcp.json`, Copilot CLI и VS Code (Copilot harness) — `.mcp.json` (общий с Claude) или `.github/mcp.json`.
   - Особняком: OpenCode V2 `mcp.servers{type:"local",command:[...]}` в JSONC и устаревший `servers` в `.vscode/mcp.json` (не писать).
   - Id сессии по MCP не передаёт ни один агент. Связь MCP и сессии остаётся за хуками (регистрация «текущей сессии» по `cwd`), как сейчас.
3. **Хуки — один обобщённый адаптер «нормализатор + рендерер».**
   - **Вход:** разбор нескольких диалектов во внутреннее `HookEvent{agent, event, session_id, cwd, prompt, tool{name,input}, error, transcript_path}`:
     - snake_case Claude (Claude, Codex, Qwen, Kimi);
     - camelCase Copilot (`sessionId`, `toolName`, `toolArgs`);
     - Cursor (`conversation_id`, `cursor_version`);
     - Antigravity (`conversationId`, `toolCall.{name,args}`, `lastUserInput`).
   - **Выход:**
     - `hookSpecificOutput{additionalContext, permissionDecision}` — Claude, Qwen, Kimi;
     - `{additional_context, permission, agent_message, followup_message}` — Cursor;
     - верхнеуровневые `{additionalContext, permissionDecision}` — Copilot;
     - `{injectSteps:[{systemMessage:{systemMessage}}], decision}` — Antigravity;
     - плагин-прослойка — OpenCode.
   - **Таблица событий** — данные (агент × наше событие → имя у агента, может ли внедрять контекст, может ли блокировать), а не код. Это позволит покрыть всех агентов одним набором фикстур.
   - **Внедрение на каждом запросе** есть у Claude, Qwen, Kimi (stdout) и Antigravity (`PreInvocation`). У Cursor и Copilot его нет: подсказку нужно переносить в `sessionStart` и `postToolUse`/`postToolUseFailure.additionalContext`. Это нужно учесть в логике подсказок M4.5 и M7.5.
   - **Определять агента** по `--agent` плюс проверка payload и окружения (`CURSOR_VERSION`, `cursor_version`, camelCase-поля). Причина: **Cursor и VS Code (Copilot harness) сами исполняют хуки из `.claude/settings.json`**. Хук `--agent claude`, вызванный не из Claude, должен молча выходить, иначе будут двойные записи и неверно разобранный payload. Нужен тест «хук Claude получает payload Cursor или Copilot».
   - **Отказоустойчивость:** у Copilot `preToolUse` закрывается при ошибке; у Kimi, Qwen, Cursor и Antigravity ошибка пропускает действие. Поэтому `pre-tool` всегда должен выходить с кодом 0 и `{}` при внутренних ошибках и укладываться в 1–5 с.
   - **Кодировка:** файлы конфигурации писать в UTF-8 без BOM (на Windows Antigravity до 2.33 игнорировал файлы с BOM).
4. **Журналы и снимки — путь из хука, разбор с допусками.**
   - Почти везде путь к журналу приходит в payload хука: `transcript_path` у Qwen и Cursor, `transcriptPath` у Antigravity и Copilot (в Stop и PreCompact), `transcript_path` у Kimi (в Stop). Не искать каталоги эвристиками, а запоминать путь из первого же хука сессии.
   - Форматы: JSONL у Kimi (`wire.jsonl`), Qwen (`ChatRecord`, схема в исходниках), Antigravity, Cursor (заявлен совместимым с Claude) и Copilot (`events.jsonl`); SQLite у OpenCode (лучше `opencode session export`) и у VS Code.
   - Схемы, кроме Qwen, официально не описаны. Нужны версионированные парсеры с деградацией (неизвестная запись пропускается, снимок строится по тому, что распознано) и фикстуры из реальных прогонов: по одному журналу на агента, по образцу разрешения 17a.
5. **Что можно делать одной реализацией.**
   - Диалект Claude: Claude + Codex + Qwen (почти без изменений) + Kimi (тот же JSON, только обёртка TOML или плагин).
   - Copilot CLI = Copilot harness в VS Code; cloud agent — подмножество.
   - Antigravity App, IDE и CLI — одна реализация (общие `.agents/*`).
   - Cursor IDE и CLI — одна.
   - OpenCode — отдельная.
6. **Тестирование.**
   - Основа — фикстуры payload по каждому диалекту плюс ожидаемый stdout, без живых агентов в CI (всем нужны аккаунты и ключи).
   - Живые дымовые прогоны headless (`qwen -p`, `kimi -p`, `agy -p`, `agent -p`, `copilot -p`, `opencode run`) — у владельца, по желанию.
   - Идея: Kimi (`kimi acp`) и Antigravity (`agy acp`) поддерживают Agent Client Protocol. Через ACP можно сделать единый испытательный стенд для нескольких агентов.
7. **Порядок по отношению ценность/затраты.**
   1. **Qwen Code** — полная почти даром: тот же диалект, проектные файлы, документированная схема журнала.
   2. **Copilot CLI + VS Code** — большая аудитория, один формат на две поверхности, `.mcp.json` уже общий. Средние затраты (свой рендерер, fail-closed, нет внедрения на запросе). Cloud agent — только инструкции.
   3. **Cursor** — большая аудитория, богатые хуки. Средние затраты; обязательна защита от двойного вызова через `.claude/settings.json`.
   4. **Kimi Code** — тот же диалект, но хуки только пользовательские (плагин). Средние затраты.
   5. **Antigravity** — свой диалект и меньше событий, но одна реализация на три продукта. Средние затраты.
   6. **OpenCode** — сразу MCP + инструкции (дёшево); полная — последней: TS-плагин, две ветки V1 и V2, SQLite.

   Вне зависимости от порядка сначала стоит сделать **общий шаг**: генератор `AGENTS.md` и MCP для всех агентов (уровень «MCP + инструкции» — малые затраты, сразу 6 агентов), затем нормализатор и рендерер хуков.
8. **Не подтверждено (нужна проверка на реальных версиях):**
   - схемы `wire.jsonl` (Kimi), `transcript.jsonl` (Antigravity), `events.jsonl` (Copilot), транскриптов Cursor, выгрузки OpenCode;
   - имена инструментов Kimi и Antigravity для матчеров записи файлов;
   - чтение `CLAUDE.md` в Kimi, Qwen, Antigravity и Cursor IDE;
   - успевает ли `additional_context` из `sessionStart` у Cursor попасть в первый ход (хук исполняется без ожидания);
   - работают ли хуки в headless у Antigravity, Copilot (кроме `--trust-folder`) и Qwen (одобрение проектных хуков);
   - поле id сессии в JSON-выводе Cursor, Copilot и OpenCode;
   - терпимость Claude Code к полю `tools` в общем `.mcp.json`;
   - читает ли VS Code корневой `.mcp.json` без экспериментальной настройки;
   - формат MCP в OpenCode V1;
   - пути Windows для Antigravity и OpenCode;
   - версии Antigravity IDE и CLI и Cursor IDE;
   - пределы числа инструментов MCP (ни у одного агента не найдены в текущей документации).
