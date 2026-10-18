# Исследование 01. Аналоги: системы памяти для AI-агентов

Дата: 2026-10-17. Метод: README, исходники, заголовки issues (отсортированы по числу комментариев). Тела issues не читались.

## OpenViking (volcengine) — https://github.com/volcengine/OpenViking
- ~40k★, AGPL-3.0. «Context database» в виде виртуальной ФС `viking://` (resources/, user/{id}/memories|resources|skills).
- **Тиры контекста L0/L1/L2**: L0 — абстракт в 1 предложение, L1 — обзор, L2 — полный источник. Рядом с файлами `.abstract.md`, `.overview.md`; сводки сворачиваются вверх по каталогам.
- Запись: автоматически, при «session commit» LLM извлекает память (асинхронно, с задержкой появления).
- Поиск: сначала выбор поддерева каталогов, затем вектор внутри (TrieHI); `find` (прямой) и `search` (с планированием по контексту сессии).
- Интеграция: CLI `ov` (ls/tree/read/grep/find), HTTP, SDK, MCP, плагины для Claude Code/Codex/Cursor, хуки.
- Заявлено (свой бенчмарк): LoCoMo 57%→80% для Claude Code, −34…91% входных токенов.
- Проблемы (issues): #1258 невоспроизводимая точность (7.66%); #1549 L0/L1 недостижимы вектором; #3508 ломается на неожиданном формате ответа LLM; #4612 L0 каталогов не генерируются (lock, нет rollup); #4043 LLM дробит пользователя; #1119/#368 пустые результаты при фильтрах; #5038 медленно.
- Вывод: идея тиров + иерархии сильная; автоматический LLM-пайплайн записи — хрупкий и медленный.

## Graphiti (getzep) — https://github.com/getzep/graphiti, https://arxiv.org/abs/2501.13956
- Временной граф знаний: episodes → entity nodes → edges (факты) с битемпоральностью (`valid_at/invalid_at`, `created_at/expired_at`), communities.
- Противоречия: LLM сравнивает новое ребро с похожими, старое **инвалидируется, а не удаляется**.
- Поиск: косинус + BM25 + BFS по графу, RRF/MMR/cross-encoder; без LLM в момент запроса (<1 с).
- Интеграция: MCP (add_episode, search_*, group_id), FastAPI.
- Проблемы: малые модели не держат JSON-схему; много вызовов LLM на эпизод; зависания MCP на 100% CPU; тяжёлая графовая СУБД (Neo4j/FalkorDB).

## personal-os (amanaiproduct) — https://github.com/amanaiproduct/personal-os
- Не сервис: каркас из markdown (BACKLOG.md, Tasks/*.md с frontmatter, Knowledge/, GOALS.md) + сценарии в AGENTS.md/CLAUDE.md.
- Опциональный MCP с дедупликацией (порог 0.6, merge/link/cancel), `auto_create:false` — подтверждение человеком.
- Удачно: жёсткие лимиты (P0≤3), дедуп перед записью, сценарии прямо в инструкции, GC закрытых задач через 30 дней.
- Минусы: нет поиска/эмбеддингов, одна машина.

## Прочие аналоги
- **basic-memory** (https://github.com/basicmachines-co/basic-memory) — ближе всего: markdown-файлы = истина, SQLite = индекс, observations `- [category] факт #tag`, wiki-relations, гибрид FTS + вектор (FastEmbed), ~20 MCP-инструментов (много), `doctor` для проверки согласованности.
- **mem0/OpenMemory** — LLM решает ADD/UPDATE/DELETE/NOOP; критика: тихо удаляет валидные воспоминания, путает отзыв с обновлением (https://dev.to/mukesh_13/mem0-auto-resolves-memory-conflicts-for-you-until-it-silently-deletes-one-you-still-need-4f4m, https://tianpan.co/blog/2026-05-17-the-agent-that-remembers-what-you-took-back).
- **Letta** — core-память в контексте + archival-вектор + sleep-time консолидация. Эксперимент: агент с одними файловыми инструментами (grep/open) — 74% на LoCoMo против 68.5% у mem0 (https://www.letta.com/blog/benchmarking-ai-agent-memory). Контраргумент: на больших объёмах/multi-hop файлов мало (https://buttondown.com/nicoloboschi/archive/file-based-agent-memory-great-demo-good-luck-in/).
- **Claude Code native**: CLAUDE.md (иерархия), auto memory — индекс `MEMORY.md` (~200 строк) + тематические файлы по требованию; нет синхронизации между машинами. Anthropic memory tool (`memory_20250818`) — файловые операции над `/memories`. Вывод: вендор выбрал файлы + оглавление.
- **Cognee** — граф + вектор, та же проблема дорогой LLM-записи.

## MCP vs REST/CLI
- MCP раздувает контекст определениями инструментов (сторонние замеры 44k vs 1.4k токенов у CLI: https://www.apideck.com/blog/mcp-server-eating-context-window-cli-alternative.md), частично снято deferred tools в Claude Code (https://www.checklyhq.com/blog/mcp-vs-cli-token-efficiency/).
- Консенсус 2026: ядро REST, тонкий MCP (5–8 инструментов) + CLI/skill как адаптеры.

## Устаревание и конфликты
| Подход | Где | Проблема |
|---|---|---|
| Битемпоральная инвалидация | Graphiti | LLM-решение, дорого |
| LLM UPDATE/DELETE | mem0 v1–v2 | Тихая потеря |
| Append-only + ранжирование | mem0 v3 | Устаревшее всплывает |
| Человек правит markdown | basic-memory, Claude Code | Прозрачно, но вручную |
| Фоновая консолидация | Letta, OpenViking | Недетерминированность |

Кэш ответов с инвалидацией по изменению источников ни у кого не найден — ниша ZeroAIMemory.

## Выводы для ZeroAIMemory
1. Тиры L0/L1/L2 на каждую заметку и узел дерева; индексировать все уровни.
2. Поиск по поддереву (фильтр по пути/группе), тесты на пустые фильтры.
3. Гибридный поиск без LLM в момент запроса; LLM только для выжимки.
4. Битемпоральность для insights: старое не удалять, помечать; отзыв ≠ обновление.
5. Запись явная (агент вызывает инструмент); авто-извлечение только как предложения с подтверждением; дедуп перед записью.
6. Кэш с точной инвалидацией по версиям источников, проверка при чтении.
7. Ядро REST + тонкий MCP + CLI + хуки Claude Code.
8. Human и vault физически вне индекса.
9. GC/архивирование, лимит размера дайджестов.
10. `doctor`: согласованность SQLite ↔ blobs ↔ векторного индекса.
11. Явный статус индексации (`indexed/pending`), иначе «только что записанное не находится».
12. Файловая метафора (пути, tree/read/grep) — агентам она привычна.
