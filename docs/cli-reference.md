# ZeroAIMemory: справочник CLI, REST API и MCP

Подробный справочник с флагами и версиями, с которых они работают (перенесено из README 2026-11-28). Обзор команд по задачам — [README](../README.md#команды-zam); точные флаги — `zam <команда> --help`.

## CLI `zam`

Адрес и токен `zam` ищет по порядку: флаги `--url`/`--token`, переменные `ZAM_URL`/`ZAM_TOKEN`,
блок в `CLAUDE.local.md`, `CLAUDE.md` или `AGENTS.md` от текущего каталога вверх, затем
`~/.zeroaimemory/config.toml` (токен оттуда — только для найденного адреса). У всех команд есть
`--json`.

Цель команды — `[пространство:]путь` или id узла. Пространство: `knowledge`, `rules`, `human`,
`solution` или GUID решения. Без префикса берётся решение из `.zeroaimemory/solution.toml`,
а если файла нет — `knowledge`.

| Команда | Что делает |
|---|---|
| `zam status` | проверка сервера, токена (печатается префикс `zam_XXXXXXXX…`, как в списке токенов WebUI), пользователя и решения: M6.6 — строка `Решение:  «<имя>» (<GUID>) — <путь solution.toml>`, в `--json` — `solution_id`, `solution_name` (имя реестра), `solution_file_name` (имя в файле); M7.1 — источник токена с адресом `~/.zeroaimemory/config.toml [servers."<адрес>"]` (`token_source` в `--json`), у адреса без токена при токенах других серверов — код 1 до обращения к сети с подсказкой `zam init --url <адрес> --token <токен>`, ключ без адреса — примечание с командой привязки |
| `zam tree [цель] [--depth 1..5]` | дерево с краткими сводками L0 |
| `zam read <цель>… [--tier L0\|L1\|L2\|focus] [--section "A > B"] [--rev N]` | чтение Markdown как он сохранён; `--batch`, `--max-tokens N`, `--budget N`, `--if-rev N`, `--since-rev N` (дифф с ревизии, M4.5), `--locator` (часть вложения, M5) — пачкой через `POST /read` с бюджетом токенов; `--focus "<запрос>"` (M6) — оглавление и ближайшие к запросу части, пропуски с адресом `range:r<rev>:S-E`; `--range r<rev>:S-E` (M6) — ровно пропущенный текст той ревизии |
| `zam write <цель> [--file f.md] [-m сообщение] [--base-rev N] [--l0 текст]` | создать или заменить заметку (без `--file` текст читается из stdin) |
| `zam history <цель> [--diff A..B \| --revert N]` | ревизии, дифф, откат |
| `zam search <запрос> [--all] [--mode brief\|chunks\|notes] [--top-k N] [--rerank] [--budget N] [--kind K]… [--insight-kind K]… [--certainty C]… [--status S]… [--decided-by human\|agent] [--file P] [--history]` | поиск, по умолчанию Markdown-карточки; `--rerank` — серверный реранк. M6.6: бюджет ответа и фильтры `filters` флагами; бюджет или отсечка слабых срезали карточки — строка в stderr «показано K из L: бюджет — ещё N (--budget), слабые — M» |
| `zam release add <версия> [--date D] [--tag T] [--from-git [FROM..TO]] \| compare <версия> [--with prev\|<версия>] \| list [--limit N \| --all]` | релизы решения (M3); M6.6: `list` — новые первыми, строками `<id> <версия> <дата>[ тег <tag>][ · коммитов K]` |
| `zam attach <цель> <файл>` | загрузить вложение и напечатать ссылку `_attachments/…` |
| `zam report <файл.md> [--title T] [--tag X]… [--area A] [--ref commit:<sha>\|file:<путь>\|note:<id>\|task:k:<id>]… [--key K] [--json]` | отчёт пользователю (M5.5): Markdown с картинками и файлами по относительным ссылкам — одним запросом; печатает ссылку на страницу WebUI |
| `zam code sync [--full] [--prune] \| status \| map [--focus <путь\|имя>]… [--task T] [--tokens N] \| sym <имя\|путь#qual> \| refs <имя> \| impact [--since C]` | карта кода решения (M5.6): синхронизация рабочей копии, карта в бюджете, карточка символа, кто ссылается, влияние правки. M6.6: секреты сигнатур клиент маскирует до отправки правилами сервера — итог строкой stderr «секреты в сигнатурах: N (файлов F) — …», список мест — `masked` в `--json` |
| `zam log insight --kind K --title T [--scope global] [--certainty C] [--evidence-file F \| --evidence-checked C --evidence-method M --evidence-result R] [--context C] [--option O]… [--chosen C] [--rationale R] [--consequences C] [--decided-by human\|agent] [--revisit-if R]` | insight (открытие, ловушка, решение); M6.6: поля решения `data.decision` флагами — только у `--kind decision` (у `zam insight supersede` без `--kind` — вид замещаемой записи), до 8 `--option`; доказательство без файла — три флага `--evidence-*` вместе; без `--certainty` — строка в stderr об умолчании агента `hypothesis`; M7.1: замещение текстом с метками `‹secret-…›` — с ревизией старой записи: `--supersedes <id> --supersedes-rev <rev>`, у `zam insight supersede <id>` — `--rev <rev>` (`If-Match`; без неё — 428) |
| `zam insight triggers <id> --trigger ВИД:ШАБЛОН… \| --clear` | правка набора триггеров insight без замещения записи (M6.6): ревизия — из записи, правка с `If-Match` (запись изменилась — код 4); печатает `<id>@<rev> триггеров K` и новый набор |
| `zam task new --title T [--next S] [--owner agent\|human] [--estimate МИН] [--link L]… [--priority low\|normal\|high]` | задача (M4.5); M6.6: владелец, оценка в минутах, ссылки и приоритет — в `data` задачи, их печатает `zam task show` |
| `zam task show k:<id> \| list [--parent k:<id> \| --top]` | карточка и лента задач (M4.5); M7.2: у общей задачи — «подзадачи: 8/13 (в работе 2, открыто 2, …)» и шаги `k:<id> [статус] «…»` (до 50), у шага — «часть: k:<id> «…» (8/13)», у давней — «в работе без обновлений N ч»; в ленте — « · 8/13»; `--parent` — шаги общей в порядке плана (все статусы, если не задан `--status`), `--top` — без подзадач |
| `zam insight\|task\|question\|idea list [--global] [--limit N \| --all]` | ленты журналов; M6.6: есть ещё записи — строка в stderr «показано N, есть ещё: --limit 200 или --all», `--all` — вся лента. Область Knowledge — `--global` у лент, записей и Vault; `--knowledge` — синоним |
| `zam idea list [--status S]… [--origin human\|agent] [--global] \| new <заголовок> [--body T\|--file F] [--priority low\|normal\|high] [--horizon YYYY-MM-DD] [--tag X]… [--origin human\|agent] [--global] \| propose <w:id> [--text T] [--space <guid>] \| accept <w:id> [--by human\|agent] [--channel chat\|cli] [--space <guid>] \| defer\|decline <w:id> [--by human\|agent] [--channel chat\|cli] \| queue [--limit N] \| review <w:id> --plan\|--return\|--decline\|--done --summary-file F [--missing T] [--basis ID]… [--similar w:ID]… [--task k:ID \| --task-title T --task-body-file F --task-priority P] [--key K]` | идеи (M5.7): лента, запись, предложение агентом, решение человека (`defer --until`, `decline --reason`); под агентом `--origin` и `--by` обязательны. M6.5: `queue` — пакет разбора, `review` — исход разбора агентом (в план, на доработку, отклонить, уже сделано) |
| `zam vault list [--q Q] \| get <имя> [--raw] \| set <имя> --type T [--purpose P] [--address A] [--limits L] [--secret-stdin] \| rm <имя> \| rename <имя> <новое> \| mv <имя> --to global\|solution\|<GUID>` | Vault: записи без значений, значение в stdout (раскрытие — в аудит), запись (значение — только из stdin или скрытого ввода; у `host` и `environment` необязательно), мягкое удаление, переименование, перенос между личным Vault и Vault решения (M6.6, новая ревизия). Область — `--global` (личный, для всех проектов) или `--solution G`, без них — решение из `solution.toml`. `add` — синоним `set`, `delete` — синоним `rm` |
| `zam cache put --query Q --sources ID[#секция][@rev],… [--promote note:<путь>\|insight:<вид>] < выжимка.md` | сохранить выжимку с источниками (печатает `c_<id>`); `zam cache list\|show\|rm` — список, карточка с изменившимися источниками, удаление |
| `zam export [--space S] [--path P]… [--kind K]… [--no-history] -o <файл.zip>` | экспорт пространства (M6): архив zip — заметки `.md` с frontmatter, вложения, записи журналов, связи, история; без Vault, карты кода, корзины и сессий; токен — без Human и отчётов. Загрузка идёт в `<файл>.<pid>.part` (после убитого процесса его можно удалить); сервер молчит дольше `ZAM_EXPORT_IDLE_SECS` (300 с) — код 2. В WebUI — пункт «Экспорт (.zip)» в меню пространства |
| `zam import <файл.zip\|каталог> [--space S] [--own] [--on-conflict skip\|revision] [--axioms demote\|keep] [--dry-run]` | импорт пространства (M6): архив `zam export` или каталог `.md` (без `manifest.json` — все `*.md` заметки, архив чужой). Свой архив (тот же пользователь или `--own` при переезде) — id и аксиомы как есть; чужой — новые id, ссылки переписаны, аксиомы → `theorem` (`--axioms keep` — как есть). Повтор того же архива пропускается, изменившееся — в отчёт конфликтов (`--on-conflict revision` — новая ревизия). Только владелец пространства или администратор; отчёты — только из сессии WebUI; предел — `import.max_bytes` (2 ГиБ). Сервер принимает архив и выполняет импорт заданием; `zam` опрашивает его и печатает этап, Ctrl+C прекращает только ожидание; каталог упаковывается на лету, скрытые пути не переносятся |
| `zam sessions tokens [<id> \| --last N] [--by-holdout] [--subagents] [--json]` | выданное сессиям агента (M6): таблица «событие · вызовов · токенов · уже выдавалось» (Rules — строкой `rules`) и доля повторных чтений; без id — сессия агента или последняя; `--by-holdout` — отдельно контрольная группа A/B; `--subagents` (M7.2) — вместе с субагентами сессии; была выдача без сессии за 30 сут. — строка «без сессии за 30 сут.: вызовов C, токенов T»; та же сводка — на странице «Сессии» WebUI |
| `zam sessions tokens --unattributed [--days N] [--json]` | выдача без сессии (M7.2): MCP по HTTP без привязки к сессии агента — «сутки · событие · вызовов · токенов» и итог за N суток UTC (1..=366, 30); в WebUI — блок «Без сессии» страницы «Сессии» |
| `zam session snapshot --transcript <файл> [--format claude\|codex] [--session <id>] [--json]` | снимок сессии из журнала агента вручную (M6): последние запросы, изменённые файлы, команды и хвост конспекта — после маскировки секретов; сессия — `--session`, `ZAM_SESSION_ID` или id из журнала (нет ни одного — код 1); сессии нет на сервере — регистрируется. Хуки снимают сами (`checkpoint`, `session-start`; выключатель `[hooks] session_snapshot`) |

```
echo "# Bash-скрипты" | zam write "knowledge:Инструменты/Bash" -m "первая версия"
zam search "перевод строки в sh" --mode chunks
zam read "knowledge:Инструменты/Bash" --section "Установка > Linux"
```

Если векторный поиск выключен (нет моделей, Qdrant недоступен), `zam search` отвечает
результатами полнотекстового поиска: в Markdown остаётся строка сервера `> degraded: <код> — <причина>`,
а в stderr печатается причина по-русски. Предупреждения ответа (например, «реранкер не
установлен» при `--rerank`) тоже идут в stderr, в Markdown — строками `> warning:`. Бюджет срезал
карточки — в Markdown строка хвоста сервера `> не поместилось по бюджету: N (budget_tokens B)`,
в stderr — «показано K из L: бюджет — ещё N (--budget), слабые — M» (сервер 0.0.13-m6.6 и новее).

Коды выхода: 0 — успех, 1 — конфигурация, 2 — сервер недоступен, 3 — токен, 4 — конфликт
ревизий (дифф в stderr), 5 — не найдено, 6 — неверный ввод. Команды журналов, Vault, связей и
доступа (`zam log`, `insight`, `release`, `vault`, `link`, `links`, `solution access`) есть с M3,
команды агента (`zam init`, `guide`, `mcp`, `hook …`, `triggers sync`, `cache`,
`solution rename`) — с M4, рабочей памяти (`zam log term`, `glossary`, `task`, `question`,
`refs check|verify`, `links file:`, `vault set --type host`, `hook subagent-start`) — с M4.5, `zam report` — с
M5.5, `zam code` — с M5.6, `zam idea` — с M5.7, `zam export`, `import`, `sessions tokens|snapshot`
(`zam session …` — то же) и `zam read --focus|--range` — с M6, `zam idea review|queue` — с M6.5,
`zam vault rm|rename|mv` — с M6.6 (`mv` — сервер `0.0.13-m6.6` и новее), `zam insight triggers`
и `zam init --url-to` — с M6.6 (сервер любой), флаги решения и
доказательства `zam log insight`, поля `zam task new`, `--all` лент, флаги бюджета и фильтров
`zam search` и `zam release list` — с M6.6, токен по адресу сервера и хук `post-tool` с
ожидаемыми кодами и строкой «не повторяй» — с M7.1 (сервер любой), `zam task list --parent|--top`,
строки подзадач `zam task show`, `zam sessions tokens --unattributed|--subagents` — с M7.2
(`--parent`, `--top`, `--subagents` и строки подзадач — сервер `0.0.15-m7.2` и новее,
`--unattributed` — `0.0.11-m6`), `zam work`, `zam check`, `zam rules` и «открыто» в `zam insight
show` — с M7.3 (сервер `0.0.16-m7.3` и новее), `zam check probes list|add|retire` — с M7.4 (`add`
и `retire` — сервер `0.0.17-m7.4` и новее, `list` — `0.0.16-m7.3`). Нужный сервер:
`--evidence-checked|method|result` (`data.evidence`) и `--all`/`--limit` (курсор `next_cursor`,
страница до 200) — `0.0.4-m3` и новее (лента задач и вопросов — с `0.0.6-m4.5`, идей — с
`0.0.10-m5.7`); поля решения (`data.decision`) и `--owner`, `--estimate`, `--link` задачи —
`0.0.6-m4.5`; `--priority` задачи — `0.0.12-m6.5`; фильтры `zam search` (`--decided-by`,
`--file`, `--insight-kind`) — `0.0.6-m4.5` (старый сервер неизвестные фильтры пропускает молча),
строка «показано K из L» — `0.0.13-m6.6`; `zam release list` — любой; см.
[deploy/README-install.md](../deploy/README-install.md). Хуки
(`zam hook …`) всегда выходят с кодом 0.

## REST API

Все эндпоинты лежат под `/api/v1` и принимают `Authorization: Bearer <токен>`. Ошибки
приходят в формате RFC 9457. Полный список — в
[ТЗ §12](specs/2026-10-17-zeroaimemory-tz.md#12-rest-api-apiv1). Уже реализованы
группы Auth, Админ (пользователи, токены, аудит, reindex), Пространства и решения, Дерево,
Узлы, Ревизии, Рендер, Вложения, `POST /search`, `GET /index/status`, а с M3 — решения и
доступ, журналы, insights и триггеры, vault и граф, а с M4 — сессии, `POST /read`, кэш выжимок,
`/recall`, `/agent-setup`, `/downloads`, вход пользователей и `/mcp`, с M4.5 — словарь,
задачи (`claim`/`release`), ответы на вопросы, ссылки на файлы (`/files/changed`,
`/files/verified`, `/file-refs`), `/recall` `recovery` и `brief`, с M5–M5.7 — вложения-документы,
отчёты, карта кода `/code/*` и идеи, а с M6 — экспорт и импорт пространства
(`GET /spaces/{space}/export`, `POST /spaces/{space}/import`), снимок сессии
(`PUT|GET /sessions/{id}/snapshot`), выданное без сессии (`GET /sessions/unattributed`), эпоха и
контрольная группа сессии (`POST /sessions {new_epoch, holdout}`), поле `timeout_ms` у `/recall`,
`Idempotency-Key` у `POST /nodes`, `PUT` и `PATCH /nodes/{id}`. Что умеет конкретный сервер —
список `features` в `GET /api/v1/health`; что сервер обещает не ломать и как клиенту узнавать
возможности — [docs/api-compat.md](api-compat.md).

```
curl -s -X POST http://<хост>:7700/api/v1/search \
  -H "Authorization: Bearer <токен>" -H "Content-Type: application/json" \
  -d '{"query": "bad interpreter", "mode": "brief", "format": "md", "budget_tokens": 800}'
```

## Администрирование `zam-server`

| Команда | Что делает |
|---|---|
| `serve [--config] [--bind] [--data-dir]` | запуск сервера |
| `hash-password` | argon2id-хэш пароля администратора |
| `migrate` | открыть все базы с миграциями и выйти |
| `models fetch\|status` | модели по `deploy/models.toml`, с проверкой SHA-256 (M2) |
| `cuda fetch` | библиотеки NVIDIA для GPU-комплекта (M2) |
| `qdrant fetch\|status\|reset` | бинарник, состояние и сброс хранилища управляемого Qdrant (M2) |
| `reindex [--user] [--space] [--full]` | пересборка векторного индекса на остановленном сервере (M2) |
| `service install\|uninstall\|start\|stop\|status\|run` | режим службы: Windows SCM или systemd (M4) |
| `backup [--dir D \| --zip F] [--json]` | копия сервера без остановки (M6): базы, блобы и опись `manifest.json` с SHA-256 в `<data_dir>/backups/<yyyy-MM-dd_HHmmss>/` (или `[backup] dir`, `--dir`) либо одним zip; мастер-ключ Vault в копию не входит — сохраните его отдельно |
| `restore <каталог\|архив.zip> [--replace] [--no-reindex]` | восстановление копии на остановленном сервере в пустой каталог данных (M6): суммы и версии схем проверяются до записи; `--replace` переносит прежние данные в `backups/pre-restore-…`; индекс Qdrant пересобирается из кэша эмбеддингов |

## MCP

С M4: `/mcp` по streamable HTTP без сессий, `Authorization: Bearer`, 11 инструментов
(`memory_search`, `memory_read`, `memory_cache_put`, `memory_tree`, `memory_write`, `memory_log`,
`memory_update_entry`, `memory_link`, `memory_recall`, `memory_vault`; с M5.6 — `memory_code`,
карта кода: `map`, `symbol`, `refs`, `impact`; ТЗ §13.1). Ответы —
компактный Markdown с последней строкой `~N tok`. Для агентов без HTTP-транспорта MCP — `zam mcp`
(stdio). Фрагмент `.mcp.json` пишет `zam init` или даёт «Подключение агента»; с M6 это stdio
`zam mcp`: он находит сессию агента по привязке хука `session-start`, поэтому работают пометки
«уже выдавалось» и учёт токенов по сессии. Прежний HTTP-фрагмент работает без сессии. Свой
клиент передаёт сессию заголовком `X-Zam-Session` или аргументом `session_id` у `memory_search`,
`memory_read` и `memory_recall` (оба заданы и различаются — ошибка 400), срок сбора подсказок —
`timeout_ms` у `memory_recall`, повтор записи без дубля — `idempotency_key` у `memory_write` и
`memory_log` ([docs/api-compat.md](api-compat.md)).
