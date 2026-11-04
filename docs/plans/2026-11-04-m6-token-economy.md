# M6. Экономия токенов — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

Редакция 2 (2026-11-04) — по рецензии плана (CHANGES REQUESTED: C1–C3, I1–I10, Minor); спецификация
— редакция 4.3. Пункты «ждёт ответа владельца» реализуются по варианту по умолчанию из
спецификации, пока ответа нет.

**Goal.** Сократить токены, которые память выдаёт агенту, не теряя фактов ответа, и подтвердить это
воспроизводимым замером ТС-1 и ТС-2. Попутно в этапе:
- закрываются отложенные вопросы M3–M5.7;
- исправляются ошибки `session_seen` после compact и MCP без своей сессии;
- делается снимок сессии из журнала агента (решение владельца baton-1 = А).

**Architecture.**
- **Сначала мерило, потом выдача.** Каркас замера, пилот на тегах, пополнение корпуса и заморозка
  двух отпечатков (`nuggets_fingerprint`, `corpus_fingerprint`) идут **до** любой правки выдачи
  (задачи 1a1–1b). Правки выдачи (5–8c) подбираются на dev, а приёмка — на holdout в задаче 13b.
- **Одна миграция `0025`** (задача 2) — вся схема этапа: эпохи `session_seen`,
  `sessions.seen_epoch`/`holdout`, `tokens_unattributed`, `session_snapshots`,
  `code_refs.recv_count`.
- **Сессия — явная или никакая** (задача 3). `RequestSession` получает поля `explicit` и
  `via_mcp`. Признак MCP ставит внутренний клиент `Rest` расширением запроса — снаружи его не
  подделать.
- **Ядро агентонезависимо.** Нейтральные типы снимка лежат в `zam-core`. Адаптеры журналов Claude и
  Codex — в клиенте `zam`. Сервер хранит снимок и показывает его.
- **Лёгкие общие крейты.** Сканер секретов выносится в новый крейт `zam-secrets` (только `regex`),
  стоп-слова — в `zam-core`. Тогда клиент маскирует снимок без comrak/syntect/ammonia, а
  `zam-search` использует стоп-слова без зависимости от `zam-markdown`.
- **Принцип владельца «качество важнее токенов».** Умолчания выбираются правилами §3.5,
  записанными до прогона. Если цель 30 % не достигнута, это записывается фактом.

**Tech Stack.** Rust 1.99.1, workspace без новых внешних библиотек. Новые **связи** между уже
используемыми крейтами (аудит владельца: только из workspace):
- новый крейт `crates/zam-secrets` (MIT, свой) с зависимостью `regex`;
- `zam-cli` получает `zam-secrets` и `windows` (cfg(windows), фича
  `Win32_System_Diagnostics_ToolHelp`; крейт уже есть у xtask);
- `xtask` получает `blake3` (уже у `zam-cli`);
- `zam-markdown` реэкспортирует `zam-secrets` и стоп-слова `zam-core`.

`jiff` в `zam-core` и `zam-cli` не добавляется: дата для текста агенту форматируется из строки RFC
3339 (задача 6). xtask остаётся синхронным (`ureq`), без `tokio` и `semver`. WebUI — Svelte 5, без
новых пакетов.

**Spec:** `docs/specs/2026-11-04-m6-token-economy.md`, редакция 4.3 (далее — «спецификация»; `§N` —
её разделы). Исследование — `docs/research/2026-11-04-m6-token-economy.md`, baton —
`docs/research/2026-11-04-baton-review.md`, события хуков — `docs/research/2026-10-28-agent-hook-events.md`.
Основа — ТЗ `docs/specs/2026-10-17-zeroaimemory-tz.md` v1.23. Журнал —
`docs/plans/2026-11-04-m6-token-economy-log.md`.

## Global Constraints

- **Окружение** (PowerShell): `. scripts/dev-env.ps1; $env:CARGO_INCREMENTAL=0`. Сборка и тесты —
  `-j 2 --target-dir target/r1-new`. Ниже `$T` — `-j 2 --target-dir target/r1-new`.
  - **Без `cargo clean` и `npm ci`.** Если на H: меньше 100 ГБ — удалить `target/*/incremental`,
    `target/*/tmp` и `.exe`/`.pdb` старше 6 ч из `target/*/debug/deps`.
  - **Полный прогон** `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast $T` (около 2 ч) —
    только в задачах 8c, 17d и 13b.
  - **e2e** (`npm run e2e`) — только на release-сборке сервера (`E2E_SERVER`).
- **Временное** — только `.tmp/m6/<задача>/` или `target/`. В тестах —
  `env!("CARGO_TARGET_TMPDIR")`. Запрещены: `/tmp`, `%TEMP%`, `AppData\Local\Temp`, scratchpad,
  `std::env::temp_dir()`, `tempfile` без `dir=`. Субагентам правило передаётся в брифе.
- **Вне каталога проекта не читать**, в том числе `~/.claude` и `~/.codex`, пока владелец не
  разрешит (задача 17a, шаг 0).
- **Тесты с процессами** не наследуют `ZAM_*` и `AGENT_ENV_VARS` окружения. Явная установка нужной
  переменной в самом тесте (например `ZAM_SESSION_ID`) допустима. CLI — через
  `crates/zam-cli/tests/common::zam_cmd`, сторож `repo_clean` зелёный.
  - Время сервера — замороженными часами (задача 6, фича `test-hooks`) или прямым SQL. Время в
    клиентских тестах — правкой поля `at` в `snapst-*.json` (и подобных файлах кэша) фикстурой.
  - Гонки — барьерами и точками `test-hooks`, без `sleep`.
- **Ключи-приманки** в фикстурах и тестах собираются конкатенацией (`["AK", "IA", …].concat()`):
  литерал заблокирует push protection.
- **Задачи выполняются строго последовательно** (SDD), в порядке таблицы ниже. Параллельных ветвей
  нет.
- **Сборка зелёная после каждой задачи.** Задача, меняющая общий тип или протокол (`RefAgg`,
  `SessionTouch`, `RequestSession`, `ReadItem`, `Kind::accepts`, `Settings`), правит все места
  использования.
- **Проверки каждой задачи:**
  - `cargo fmt --all -- --check`;
  - `cargo clippy --workspace --all-targets $T -- -D warnings`;
  - перечисленные в задаче `cargo test -p … --test …`;
  - WebUI — `npm run check`, `npm run build`, `npm run size`, `npm run test:build`.
- **Единообразие.**
  - Тексты и комментарии — по-русски (doc ≤ 100 символов), идентификаторы — по-английски.
  - Ошибки: `thiserror` в библиотеках, `ApiError` (problem+json) на сервере, коды выхода CLI — ТЗ
    §13.2.
  - `tracing` — без текста записей, запросов и снимков.
  - Шаблоны: сервер — `api/sessions.rs`, `api/recall.rs`; CLI — `cmd/hook/*`; xtask —
    `xtask/src/eval/*`.
- **Значения спецификации** — таблица §14 (ключи, диапазоны, умолчания). Сверх неё:
  - дата в тексте агенту — `yyyy-MM-dd HH:mm:ss` UTC, без `T`/`Z`; в JSON — RFC 3339;
  - бюджеты хуков ТЗ §10.3: `checkpoint` — 100 мс, `session-start` — 300 мс;
  - блок v7 — ≤ 500 токенов bge-m3, ru и en, худший случай: GUID
    `d6c5a5d9-c4c7-7f6b-a8b6-d8fd4c9fb6f6`, адрес 78 символов, имя 128 → 40, Rules открыты;
  - статистика — парный бутстреп, 10 000 выборок, фиксированное зерно;
  - пороги: Δ микро-полноты — нижняя граница ≥ −0,02; `L ≤ 1`; Δ nDCG@10 и recall@5 — нижняя
    граница ≥ −0,01; `reliable_first` = 1,000.
- **Миграция** — пользовательская `0025_session_epochs.sql`, откат — `bak-v24`. Последняя на main —
  `0024_ideas.sql` (проверено 2026-11-04).
- **Версия** `0.0.11-m6`, CHANGELOG `[0.0.11-m6]`, ТЗ v1.24, `AGENT_BLOCK_VERSION = 7`.
- **Бинарники баз** (теги `v0.0.5-m4`, `v0.0.9-m5.6`, `v0.0.10-m5.7`):
  - исходники — `git archive <тег> | tar -x -C .tmp/m6/tags/<тег>`. Не `git worktree`: см. память
    о junction;
  - сборка — `cargo build --release -p zam-server -p zam-cli -j 2 --target-dir target/tags/<тег>`;
  - затем удаляются `deps`, `incremental` и `build`, остаются бинарники;
  - перед каждой сборкой — H: ≥ 100 ГБ. Тулчейн у тегов тот же (1.99.1, проверено);
  - сбой сборки тега — в журнал, и база этого тега исключается из плеч с причиной в отчёте.
- **Коммиты.**
  - После каждой задачи, в ветке `m6-token-economy`, Conventional Commits по-русски.
  - **Только явными путями:** `git add <пути>; git commit -F .tmp/m6/<задача>/msg.txt -- <пути>`,
    никаких `-a` и `.`.
  - Trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Без push, merge и tag до финального ревью.
- **Зависит от владельца, остальное не блокирует:**
  - шаг 0 задачи 17a — живые журналы агента;
  - прогон агентного A/B (задача 12) — вне приёмки;
  - чек-лист на живом агенте: `CLAUDE_CODE_SESSION_ID` в MCP, `transcript_path` у Codex,
    `SessionEnd` при закрытии окна, цепочка процессов Codex.

## Review Focus

1. **Снимок не мешает хуку.** Опасные входы — журнал 50 МиБ на HDD, строка 10 МиБ, файл, который
   пишется во время чтения, `transcript_path` на каталог или файл без прав.
   - **Ожидание:** бюджет соблюдён, `decision: block` не задерживается, снимок `partial` или его
     нет, в stdout ошибки нет.
   - **Тесты** (17b): `snapshot_deadline_never_delays_stop`, `snapshot_tail_of_growing_file`,
     `snapshot_bad_path_is_silent`.
2. **Эпоха не открывает старое и не прячет нужное.** Сценарии:
   - `PreCompact` без `SessionStart compact` и наоборот;
   - две эпохи подряд;
   - субагент на общем MCP.

   **Ожидание:** после новой эпохи `(seen)` не прячет выданное до неё, идеи остаются показанными,
   MCP не теряет фрагментов. **Тесты** (7a): `epoch_bump_without_session_start`,
   `double_epoch_bump`, `ideas_seen_across_epochs`.
3. **Честный бюджет на краях.** Бюджеты 1 и 100; строка карточки длиннее бюджета; многобайтовый
   заголовок.
   - **Ожидание:** первая карточка отдаётся всегда, фрагмент режется по границе символа, остальное
     ≤ бюджета.
   - **Тесты** (5): `brief_budget_edges_first_card_only`, `snippet_cut_on_char_boundary`.
4. **Старый клиент и старый сервер.** `zam` 0.0.10-m5.7 против сервера M6 и наоборот, в том числе
   `SessionEnd`/`PostCompact`, `new_epoch`/`holdout`, `recv_count`.
   - **Ожидание:** основная регистрация без 400, старый сервер не получает неизвестных полей,
     старый `zam` отвечает на новые события пусто.
   - **Тесты** (13b): `compat_old_zam_new_server`, `compat_new_zam_old_server`.
5. **Замер нельзя «подкрутить».** Нагеты или корпус изменены после заморозки; сетка запущена на
   holdout; плечи сняты на разных отпечатках.
   - **Ожидание:** xtask падает с понятной ошибкой.
   - **Тесты** (1a1): `eval_rejects_fingerprint_mismatch`, `grid_refuses_holdout`,
     `arms_must_share_corpus_fingerprint`.

---

## Карта файлов

```
crates/zam-secrets/{Cargo.toml,src/lib.rs} (новый; перенос crates/zam-markdown/src/secrets.rs)
crates/zam-core/src/{lib.rs,text.rs (стоп-слова, новый),time.rs (новый),snapshot.rs (новый)}
crates/zam-markdown/src/{lib.rs,boundary.rs}   (реэкспорт secrets и is_stop_word)
crates/zam-store/migrations/user/0025_session_epochs.sql
crates/zam-store/src/{sessions.rs,usage.rs,code.rs,ideas.rs,snapshots.rs (новый),time.rs,test_hooks.rs,user_db.rs,lib.rs}
crates/zam-search/src/recall.rs                (WordMatch, prepared_terms, idea_stem — общий)
crates/zam-code/src/{proto.rs,rank.rs}, crates/zam-code/src/extract/*
crates/zam-server/src/{config.rs,maintenance.rs}, deploy/config.example.toml
crates/zam-server/src/api/{sessions.rs,read.rs,read_focus.rs (новый),recall.rs,snapshot.rs (новый),
      index.rs,code.rs,cache.rs,previews.rs,reports.rs}, crates/zam-server/src/api/search/{mod.rs,cache_step.rs}
crates/zam-server/src/mcp/{mod.rs,tools.rs}, crates/zam-server/src/agent_setup/{mod.rs,templates.rs,guide.ru.md,guide.en.md}
crates/zam-cli/{Cargo.toml (фича test-hooks)}, crates/zam-cli/src/{main.rs,cmd/mod.rs}
crates/zam-cli/src/cmd/{mcp.rs,read.rs,report.rs,sessions.rs (новый),session.rs (новый)}
crates/zam-cli/src/cmd/code/{config.rs,sync.rs,state.rs}
crates/zam-cli/src/cmd/hook/{mod.rs,checkpoint.rs,session_start.rs,journal.rs,cache.rs,
      binding.rs (новый),procs.rs (новый),snapshot/{mod.rs,claude.rs,codex.rs} (новые)}
crates/zam-cli/tests/{hooks.rs,mcp_bind.rs,sessions.rs,snapshot.rs,m6_acceptance.rs (новые),m4_acceptance.rs,m55_acceptance.rs}
crates/zam-cli/tests/fixtures/transcripts/*.jsonl (новые)
crates/zam-server/tests/{sessions.rs,session_tokens.rs,read_focus.rs,snapshot.rs,m6_acceptance.rs (новые часть),
      recall.rs,recall_recovery.rs,ideas_recall.rs,search.rs,cache_api.rs,mcp.rs,agent_setup.rs,code_api.rs}
crates/zam-server/tests/fixtures/idea_hints_m57.json (новый эталон)
xtask/src/eval/{mcp_client.rs,caps.rs,split.rs,nuggets.rs,sessions.rs (новые),metrics.rs,tokens.rs,run.rs,report.rs,
      hints.rs,grid.rs}, xtask/src/{eval.rs,main.rs,ab.rs (новый),code_bench.rs,smoke.rs,package.rs}, xtask/Cargo.toml
xtask/tests/{eval_cli.rs,ab.rs} (новые)
eval/{scenarios.toml,sessions.toml,idea-hints.toml,kind-queries.toml,area-queries.toml,nuggets.lock (новые часть),
      hints.toml,README.md}, eval/corpus/**, eval/sessions-corpus/** (новый), eval/ab/{tasks.toml,runs.toml}
web/src/components/{ReportView,IdeasView,SessionsView (новый)}.svelte, web/src/routes/{Shell,IndexStatus}.svelte,
web/src/components/Tree.svelte, web/src/lib/{api.ts,router.ts,time.ts,print.ts,ideaButtons.ts (новый)}
web/e2e/{reports.spec.ts,reports-print.spec.ts,user-login.spec.ts,sessions.spec.ts (новый),ideas.spec.ts}, web/.size-limit.json
deploy/licenses/*, about.toml
```

## Порядок задач

| № | Задача | Зависит от | Объём |
|---|---|---|---|
| 1a1 | Каркас замера: MCP-клиент, возможности, статистика, разбиение | — | M |
| 1c | Пополнение корпуса до заморозки, корпус ТС-2, 50 идей для эталона | 1a1 | M |
| 1a2 | Пилот на трёх тегах, контроль v0.0.9 ≡ v0.0.10, эталон подсказок-идей M5.7 | 1a1, 1c | M |
| 2 | Схема `0025` | — | M |
| 1b | Набор ТС-1, нагеты, заморозка двух отпечатков | 1c | L |
| 3 | Привязка MCP к сессии, `explicit`/`via_mcp`, `unattributed` | 2 | M |
| 4 | Сессионный набор ТС-2 | 1a1, 1c, 3 | L |
| 5 | Карточки v2, честный бюджет, `POST /cache` | 1b | M |
| 6 | Дата UTC, замороженные часы | 5 | S–M |
| 7a | Эпохи и «уже выдавалось» | 2, 3, 6 | M |
| 7b | Явный фокус | 7a | M |
| 8a | Rules-резерв, индексы, детерминированность, идеи | 6, 7a | M |
| 8b | Учёт, `zam sessions`, страница сессий WebUI, `rules_heavy` | 8a | M |
| 8c | Сетки ТС-2 и полный прогон | 4, 8b | M |
| 9 | Пороги подсказок, общий код совпадения слов | 1a2, 5, 7a | M |
| 10 | Буст вида | 1b | M |
| 11 | `boost_area`, `w_attach` | 10 | M |
| 12 | Стенд A/B, `checkpoint_impact`, `holdout` | 2, 8b | M (+ прогон владельца) |
| 14 | Ранжирование карты кода | 2 | M |
| 15a | Долги M5.5: ссылки, миниатюры, «PDF / печать» | 6 | M |
| 15b | Долги M5.6: сканер в `zam-secrets`, счётчик повторов, флейк, e2e входа | — | M |
| 15c | Упаковка: `LICENSES/`, `clarify.files` | — | S |
| 16 | Долги M5.7 | 9 | M |
| 17a | Снимок: типы, адаптеры, `take`, маскировка (шаг 0 — владелец) | 15b | M |
| 17b | Снимок: хуки, очередь, `zam session snapshot` | 17a, 3 | M |
| 17c | Снимок: сервер, показ, WebUI | 17b, 8a | M |
| 17d | Снимок: бюджеты, ТС-2, полный прогон | 17c | S |
| 13a | Блок v7 и протокол | все выше | S |
| 13b | Переснятие на holdout, приёмка, совместимость | 13a | L |
| 13c | Документы и выпуск `0.0.11-m6` | 13b | M |

Строгий порядок исполнения: **1a1 → 1c → 1a2 → 2 → 1b → 3 → 4 → 5 → 6 → 7a → 7b → 8a → 8b → 8c → 9 →
10 → 11 → 12 → 14 → 15a → 15b → 15c → 16 → 17a → 17b → 17c → 17d → 13a → 13b → 13c.** 15b стоит
раньше 17a, потому что 17a маскирует снимок через `zam-secrets`. Таблица «Зависит от» описывает
связи, а не разрешение на параллельность.

---

### Task 1a1: Каркас замера и статистика (§3.1–§3.5)

**Files.**
- Create: `xtask/src/eval/{mcp_client.rs,caps.rs,split.rs,nuggets.rs}`, `xtask/tests/eval_cli.rs`.
- Modify: `xtask/src/eval/{metrics.rs,tokens.rs,run.rs,report.rs,grid.rs}`,
  `xtask/src/{eval.rs,main.rs}`, `xtask/Cargo.toml` (`blake3.workspace = true`), `eval/README.md`.

**Interfaces — Produces:**
```rust
// mcp_client.rs — синхронный (ureq), JSON-RPC поверх POST /mcp
pub struct McpClient { /* agent: ureq::Agent, base, token, solution, session, mcp_session_id */ }
impl McpClient {
    pub fn connect(base: &str, token: &str, solution: &str, session: Option<&str>) -> anyhow::Result<Self>;
    pub fn call(&self, tool: &str, args: serde_json::Value) -> anyhow::Result<String>; // текст result.content
}
// caps.rs — версия из GET /health, разбор "0.0.10-m5.7" вручную (без semver)
pub struct Version { pub major: u32, pub minor: u32, pub patch: u32, pub stage: String }
pub fn parse_version(s: &str) -> Option<Version>;
pub struct ServerCaps { pub version: Version, pub focus: bool, pub ideas: bool, pub snapshot: bool,
                        pub keys: BTreeSet<&'static str> }   // таблица «версия → возможности/ключи»
pub fn probe(base: &str) -> anyhow::Result<ServerCaps>;
pub fn filter_overrides(caps: &ServerCaps, env: &[(String, String)]) -> Vec<(String, String)>;
// metrics.rs — расширить, а не дублировать: число выборок параметром
pub const BOOTSTRAP_M6: usize = 10_000;
pub fn ratio_estimate_with(pairs: &[(f64, f64)], n: usize, seed: u64) -> Option<Estimate>;
pub fn paired_diff_weighted(a: &[f64], b: &[f64], w: &[f64], n: usize, seed: u64) -> Estimate;
pub fn micro_recall(found_total: &[(usize, usize)]) -> f64;
pub fn net_losses(base_full: &[bool], arm_full: &[bool]) -> i64;   // #(1→0) − #(0→1)
// split.rs — расслоение по виду сценария
pub const SPLIT_SALT: &str = "m6-2026-11-04";
pub fn split(items: &[(String, String)]) -> BTreeSet<String>;
//   внутри каждого kind: сортировка по blake3(SALT ‖ id), первые ⌈n/3⌉ — holdout (возврат — их id)
// nuggets.rs
pub fn nuggets_fingerprint(scenarios: &Path, corpus: &Path) -> String; // сценарии+нагеты+тексты источников
pub fn corpus_fingerprint(dirs: &[&Path]) -> String;                   // eval/corpus + eval/docs
```
CLI xtask:
- `cargo xtask eval --tokens --via mcp [--server-bin P] [--zam-bin P] --split dev|holdout [--grid
  <файл.toml>] [--out <префикс>]`;
- `--grid` с `--split holdout` — ошибка;
- `--check` — проверки §3.2.

Формат файла сетки: `[[point]] name = "…"; env = { ZAM_SEARCH__BUDGET_TOKENS = "600", … }`.

- [ ] **Step 1: Тесты** (FAIL):
  - `ratio_identity_is_one` — одинаковые плечи: est = 1,0, `lo ≤ 1 ≤ hi`;
  - `ratio_seed_reproducible`;
  - `net_losses_counts_both_ways` — `[1,1,0]` против `[0,1,1]` → 0;
  - `split_is_third_per_kind` — 3 вида по 30 id → по 10 в holdout в каждом;
  - `parse_version_stage`;
  - `filter_overrides_drops_unknown_keys`;
  - `grid_refuses_holdout`, `eval_rejects_fingerprint_mismatch`,
    `arms_must_share_corpus_fingerprint`.

  Run: `cargo test -p xtask $T` — FAIL (нет функций).
- [ ] **Step 2: Реализация.** Политика симулированного агента §3.4 в `tokens.rs`:
  - `READ_BUDGET` 4000;
  - дочитывание один раз по триггерам `…ещё N токенов`, `(budget)`, `[пропущено: <путь> · ~N t]`:
    сначала `section` из пометки, иначе L2 с бюджетом 4000;
  - справочная политика без оракула: карточки 1–3 по порядку;
  - ответы целиком — в JSON отчёта.

  Run: `cargo test -p xtask $T` — PASS.
- [ ] **Step 3: Commit** `feat(eval): каркас замера M6 — MCP, статистика, разбиение`.

### Task 1c: Пополнение корпуса до заморозки (§3.2, §10, §11, рецензия плана C1)

**Files.** Modify: `eval/corpus/**` (области у целей, трудные соседи и пары `contradicts`). Create:
`eval/sessions-corpus/**` (Rules ≥ 10, задачи `in_progress`/`blocked`, ответы человека, доступ,
словарь, идеи, в том числе 50 идей `eval/sessions-corpus/ideas/` для эталона подсказок-идей задачи
1a2). Modify: загрузчик xtask — `X-Zam-Area`,
`--corpus`.

**Interfaces — Produces:**
- `eval/corpus` — корпус ТС-1, ТС-1 грузит только его и `eval/docs`;
- `eval/sessions-corpus` — грузит только ТС-2 (`eval --sessions`) и наборы идей;
- у целей корпуса — поле `area` (`backend`, `frontend`, `infra`, ≥ 8 на каждую).

- [ ] **Step 1: Тест загрузчика** (FAIL) `ts1_never_loads_sessions_corpus`,
  `area_header_sent_from_query` (`cargo test -p xtask $T`).
- [ ] **Step 2: Пополнение.**
  - Для §10: ≥ 24 ответа `gotcha`/`failure` с соседями — заметка той же темы, `discovery`/
    `experiment` с близким текстом — и ≥ 8 пар `hypothesis gotcha` ↔ `theorem` со связью
    `contradicts`.
  - Для §11: области.
  - `cargo xtask eval` (основной набор) на новом корпусе — числа **только в журнал**, без критерия
    прохождения: базы сравниваются уже на этом корпусе.
- [ ] **Step 3: Commit** `test(eval): пополнение корпуса M6 и корпус ТС-2`.

### Task 1a2: Пилот на тегах, контроль, эталон подсказок-идей M5.7 (§3.3, рецензия плана C3)

**Files.**
- Create: `eval/idea-hints.toml` (≥ 40 запросов, положительные и отрицательные, к фикстуре из 50
  идей в `eval/sessions-corpus/ideas/`), `crates/zam-server/tests/fixtures/idea_hints_m57.json`.
- Modify: `xtask/src/eval/{hints.rs,run.rs}` (режим `--idea-hints`).

**Interfaces — Produces:**
- `cargo xtask eval --idea-hints eval/idea-hints.toml --server-bin <P> --out <файл.json>` →
  `[{query, ideas: [w:id…]}]` — топ-5 подсказок-идей к `prompt`, по порядку.
- Эталон снимается командой:
  `cargo xtask eval --idea-hints eval/idea-hints.toml --server-bin
  target/tags/v0.0.10-m5.7/release/zam-server.exe --out
  crates/zam-server/tests/fixtures/idea_hints_m57.json`.

- [ ] **Step 1: Бинарники тегов** — по Global Constraints («Бинарники баз»). Лог —
  `.tmp/m6/task1a2/build-<тег>.log`.
- [ ] **Step 2: Пилот** — B4, B5.6, B5.7 на текущих 20 сценариях:
  `cargo xtask eval --tokens --via mcp --server-bin <…> --zam-bin <…> --split dev --out
  .tmp/m6/task1a2/pilot-<тег>`.
- [ ] **Step 3: Контроль «v0.0.9 ≡ v0.0.10-m5.7»** — побайтовое сравнение ответов `queries.toml`,
  `docs-queries.toml` и ТС-1 у B5.6 и B5.7. Расхождение — в журнал, и тогда база качества —
  v0.0.10-m5.7 (шапка спецификации).
- [ ] **Step 4: Эталон** `idea_hints_m57.json` — командой выше; число строк и сумма проверяются
  тестом `idea_hints_fixture_is_wellformed` (`cargo test -p zam-server --test ideas_recall $T
  idea_hints_fixture` — PASS).
- [ ] **Step 5: Commit** `test(eval): пилот баз M6 и эталон подсказок-идей M5.7`.

### Task 2: Схема `0025` (§13)

**Files.**
- Create: `crates/zam-store/migrations/user/0025_session_epochs.sql`, `crates/zam-store/src/snapshots.rs`,
  `crates/zam-core/src/snapshot.rs`.
- Modify: `crates/zam-store/src/{user_db.rs,sessions.rs,usage.rs,code.rs,lib.rs}`, `crates/zam-core/src/lib.rs`.
- Test: `crates/zam-store/tests/{migrate_rebuild.rs,sessions.rs,snapshots.rs (новый)}`.

**Interfaces — Produces:**
```sql
-- 0025 (rebuild: true)
CREATE TABLE session_seen (session_id TEXT NOT NULL REFERENCES sessions(id), owner_id TEXT NOT NULL,
  target_kind TEXT NOT NULL CHECK (target_kind IN ('node','entry','blob')), target_id TEXT NOT NULL,
  part TEXT NOT NULL DEFAULT '', rev INTEGER NOT NULL, seen_at TEXT NOT NULL,
  epoch INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, owner_id, target_kind, target_id, part, epoch));
ALTER TABLE sessions ADD seen_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD holdout INTEGER NOT NULL DEFAULT 0;
CREATE TABLE tokens_unattributed (day TEXT NOT NULL, event TEXT NOT NULL, calls INTEGER NOT NULL,
  tokens INTEGER NOT NULL, PRIMARY KEY (day, event)) WITHOUT ROWID;
CREATE TABLE session_snapshots (session_id TEXT PRIMARY KEY REFERENCES sessions(id),
  taken_at TEXT NOT NULL, trigger TEXT NOT NULL, agent TEXT,
  status TEXT NOT NULL CHECK (status IN ('complete','partial')), skipped INTEGER NOT NULL,
  bytes INTEGER NOT NULL, data TEXT NOT NULL);
ALTER TABLE code_refs ADD recv_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_tokens ADD reread_calls INTEGER NOT NULL DEFAULT 0;  -- повторная выдача (7a, 8b)
```
```rust
// zam-core/src/snapshot.rs — единственное место нейтральных типов снимка (serde)
pub enum SnapStatus { Complete, Partial }                        // "complete" | "partial"
pub enum SnapTrigger { PostCompact, PreCompact, SessionStartCompact, Stop, SessionEnd, CatchUp, Manual }
pub struct SnapCommand { pub cmd: String, pub failed: Option<bool> }
pub struct SnapshotData { pub compact_summary: Option<String>, pub prompts: Vec<String>,
                          pub files: Vec<String>, pub commands: Vec<SnapCommand> }
// zam-store/src/sessions.rs
pub struct SessionTouch { …, pub epoch: u32 }   // эпоха — в касании; заполняет request_session
pub async fn bump_epoch(db: &Db, session: &str) -> Result<u32, StoreError>;
pub async fn session_epoch(db: &Db, session: &str) -> Result<u32, StoreError>;
pub async fn set_holdout(db: &Db, session: &str, on: bool) -> Result<(), StoreError>;
// mark_seen(&SessionTouch, …) пишет epoch = touch.epoch;
// seen_revs(db, session, epoch, targets) — только текущая эпоха;
// seen_entry_ids, any_idea — БЕЗ фильтра по эпохе (идеи, §7.2)
// gc_sessions: явный список ["session_seen", "session_tokens", "session_snapshots"]
// usage.rs
pub async fn add_unattributed(db: &Db, day: jiff::civil::Date, event: &str, calls: u32, tokens: u64)
    -> Result<(), StoreError>;                                    // UPSERT, прибавка
// snapshots.rs
pub struct SnapshotRow { pub session_id: String, pub taken_at: Timestamp, pub trigger: SnapTrigger,
    pub agent: Option<String>, pub status: SnapStatus, pub skipped: u32, pub bytes: u32,
    pub data: SnapshotData }
pub async fn put_snapshot(db: &Db, row: &SnapshotRow) -> Result<(), StoreError>;   // перезапись
pub async fn get_snapshot(db: &Db, session: &str) -> Result<Option<SnapshotRow>, StoreError>;
pub async fn gc_snapshots(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError>;
```

- [ ] **Step 1: Фикстура 0024 и тест миграции** `migrate_0025_from_m57_db` (FAIL):
  - база 0023 по образцу `acceptance_migration_from_m56_db` → 0024 → в неё добавляются идея,
    вопрос-предложение, задача из «начать», `session_seen` идеи, отчёт с картинкой, `code_refs`;
  - сборка вне `users/`, на место — жёсткой ссылкой закрытой базы;
  - проверки:
    - строки `session_seen` в `epoch = 0`;
    - `sqlite_master` совпадает, кроме ожидаемых объектов;
    - пять триггеров отчёта срабатывают;
    - `foreign_key_check` пуст, `integrity_check` ok;
    - есть `bak-v24`;
    - `recv_count = 0`;
    - идея «показана» в своей сессии.
- [ ] **Step 2: Тесты функций** (FAIL): `bump_epoch_hides_old_marks`, `seen_ideas_ignore_epoch`,
  `unattributed_upsert_adds`, `snapshot_put_overwrites`, `gc_sessions_drops_snapshots`,
  `gc_snapshots_by_cutoff`.
- [ ] **Step 3: Реализация.** Все вызовы `mark_seen`/`seen_revs` в workspace получают эпоху. До
  задачи 7a она всегда 0.
- [ ] **Step 4: Проверка.** `cargo test -p zam-store $T` и `cargo test -p zam-server --test sessions
  --test session_tokens $T` — PASS.
- [ ] **Step 5: Commit** `feat(store): миграция 0025 — эпохи, holdout, учёт, снимки, recv_count`.

### Task 1b: Набор ТС-1, нагеты, заморозка (§3.1–§3.2)

**Files.** Modify: `eval/scenarios.toml`, `xtask/src/eval/nuggets.rs`, `eval/README.md`. Create:
`eval/nuggets.lock` (`{nuggets_fingerprint, corpus_fingerprint, scenarios, nuggets}`).

- [ ] **Step 1: Проверки `--check`** (FAIL на синтетике):
  - `nugget_occurs_once` — пробелы нормализуются, как у `evidence`;
  - `nugget_not_in_l0`;
  - `every_expect_has_nugget`;
  - `low_overlap_share_at_least_40pct` — доля общих основ ≤ 0,2. Основы здесь — snowball ru/en
    (`rust-stemmers`, только в xtask, справочная мера).
- [ ] **Step 2: Набор** — ≥ 45 сценариев по составу §3.1, по 1–4 нагета на источник, 20–200
  символов. `cargo xtask eval --check` — зелёный.
- [ ] **Step 3: Независимая разметка** — ≥ 20 % сценариев (по зерну) отдельным субагентом: корпус и
  вопросы без кода M6. Согласие — в журнал; если оно < 0,7, расхождения разбираются до заморозки.
- [ ] **Step 4: Заморозка — отдельный коммит** `test(eval): заморозка нагетов и корпуса ТС-1`
  (`eval/nuggets.lock`, `eval/scenarios.toml`).

### Task 3: Привязка MCP к сессии (§7.1, рецензия плана I1–I2)

**Files.**
- Create: `crates/zam-cli/src/cmd/hook/{binding.rs,procs.rs}`, `crates/zam-cli/tests/mcp_bind.rs`.
- Modify:
  - `crates/zam-cli/Cargo.toml` — фича `test-hooks = []`; `windows` cfg(windows) с
    `Win32_System_Diagnostics_ToolHelp`;
  - `crates/zam-cli/src/cmd/{mcp.rs,hook/session_start.rs,hook/cache.rs}` — в `gc_cache` префикс
    `agent-`;
  - `crates/zam-server/src/mcp/{mod.rs,tools.rs}`, `crates/zam-server/src/api/{sessions.rs,search/mod.rs,read.rs}`.
- Test: `crates/zam-server/tests/{mcp.rs,session_tokens.rs}`.

**Interfaces — Produces:**
```rust
// procs.rs — цепочка предков процесса
pub struct Proc { pub pid: u32, pub name: String }
pub fn ancestors(max: usize) -> Vec<Proc>;
//   Windows: CreateToolhelp32Snapshot + Process32FirstW/NextW (th32ParentProcessID, szExeFile);
//   Linux: /proc/<pid>/stat — поле 4 (ppid), поле 2 (comm). Ошибка — пустой список.
//   test-hooks: thread_local подмена списка (`procs::set_fake_ancestors(Vec<Proc>)`) для тестов.
pub const AGENT_PROC_NAMES: [&str; 3] = ["codex", "claude", "node"];   // без расширения, нижний регистр
// binding.rs
pub const BINDING_ANCESTORS_MAX: usize = 4;
pub fn write_binding(root: &Path, session: &str) -> io::Result<()>;
//   agent-<pid>.json {session_id, started_at} на каждого предка до первого AGENT_PROC_NAMES включительно
pub fn read_binding(root: &Path) -> Option<String>;   // первый найденный по своей цепочке
// сервер
pub struct McpOrigin { pub implicit: bool }   // расширение http::Extensions, ставит только Rest
pub struct RequestSession { pub touch: SessionTouch, pub db: Db, pub explicit: bool, pub via_mcp: bool }
//   explicit = заголовок/тело с сессией и НЕ (via_mcp && implicit)
// учёт: tools::call пишет событие "mcp" (явная сессия → session_tokens, иначе add_unattributed);
//   search/read внутри MCP (via_mcp) событий search/read не пишут — двойного счёта нет
// GET /sessions/unattributed?days=N → {items: [{day, event, calls, tokens}]}
```
`zam mcp` ставит `X-Zam-Session` по порядку: `ZAM_SESSION_ID` → `CLAUDE_CODE_SESSION_ID` →
`read_binding` (при `initialize` и раз в 30 с). Прогоны ТС-2 из xtask задают `ZAM_SESSION_ID`.

- [ ] **Step 1: Тесты** (FAIL):
  - CLI (`tests/mcp_bind.rs`): `binding_written_on_session_start_clear_resume` (после `resume` —
    новая сессия в файле), `env_session_beats_binding`;
  - юнит в `binding.rs`: `binding_found_via_fake_ancestors` — подмена предков `procs::set_fake_ancestors`
    под `test-hooks`, без новой переменной `ZAM_*`;
  - сервер: `mcp_explicit_session_marks_seen`, `implicit_session_no_marks_goes_unattributed`,
    `mcp_origin_not_forgeable_by_header` (внешний запрос с любыми заголовками —
    `via_mcp = false`), `mcp_search_counts_once`, `subagent_on_shared_mcp_keeps_snippet` (фрагмент
    и ` (выдавалось)`; `skip_seen: true` прячет).
- [ ] **Step 2: Реализация** по §7.1; DEBUG-признак `implicit`. Описание `(seen)` у MCP — в
  `guide` (п. 1 «Карточки», строка 17): «(выдавалось) — уже было в сессии, перечитывать не нужно,
  если текст у тебя есть».
- [ ] **Step 3: Проверка.**
  - `cargo test -p zam-cli --bin zam --features test-hooks $T binding` — PASS;
  - `cargo test -p zam-cli --test mcp_bind $T` — PASS;
  - `cargo test -p zam-server --test mcp --test session_tokens $T` — PASS;
  - `cargo test -p zam-cli --test m4_acceptance $T hook_budgets` — PASS: `session-start` с записью
    привязки ≤ 300 мс.
- [ ] **Step 4: Commit** `feat(mcp): привязка zam mcp к сессии, явная сессия и учёт без неё`.

### Task 4: Сессионный набор ТС-2 (§3.1, §3.3)

**Files.** Create: `eval/sessions.toml`, `xtask/src/eval/sessions.rs`. Modify: `xtask/src/eval/{run.rs,report.rs}`,
`eval/README.md`.

**Interfaces — Consumes:** `McpClient`, `ServerCaps`, `metrics` (1a1), `eval/sessions-corpus` (1c),
привязка (3). **Produces:** `cargo xtask eval --sessions [--profile standard|heavy] [--ideas on|off]
[--base B5.6|B5.7|B4] --split dev|holdout`. Выдаёт:
- `R` по событиям;
- полноту обязательных строк;
- `hint_recall` — отдельно общий (без подсказок-идей) и подсказки-идеи (доля и совпадение с B5.7);
- долю повторной выдачи и совпадение дайджеста при двух стартах;
- строки «идеи» и «снимок сессии».

- [ ] **Step 1: Набор** — ≥ 12 сессий: стандартный профиль ≥ 8, тяжёлый ≥ 4, события и идеи по
  §3.1. Поле `transcript` у сессий заполняет задача 17d.
  - Хуки — `zam hook <событие>` (stdin в формате Claude Code).
  - MCP — `zam mcp` с `ZAM_SESSION_ID` и без него (неявный путь).
- [ ] **Step 2: Тесты раннера** (FAIL, мок-сервер): `required_lines_detected`,
  `idea_hints_excluded_from_general_hint_recall`, `idea_hints_compared_to_b57`,
  `events_missing_in_b4_excluded_both_arms`. Run: `cargo test -p xtask $T sessions`.
- [ ] **Step 3: Реализация и пилот** на B5.6, B5.7 и B4. Числа — в журнал.
- [ ] **Step 4: Commit** `feat(eval): сессионный набор ТС-2`.

### Task 5: Карточки v2, честный бюджет, ответы записи (§4.1–§4.3, §5, §13 Markdown v2)

**Files.** Modify:
- `crates/zam-server/src/api/search/{mod.rs,cache_step.rs}` — `pack_items` около стр. 1406,
  рендер карточек около стр. 1688, 1740, 2781;
- `crates/zam-server/src/api/recall.rs` — `SNIPPET_TOKENS`, строки подсказок;
- `crates/zam-server/src/api/cache.rs` — `echo`;
- `crates/zam-server/src/mcp/tools.rs`, `crates/zam-server/src/config.rs`,
  `deploy/config.example.toml`;
- `crates/zam-server/src/agent_setup/guide.{ru,en}.md` — п. 1 «Карточки»: без «N дн. назад»,
  легенда хвоста;
- `xtask/src/eval/tokens.rs` — `refs_in_text` на строках v2.

Test: `crates/zam-server/tests/{search.rs,cache_api.rs,mcp.rs}`.

**Interfaces — Produces:**
```rust
// config [search]: budget_tokens: usize (1..=200000, 800), brief_min_rel: f32 (0..=1, 0)
// config [recall]: snippet_tokens: usize (0..=200, 60)
fn pack_items(items: Vec<Item>, mode: SearchMode, budget_tokens: usize, counter: &dyn TokenCounter)
    -> Vec<Packed>;   // стоимость — вся строка карточки
fn card_tail(score: f32, rev: u32, updated_rfc3339: &str, tokens: usize, cert: Option<Certainty>,
             status: Option<EntryStatus>) -> String;   // "[.82] r7 · 2026-10-10 14:03:22 · 340t"
// POST /cache {…, echo?: bool}: без echo — без summary, остальные поля прежние; echo: true — форма v0.0.10
```

- [ ] **Step 1: Тесты** (FAIL):
  - `brief_markdown_within_budget` — бюджеты 100, 400, 800, 2000 счётчиком сервера;
  - `brief_budget_edges_first_card_only`, `snippet_cut_on_char_boundary`;
  - `card_v2_tail_format` — строки §4.1 дословно;
  - `brief_min_rel_drops_weak`;
  - `cache_put_without_echo_has_no_summary`, `cache_put_echo_true_old_shape` — побайтово с
    эталоном, снятым до правки (шаг 0);
  - `mcp_cache_put_one_line_le_80_tokens`, `mcp_update_entry_one_line_le_80_tokens`;
  - `memory_log_warnings_line_per_warning`;
  - `recall_hint_line_v2` — `ZeroAIMemory: <заголовок> [<вид>·<cert>] i:<id>`;
  - `refs_in_text_parses_v2` (xtask).
- [ ] **Step 0 (до правки): эталон** ответа `POST /cache` с `echo: true` —
  `crates/zam-server/tests/fixtures/cache_echo_m57.json` (снять тестом `--ignored`
  `snapshot_cache_echo_fixture`).
- [ ] **Step 2: Реализация.** `DEFAULT_BUDGET_TOKENS` удаляется. Проверка:
  - `cargo test -p zam-server --test search --test cache_api --test mcp $T` — PASS;
  - `cargo test -p xtask $T refs_in_text` — PASS.
- [ ] **Step 3: Сетки на dev ТС-1** (§3.5, п. 1–2):
  - `budget_tokens` {400, 600, 800} × `brief_min_rel` {0; 0,4; 0,5};
  - затем `snippet_tokens` {40, 60};
  - команда — `cargo xtask eval --tokens --via mcp --split dev --grid eval/grid-m6-search.toml`;
  - выбор по правилу §3.5 — в журнал и `config.rs`.
- [ ] **Step 4: Commit** `feat(search): карточки v2 и честный бюджет`.

### Task 6: Дата UTC в тексте агенту, замороженные часы (§4.4)

**Files.** Create: `crates/zam-core/src/time.rs`, `web/src/lib/time.ts`. Modify: места выдачи
(инвентаризация шага 1), `crates/zam-store/src/{time.rs,test_hooks.rs}`.
Test: `crates/zam-server/tests/render.rs`, `crates/zam-cli/tests/*`.

**Interfaces — Produces:**
```rust
// zam-core/src/time.rs — без jiff: из строки RFC 3339 UTC
pub fn agent_ts(rfc3339: &str) -> Option<String>;     // "2026-10-10T14:03:22.123Z" → "2026-10-10 14:03:22"
pub fn agent_date(rfc3339: &str) -> Option<String>;   // → "2026-10-10"
// сервер вызывает agent_ts(&fmt_ts(t)); CLI — на строках JSON ответа
// zam-store/src/time.rs — источник времени
pub fn now() -> jiff::Timestamp;                       // все «сейчас» сервера и хранилища — через неё
#[cfg(feature = "test-hooks")] pub fn freeze_now(t: jiff::Timestamp) -> FreezeGuard;
//   FreezeGuard держит существующую блокировку GATE (`zam-store/src/test_hooks.rs`, write) до drop:
//   тесты с замороженными часами не идут параллельно с прочими точками test-hooks
```
```ts
export function localTime(iso: string): string;        // RFC 3339 UTC → локальное время браузера
```

- [ ] **Step 1: Инвентаризация** — `rg -n 'strftime|%Y-%m-%d|to_rfc3339|Timestamp::now' crates`;
  перечень — в журнал.
- [ ] **Step 2: Тесты** (FAIL):
  - `markdown_has_no_iso_t` — Markdown MCP, хуки, текст `zam` на сценариях ТС-1 и ТС-2;
  - `json_dates_unchanged` — RFC 3339;
  - `utc_storage_audit` — все `*_at` оканчиваются на `Z`;
  - `agent_ts_cases` — дробные секунды, без них, мусор → `None`;
  - `freeze_now_applies` (`--features test-hooks`).
- [ ] **Step 3: Реализация.** Все «сейчас» на путях выдачи и окон — через `zam_store::time::now()`.
  Проверка: `cargo test -p zam-core -p zam-store $T`; `cargo test -p zam-server --test render
  --test search --test recall $T` — PASS.
- [ ] **Step 4: Commit** `feat(render): дата UTC yyyy-MM-dd HH:mm:ss в тексте агенту, часы для тестов`.

### Task 7a: Эпохи и «уже выдавалось» (§7.2–§7.3)

**Files.** Modify: `crates/zam-server/src/api/{sessions.rs,read.rs,recall.rs,search/mod.rs}`,
`crates/zam-cli/src/cmd/hook/{checkpoint.rs,session_start.rs}`. Test: `crates/zam-server/tests/{sessions.rs,recall.rs,
ideas_recall.rs,read_batch.rs}`, `crates/zam-cli/tests/hooks.rs`.

**Interfaces — Consumes:** `bump_epoch`, `SessionTouch.epoch`, `RequestSession.explicit` (2, 3).
**Produces:**
- `POST /sessions {id, new_epoch: true}` — отдельный запрос → 201 `{id, epoch}`, как у прочих
  `POST /sessions`.
- Эпоху увеличивают:
  - `session_start` с `source` `compact|resume|clear`;
  - `event: recovery`;
  - `PreCompact` (параллельно со сбросом очереди, в 100 мс);
  - явный `new_epoch`.
- `/read` при той же ревизии и эпохе выдаёт полный текст, в заголовке — `· уже выдавалось в этой
  сессии`; в JSON — `seen: true`.
- Счётчик повторной выдачи — колонка `session_tokens.reread_calls` (миграция 0025, задача 2):
  `/read` с пометкой «уже выдавалось» прибавляет 1 к строке события `read`. `GET /sessions/{id}`
  отдаёт `tokens.read.reread_calls`. Из него WebUI (8b) показывает долю.

- [ ] **Step 0 (до правки): эталон** `/read` без сессии на 10 целях —
  `crates/zam-server/tests/fixtures/read_l2_m57.json` (тест `--ignored`).
- [ ] **Step 1: Тесты** (FAIL):
  - `compact_resets_seen` — сценарий §17 п. 7 по REST и хукам;
  - `epoch_bump_without_session_start`, `double_epoch_bump`;
  - `ideas_seen_across_epochs`;
  - `checkpoint_precompact_new_epoch_within_100ms`;
  - `old_server_new_epoch_400_silent`;
  - `reread_full_text_with_mark`, `reread_counter_in_session_tokens`;
  - `read_without_session_byte_equal_m57` — сравнение с эталоном шага 0.
- [ ] **Step 2: Реализация** по §7.2–§7.3. Проверка: `cargo test -p zam-server --test sessions
  --test recall --test ideas_recall --test read_batch $T`; `cargo test -p zam-cli --test hooks $T` —
  PASS.
- [ ] **Step 3: Commit** `feat(sessions): эпохи seen и пометка «уже выдавалось»`.

### Task 7b: Явный фокус (§6)

**Files.** Create: `crates/zam-server/src/api/read_focus.rs`, `crates/zam-server/tests/read_focus.rs`. Modify:
`crates/zam-server/src/api/read.rs`, `crates/zam-server/src/mcp/tools.rs`, `crates/zam-cli/src/cmd/read.rs`,
`crates/zam-server/src/config.rs`.

**Interfaces — Produces:**
```rust
pub struct ReadItem { …, pub focus: Option<String> }    // tier=focus требует focus
// [read] focus_leaves 1..=10 (3), focus_min_tokens 0..=20000 (600), focus_timeout_ms 1..=10000 (80)
pub async fn focus_read(st: &AppState, target: &Target, focus: &str, cfg: &ReadConfig)
    -> Result<FocusOut, ApiError>;       // оглавление ≤ 40 заголовков/150 токенов, листы, пропуски
// warnings: "focus skipped: short", "focus lexical"; 400 invalid: "нужен текст focus";
//   focus с section|locator|since_rev — 400 invalid
// CLI: zam read <id> --focus "<запрос>"
```

- [ ] **Step 1: Тесты** (FAIL):
  - `read_without_focus_byte_equal_l2` — эталон 7a, шаг 0;
  - `focus_toc_le_150_tokens`;
  - `focus_skip_marker_format` — `[пропущено: <путь заголовков> · ~N t]`;
  - `focus_short_target_skipped`, `focus_journal_entry_whole`;
  - `focus_timeout_lexical_fallback` — задержка точкой `test-hooks`;
  - `focus_doc_blob_locator` — `d:`/`b:` → `locator_text`, оглавление до 40 частей;
  - `focus_chunk_whole` — `a:<chunk>` целиком;
  - `focus_marks_only_leaves_seen`;
  - `focus_p95` (`--ignored`): CPU int8 ≤ 120 мс, GPU ≤ 50 мс, 5 целей.
- [ ] **Step 2: Реализация.** Проверка: `cargo test -p zam-server --test read_focus $T` — PASS;
  `cargo test -p zam-server --test read_focus $T -- --ignored focus_p95` — числа в журнал.
  Справочная политика «явный фокус» — в отчёте `eval --tokens`.
- [ ] **Step 3: Commit** `feat(read): явное фокусное чтение`.

### Task 8a: Rules-резерв, индексы, детерминированность, идеи (§8)

**Files.** Modify: `crates/zam-server/src/api/recall.rs` (`digest` около стр. 1650, `arrange` около
стр. 3006, `digest_ideas`), `crates/zam-server/src/config.rs`. Test: `crates/zam-server/tests/{recall.rs,
recall_recovery.rs,recall_brief.rs}`.

**Interfaces — Produces:**
- Ключи `[recall]`:
  - `digest_tokens` 1..=20000, 1500 (без Rules);
  - `recovery_tokens` — 2000;
  - `brief_tokens` — 800;
  - `digest_entries` 1..=20, 5;
  - `digest_code_map_tokens` 0..=1000, 0.
- Rules — до `RULES_MAX` (20), L1 ≤ `RULE_L1_TOKENS` (200), сверх бюджета раздела.
- Окна `ANSWERS_DAYS` и `RECOVERY_DECISIONS_DAYS` считаются от начала суток UTC
  (`zam_store::time::now()`).
- Порядок свёрток — без `usage` от показов.

- [ ] **Step 1: Тесты** (FAIL):
  - `rules_reserved_never_cut` — `digest_tokens` 100, все Rules в выдаче;
  - `digest_deterministic_two_sessions` — `freeze_now`, побайтово равный `text`;
  - `digest_order_ignores_recall_shown`;
  - `ideas_section_not_cut_by_budget`, `no_ideas_in_recovery_brief_focus`;
  - `changelog_index_line_format` — `c:<id> «заголовок» · Nt`;
  - `code_map_pointer_only_at_zero`.
- [ ] **Step 2: Реализация.** Проверка: `cargo test -p zam-server --test recall --test
  recall_recovery --test recall_brief $T` — PASS.
- [ ] **Step 3: Commit** `feat(recall): Rules-резерв и детерминированный компактный дайджест`.

### Task 8b: Учёт, `zam sessions`, страница сессий WebUI, `rules_heavy` (§8)

**Files.**
- Create:
  - `crates/zam-cli/src/cmd/sessions.rs` (регистрация в `cmd/mod.rs` и `main.rs`);
  - `crates/zam-cli/tests/sessions.rs`;
  - `web/src/components/SessionsView.svelte` — новая страница, маршрут `#/sessions`;
  - `web/e2e/sessions.spec.ts`.
- Modify:
  - `crates/zam-server/src/api/{index.rs,sessions.rs}`;
  - `web/src/lib/{api.ts,router.ts}`, `web/src/routes/{Shell,IndexStatus}.svelte`,
    `web/src/components/Tree.svelte`.

**Interfaces — Produces:**
- Учёт `session_tokens`: события `search`, `read` (REST и CLI с явной сессией) и `mcp`.
- CLI: `zam sessions tokens [<id> | --last N] [--by-holdout]` — таблица «событие · вызовов ·
  токенов».
- `GET /index/status` — в ответ добавляется `"rules": {"solution": <guid>, "count": N, "l1_tokens":
  M, "warning": "rules_heavy" | null}`. Область — Rules текущего решения (`X-Zam-Solution`).
  `rules_heavy` — при > 10 Rules или Σ L1 > 1500.
- WebUI: страница сессий (список, «событие · вызовов · токенов», доля «уже выдавалось» из
  колонки `session_tokens.reread_calls` через `tokens.read.reread_calls`), плашка `rules_heavy` у папки Rules и в «Состоянии индекса».

- [ ] **Step 1: Тесты** (FAIL):
  - сервер: `session_tokens_search_read_mcp`, `index_status_rules_heavy_shape`;
  - CLI: `cli_sessions_tokens_table`, `cli_sessions_tokens_last`;
  - e2e: `sessions page shows tokens and reread share`, `rules heavy banner`.
- [ ] **Step 2: Реализация.** Проверка:
  - `cargo test -p zam-server --test session_tokens --test index_api $T`;
  - `cargo test -p zam-cli --test sessions $T`;
  - `npm run check`, `npm run build`, `npm run size`;
  - `npm run e2e -- sessions` на release.
- [ ] **Step 3: Commit** `feat(sessions): учёт search/read/mcp, zam sessions tokens, страница сессий`.

### Task 8c: Сетки ТС-2 и полный прогон (§3.5 п. 3–5)

- [ ] **Step 1: Сетки на dev ТС-2** по таблице §8:
  - `digest_tokens` × `digest_entries`;
  - `recovery_tokens`;
  - `brief_tokens` × `code.brief_tokens`.

  Условие — полнота обязательных строк = 1,0. Команда — `cargo xtask eval --sessions --split dev
  --grid eval/grid-m6-recall.toml`. Выбор — в журнал и `config.rs`.
- [ ] **Step 2: Полный прогон** `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast $T` —
  итог в журнал (ожидается 0 failed; флейки перепроверяются отдельно).
- [ ] **Step 3: Commit** `perf(recall): бюджеты дайджеста по сеткам ТС-2` (`config.rs`,
  `deploy/config.example.toml`, `eval/grid-m6-recall.toml`).

### Task 9: Пороги подсказок, общий код совпадения слов (§9, рецензия плана C2)

**Files.**
- Create: `crates/zam-core/src/text.rs` — стоп-слова переносятся из
  `crates/zam-markdown/src/boundary.rs`.
- Modify:
  - `crates/zam-markdown/src/{boundary.rs,lib.rs}` — `pub use zam_core::text::is_stop_word`;
  - `crates/zam-search/src/recall.rs` — около стр. 192–201;
  - `crates/zam-server/src/api/recall.rs` — около стр. 1030–1110: идеи переходят на общий код;
  - `eval/hints.toml`, `xtask/src/eval/hints.rs`, `crates/zam-server/src/config.rs`.
- Test: юнит `zam-search`, `crates/zam-server/tests/ideas_recall.rs`.

**Interfaces — Produces:**
```rust
// zam-core/src/text.rs — без зависимостей (поэтому zam-search не тянет zam-markdown)
pub fn is_stop_word(w: &str) -> bool;            // тот же список STOP_WORDS, побайтово
// zam-search/src/recall.rs — перенос алгоритма идей БЕЗ изменения поведения
pub const STEM_MIN_CHARS: usize = 4;
pub fn norm_word(w: &str) -> String;             // нижний регистр, ё → е (бывш. idea_norm_word)
pub fn stem(w: &str) -> String;                  // первые max(4, len − 3) символов (бывш. idea_stem)
pub struct StemWord { … } impl StemWord { pub fn new(w: String) -> Self; pub fn matches(&self, o: &StemWord) -> bool; }
pub enum WordMatch { Plain, Stems }              // Stems = norm_word + is_stop_word + StemWord::matches
pub fn prepared_terms(terms: &[String], m: WordMatch) -> Vec<String>;   // бывш. idea_terms при Stems
pub struct CoverageRule { pub divisor: u32, pub cap: u32, pub word_match: WordMatch }
pub fn coverage_enough(query: &[String], text: &str, rule: &CoverageRule) -> bool;
// правило идей «≥ 1 совпадение с заголовком + min(порог, max(1, ⌈k/2⌉))» — отдельная функция
// идей в api/recall.rs (IdeaQuery::matches), вызывает общие StemWord/prepared_terms
// [recall] post_tool_coverage = {divisor, cap, word_match}; prompt_min_score_fts 0..=1 (0,6)
```
Snowball (`rust-stemmers`) — только справочно в `xtask --check`, в рабочем коде его нет.

- [ ] **Step 1: Тест эквивалентности** (до переноса — PASS на старом коде, после — тоже):
  `idea_hints_top5_match_m57` сравнивает подсказки-идеи с `idea_hints_m57.json` (1a2).
  Плюс `stop_words_identical_after_move`.
- [ ] **Step 2: Набор** `hints.toml`:
  - ≥ 120 случаев, из них ≥ 48 `post_tool`, половина нерелевантных;
  - корзины длин ≤ 3, 4–8, 9–20, > 20 — по ≥ 10;
  - классы — сборка, тесты, git, кодировки, сеть.
- [ ] **Step 3: Реализация переноса.** Проверка:
  - `cargo test -p zam-core -p zam-search -p zam-markdown $T`;
  - `cargo test -p zam-server --test ideas_recall $T` — PASS, эквивалентность сохранена.
- [ ] **Step 4: Сетка.**
  - Делитель {3, 2} × `cap` {3, 4, 6} × `WordMatch` {Plain, Stems} на `hints.toml`, отчёт по
    корзинам: `cargo xtask eval --hints --grid eval/grid-m6-hints.toml`.
  - `prompt_min_score_fts` {0,2…0,6} — на `--mode fts`.
  - Правила §9; решение — в журнал. Перепроверка — тем же прогоном (7a уже позади).
- [ ] **Step 5: Commit** `feat(hints): общий код совпадения слов, пороги по eval подсказок`.

### Task 10: Буст вида (§10)

**Files.** Create: `eval/kind-queries.toml`. Modify: `crates/zam-server/src/api/search/mod.rs`,
`crates/zam-server/src/config.rs`. Test: `crates/zam-server/tests/search_rerank_boosts.rs`.

**Interfaces — Produces:** `[search] kind_boost_mode = "always" | "unless_contradicted" |
"reliable_only"`. Противоречие проверяется среди всех кандидатов после слияния, до `top_k`.

- [ ] **Step 0 (до правки): эталон** выдачи `queries.toml` при `always × 1,05` —
  `.tmp/m6/task10/always-m57.json`.
- [ ] **Step 1: Набор** — ≥ 24 запроса к корпусу 1c. Условие «не на потолке» (≥ 60 % с nDCG@10 < 1
  при 1,0) — в журнал **до** сетки.
- [ ] **Step 2: Тесты** (FAIL): `unless_contradicted_sees_cut_candidates`,
  `reliable_only_boosts_theorem_axiom`, `always_byte_equal_m57` (эталон шага 0).
- [ ] **Step 3: Реализация.** Проверка: `cargo test -p zam-server --test search_rerank_boosts $T` —
  PASS.
- [ ] **Step 4: Сетка** §10 — гибрид CUDA и CPU, `rerank=on` на CUDA. Правило §10, выбор — в журнал.
- [ ] **Step 5: Commit** `feat(search): режим буста вида по набору вне потолка`.

### Task 11: `boost_area`, `w_attach` (§11)

**Files.** Create: `eval/area-queries.toml`. Modify: `crates/zam-server/src/api/search/mod.rs`,
`crates/zam-server/src/config.rs`. Test: `crates/zam-server/tests/attachments_search.rs`.

**Interfaces — Produces:** `[search] w_attach: f32` (0..=10, 1,0).

- [ ] **Step 0 (до правки): эталон** выдачи `docs-queries.toml` — `.tmp/m6/task11/docs-m57.json`.
- [ ] **Step 1: Тест** (FAIL): `w_attach_1_byte_equal_m57`.
- [ ] **Step 2: Реализация и набор** — ≥ 30 запросов `same`/`other`/`none`, по ≥ 10. Проверка:
  `cargo test -p zam-server --test attachments_search $T` — PASS.
- [ ] **Step 3: Сетки** `boost_area` {1,0; 1,05; 1,10; 1,20; 1,30} и `w_attach` {0,7; 1,0; 1,3; 1,6};
  правила §11; выбор — в журнал.
- [ ] **Step 4: Commit** `feat(search): boost_area и w_attach по eval`.

### Task 12: Стенд A/B, `checkpoint_impact`, `holdout` (§12)

**Files.**
- Create: `xtask/src/ab.rs`, `xtask/tests/ab.rs`, `eval/ab/{tasks.toml,runs.toml}`.
- Modify:
  - `crates/zam-cli/src/cmd/hook/mod.rs` — `Settings`: ключи `[hooks]` `checkpoint_impact`,
    `holdout`;
  - `crates/zam-cli/src/cmd/hook/{checkpoint.rs,session_start.rs}`;
  - `crates/zam-server/src/api/{sessions.rs,recall.rs}`, `eval/README.md`.

**Interfaces — Produces:**
- `cargo xtask ab --transcripts <каталог> --runs eval/ab/runs.toml --server <url>` → отчёт по
  плечам с интервалами. Из транскриптов читаются только поля `usage`, файлы не копируются.
- `POST /sessions {id, holdout: true}` — отдельный запрос; 400 старого сервера игнорируется.
- `GET /sessions/{id}` → `holdout`; `/recall` в такой сессии отдаёт `text: ""`, учёт ведётся.
- Клиентские ключи: `hooks.checkpoint_impact` (bool, false), `hooks.holdout` (0..=0,5, 0).

- [ ] **Step 1: Тесты** (FAIL):
  - xtask: `ab_report_on_fixture`;
  - сервер: `holdout_empty_text_and_inherited` — субагент и сессия после compact;
  - CLI: `checkpoint_impact_lines_inside_single_reminder`, `checkpoint_impact_timeout_60ms_no_lines`.
- [ ] **Step 2: Реализация.** `POST /code/impact` идёт параллельно с `GET /sessions/{id}`, срок 60 мс
  внутри 100 мс. Проверка:
  - `cargo test -p xtask --test ab $T`;
  - `cargo test -p zam-server --test sessions $T`;
  - `cargo test -p zam-cli --test hooks $T`;
  - `cargo test -p zam-cli --test m4_acceptance $T hook_budgets` с `checkpoint_impact = true` — ≤
    100 мс.

  Всё — PASS.
- [ ] **Step 3: Инструкция владельцу** в `eval/README.md`: ≥ 10 задач × 2 плеча × 2 повтора.
  **Прогон — владелец, вне приёмки M6.**
- [ ] **Step 4: Commit** `feat(ab): стенд агентного A/B, checkpoint_impact, holdout`.

### Task 14: Ранжирование карты кода (§12а)

**Files.** Modify:
- `crates/zam-code/src/{proto.rs,rank.rs}` — `EXTRACT_VERSION` 2 → 3, `RefAgg`, `ref_kind_weight`
  около стр. 95;
- `crates/zam-code/src/extract/*`;
- `crates/zam-store/src/code.rs`;
- `crates/zam-cli/src/cmd/code/{config.rs,sync.rs,state.rs}` — `exclude_defaults`, версия сервера;
- `crates/zam-server/src/api/code.rs`, `crates/zam-server/src/mcp/tools.rs`,
  `web/src/components/CodeView.svelte`.

Test: `crates/zam-code/tests/*`, `crates/zam-server/tests/{code_api.rs,code_memory.rs}`,
`crates/zam-cli/tests/code.rs`.

**Interfaces — Consumes:** `code_refs.recv_count` (2). **Produces:**
```rust
pub struct RefAgg { pub name: String, pub kind: RefKind, pub count: u32, pub first_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub recv_count: Option<u32> }
fn ref_kind_weight(kind: RefKind) -> f64;   // type 2,0; impl 2,0; call 1,0; import 1,0; other 1,0
// вклад call = (count − recv_count) × 1,0 + recv_count × 0,25
// cmd/code/config.rs: DEFAULT_EXCLUDE = ["**/tests/fixtures/**"]; exclude_defaults: bool (true)
```

- [ ] **Step 0 (до правки): эталон** рангов карты репозитория на `v0.0.10-m5.7` (бинарник тега
  1a2) — `.tmp/m6/task14/ranks-m57.json`.
- [ ] **Step 1: Тесты** (FAIL):
  - `recv_call_detected_rust_ts_py` — `x.f()`, `self.f()`, `a.b.f()` против `f()`, `T::f()`,
    `use …::f`;
  - `old_server_gets_no_recv_count`;
  - `ranks_unchanged_when_recv_count_zero` — данные старого клиента → ранги совпадают с эталоном
    шага 0;
  - `fixtures_excluded_by_default_and_pruned`, `exclude_defaults_false_keeps`;
  - `weights_table_fixed`;
  - `extractor_metrics_1_000`;
  - `line_anchor_symbol_link_by_id` — WebUI и MCP для `L10`.
- [ ] **Step 2: Реализация.** Проверка:
  - `cargo test -p zam-code $T`;
  - `cargo test -p zam-server --test code_api --test code_memory $T`;
  - `cargo test -p zam-cli --test code $T`.

  Всё — PASS.
- [ ] **Step 3: Замер** `cargo xtask code-bench`:
  - на снимке `HEAD` `045ef40` (прогон 4 задачи 9C M5.6, ТЗ §18.2; справочно — прогон 3,
    `4cf804f`) и на финальном `HEAD`;
  - абляция — справочно;
  - прочие цели — не хуже 9C + 10 %.

  Факт (≥ 6/10 или число) — в журнал.
- [ ] **Step 4: Commit** `feat(code): ранжирование карты — fixtures, вызов по получателю, веса`.

### Task 15a: Долги M5.5 — ссылки, миниатюры, «PDF / печать»

**Files.** Modify: `crates/zam-cli/src/cmd/report.rs`, `crates/zam-server/src/api/{reports.rs,previews.rs}`,
`web/src/components/ReportView.svelte`, `web/src/lib/print.ts`, лента отчётов. Test:
`crates/zam-server/tests/reports.rs`, `crates/zam-cli/tests/{report.rs,m55_acceptance.rs}`,
`crates/zam-server/tests/m55_acceptance.rs`, `web/e2e/{reports.spec.ts,reports-print.spec.ts}`.

**Interfaces — Produces:** `pub const REPORT_LINK_SCHEMES: [&str; 5] = ["http", "https", "mailto",
"vscode", "file"];` — общее у CLI и сервера (`zam-core`).

- [ ] **Step 1: Тесты** (FAIL):
  - `report_link_schemes` — 201 для пяти схем, 422 для `ftp:` и `javascript:`;
  - `report_preview_token_404`;
  - e2e:
    - кнопка «копировать путь» у `file:`/`vscode:`, `file:` без перехода;
    - `rel="noopener noreferrer"` у внешних ссылок;
    - в печати виден адрес;
    - лента грузит `kind=thumb`;
    - кнопка «PDF / печать» с подсказкой — `reports-print.spec.ts`, `acceptance_pdf_from_cli_report`
      по новой подписи.
- [ ] **Step 2: Реализация.** Проверка:
  - `cargo test -p zam-server --test reports --test m55_acceptance $T`;
  - `cargo test -p zam-cli --test report --test m55_acceptance $T`;
  - `npm run check`, `npm run build`, `npm run size`;
  - `npm run e2e -- reports` на release.

  Всё — PASS.
- [ ] **Step 3: Commit** `fix(reports): схемы vscode:/file:, серверные миниатюры, «PDF / печать»`.

### Task 15b: Долги M5.6 — `zam-secrets`, счётчик повторов, флейк, e2e входа (рецензия плана I3)

**Files.**
- Create: `crates/zam-secrets/{Cargo.toml,src/lib.rs}` — перенос `crates/zam-markdown/src/secrets.rs`
  без изменения поведения; зависимость только `regex`.
- Modify:
  - `crates/zam-markdown/src/lib.rs` — `pub use zam_secrets as secrets` и прежние реэкспорты;
  - `Cargo.toml` workspace — член `crates/zam-secrets`;
  - `crates/zam-server/tests/code_api.rs`, `web/e2e/user-login.spec.ts`.

**Interfaces — Produces:**
```rust
pub struct ScanOutcome { pub hits: Vec<SecretHit>, pub passes: u32, pub depth_capped: bool }
pub fn scan_secrets_ext(text: &str) -> ScanOutcome;   // scan_secrets/mask_text — прежние обёртки
// запись: warnings += "secrets_scan_depth" при depth_capped; WARN без текста находок
```

- [ ] **Step 1: Тесты** (FAIL):
  - `scan_depth_capped_warns` — вход подобран генератором и закреплён фикстурой;
  - `ordinary_inputs_le_3_passes`;
  - `secrets_reexport_identical` — `zam_markdown::secrets::mask_text` ≡ `zam_secrets::mask_text` на
    корпусе тестов сканера.
- [ ] **Step 2: Флейк** `focus_inputs_load_survives_timeout_single_read` — барьер или точка
  `test-hooks` вместо времени. 20 прогонов подряд: `1..20 | % { cargo test -p zam-server --test
  code_api $T focus_inputs_load }`, лог — `.tmp/m6/task15b/`, 0 падений.
- [ ] **Step 3: e2e** `user-login` — при 429 ждать `Retry-After` и повторить один раз; лимит не
  трогать.
- [ ] **Step 4: Проверка.**
  - `cargo test -p zam-secrets -p zam-markdown $T`;
  - `cargo test -p zam-server --test secrets $T`;
  - `npm run e2e -- user-login` на release.

  Всё — PASS.
- [ ] **Step 5: Commit** `refactor(secrets): крейт zam-secrets; fix: упор сканера, флейк, e2e входа`.

### Task 15c: Упаковка — `LICENSES/`, `clarify.files` (ответы владельца M5.6 п. 13–14)

**Files.** Modify: `xtask/src/package.rs`, `about.toml`. Create: `deploy/licenses/{tree-sitter-typescript,tree-sitter-java}.LICENSE`.

- [ ] **Step 1: Тест** (FAIL) `client_zip_has_licenses` (`cargo test -p xtask $T package`).
- [ ] **Step 2: Реализация** — в `zam-*.zip` кладётся `LICENSES/` (cargo-about для `zam`);
  `clarify.git` заменяется на `clarify.files` с теми же SHA-256. Проверка: `cargo xtask package
  --platform windows-x64 --flavor cpu --version 0.0.11-m6-dev` без сети — ok; тест — PASS.
- [ ] **Step 3: Commit** `build(package): лицензии в архиве клиента, офлайн-упаковка`.

### Task 16: Долги M5.7

**Files.** Modify: `crates/zam-server/src/api/recall.rs` (лёгкий поиск идей, стоп-основы, doc-ссылки),
`crates/zam-server/src/api/search/mod.rs` (параметры лёгкого поиска), `crates/zam-store/src/ideas.rs`
(`on_entry_restored_tx`), `web/src/components/IdeasView.svelte`. Create: `web/src/lib/ideaButtons.ts`.
Test: `crates/zam-server/tests/{ideas_recall.rs,m57_acceptance.rs}`, `crates/zam-store/tests/ideas.rs`.

**Interfaces — Consumes:** `norm_word`, `StemWord`, `prepared_terms` (9), `localTime` (6), эталон 1a2.
**Produces:**
```rust
pub const IDEA_STOP_STEMS: [&str; 17] = ["сдел", "добав", "исправ", "обнов", "поправ", "почин",
    "модул", "тест", "функци", "файл", "make", "add", "fix", "updat", "chang", "modul", "test"];
fn is_idea_stop(norm: &str) -> bool;  // norm.starts_with(s) && chars(norm) ≤ chars(s) + 3
pub struct LightSearch { pub channel_depth: usize, pub snippets: bool }  // идеи: max(4·top_k, 20), false
```

- [ ] **Step 1: Тесты** (FAIL):
  - `idea_hints_top5_match_m57` — расширен: после правки поиска;
  - `idea_stop_stems_examples` — «добавьте» — служебное; «changelog», «модульный», «address»,
    «makefile» — нет;
  - `idea_stop_verbs_negatives` — ≥ 6 новых, например «добавь тест на модуль авторизации» к
    «Экспорт отчётов в PDF» → без подсказки; пять запросов таблицы M5.7 — прежние ответы;
  - `restored_idea_reconciles_with_task_status` — расширен: `done_at` = времени ревизии `done`
    задачи;
  - `inner_search_ideas_median` (`--ignored`) — 50 идей, 200 вызовов, `ZAM_TIMING m6`.
- [ ] **Step 2: Реализация.** Если топ-5 расходится при урезанной глубине, глубина остаётся 50, а
  снимаются только сниппеты. Проверка:
  - `cargo test -p zam-server --test ideas_recall --test m57_acceptance $T`;
  - `cargo test -p zam-store --test ideas $T`;
  - `$env:RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links"; cargo doc --no-deps -p zam-server $T`
    — чисто.
- [ ] **Step 3: `IdeasView`** — общие кнопки и `localTime` вынесены; `npm run size` — `IdeasView`
  ≤ 7,2 КБ (до возможного значка вопроса 11 M5.7).
- [ ] **Step 4: Замер.**
  - Микрозамер: `cargo test -p zam-server --test ideas_recall $T -- --ignored
    inner_search_ideas_median`. Цель — медиана ≤ 4 мс в debug.
  - Стенд Linux (release): медиана p95 `prompt` из 5 прогонов по 40 вызовов на решение
    вперемешку. Цель прироста — ≤ +7,5 мс, критерий — +15.

  Числа — в журнал.
- [ ] **Step 5: Commit** `perf(ideas): дешёвый поиск подсказок-идей; fix: долги M5.7`.

### Task 17a: Снимок — адаптеры, `take`, маскировка (§8а)

**Files.** Create: `crates/zam-cli/src/cmd/hook/snapshot/{mod.rs,claude.rs,codex.rs}`,
`crates/zam-cli/tests/snapshot.rs`, `crates/zam-cli/tests/fixtures/transcripts/*.jsonl`. Modify:
`crates/zam-cli/Cargo.toml` (`zam-secrets`).

**Interfaces — Consumes:** `zam_core::snapshot::*` (2), `zam_secrets::mask_text` (15b). **Produces:**
```rust
pub enum AgentKind { Claude, Codex }
pub struct Snapshot { pub trigger: SnapTrigger, pub agent: AgentKind, pub status: SnapStatus,
                      pub skipped: u32, pub session_hint: Option<String>, pub data: SnapshotData }
pub trait TranscriptAdapter { fn accept_line(&mut self, line: &str); fn finish(self: Box<Self>) -> SnapshotData; }
pub const PREFILTER: [&str; 7] = ["\"isCompactSummary\"", "\"compact_boundary\"", "\"type\":\"user\"",
    "\"tool_use\"", "\"compacted\"", "\"user_message\"", "\"session_meta\""];
pub fn take(path: &Path, root: &Path, agent: AgentKind, session: &str, tail_kb: u32,
            deadline: Duration) -> Option<Snapshot>;
//   отдельный поток (std::thread), чтение с конца блоками по 64 КиБ, предфильтр, срок — весь ввод-вывод;
//   хук ждёт recv_timeout(deadline); после срока поток брошен, результат — None
pub fn mask_and_limit(s: Snapshot, max_bytes: usize) -> Snapshot;
//   mask_text по полям; запросы 5×500, файлы 20, команды 10×200, конспект ≤ 8 КиБ — хвост/разделы;
//   затем по шагам: команды (старые), запросы (старые), конспект с начала
```

- [ ] **Step 0 (зависит от владельца): живые фикстуры** — по вопросу 2 (§15 «Вопросы редакции 4»).
  - Вариант А: короткие обезличенные фрагменты одного свежего журнала Claude Code и Codex в
    `crates/zam-cli/tests/fixtures/transcripts/<агент>-<версия>-<случай>.jsonl`. Сами журналы не
    копируются.
  - Вариант Б: фрагменты от владельца там же.
  - Сверка форматов §8а — в журнал.

  До шага 0 работают фикстуры `-doc-`, и §17 п. 19 открыт.
- [ ] **Step 1: Тесты** (FAIL):
  - Claude:
    - `claude_summary_record_is_not_compact`;
    - `claude_compact_after_boundary`;
    - `claude_whitelist_drops_inserts` — `<bash-stdout>`, `<local-command-stdout>`,
      `<command-name>`, `<system-reminder>`, картинка, `isSidechain`, `isMeta`, чужой `sessionId`;
  - Codex:
    - `codex_compacted_and_user_message`;
    - `codex_ignores_user_instructions_and_encrypted`;
  - разбор:
    - `broken_and_unknown_lines_skipped`;
    - `huge_line_skipped`;
    - `path_outside_root_dropped`;
    - `prefilter_skips_unrelated_lines`;
  - маскировка и пределы:
    - `secrets_masked_in_prompts_and_flags` — `-pXXX`, `--password X`, `curl -u u:p`, `Bearer`;
      приманки собираются конкатенацией;
    - `limit_order_commands_prompts_summary`;
    - `summary_keeps_tail_sections`.
- [ ] **Step 2: Реализация.** Проверка: `cargo test -p zam-cli --test snapshot $T` — PASS; p95 `zam
  --version` — не хуже прежнего + 10 % (замер `cargo xtask code-bench`, как в 9C).
- [ ] **Step 3: Commit** `feat(snapshot): адаптеры журналов агента и маскировка`.

### Task 17b: Снимок — хуки, очередь, `zam session snapshot`

**Files.** Create: `crates/zam-cli/src/cmd/session.rs` (регистрация в `cmd/mod.rs`, `main.rs`). Modify:
`crates/zam-cli/src/cmd/hook/{mod.rs,checkpoint.rs,session_start.rs,journal.rs,cache.rs}`,
`crates/zam-server/src/agent_setup/mod.rs`. Test: `crates/zam-cli/tests/{hooks.rs,snapshot.rs}`,
`crates/zam-server/tests/agent_setup.rs`.

**Interfaces — Produces:**
- `Kind::Checkpoint` принимает `"Stop" | "PreCompact" | "PostCompact" | "SessionEnd"`, бюджет —
  100 мс.
- Ключи `[hooks]` в `Settings`: `session_snapshot` (true), `snapshot_tail_kb` (2048),
  `snapshot_ms` (25, 5..=50).
- Файлы:
  - `snapst-<сессия>.json` — `{transcript_path, size, at}`;
  - `snapq-<сессия>.json` — очередь, права 0600;
  - оба — в `gc_cache`, через 7 дней.
- `Ev::Snapshot {trigger, status, bytes, prompts, files, cmds}` — только счётчики.
  `render_summary` получает строку «снимок сессии <yyyy-MM-dd HH:mm:ss>, извлечено
  автоматически».
- `SessionEnd` → `PATCH /sessions/{id} {ended: true}`.
- `claude_settings()`: `SessionEnd` и `PostCompact` → `zam hook checkpoint`; `codex_hooks()`:
  `SessionEnd`.
- CLI: `zam session snapshot --transcript <файл> [--format claude|codex] [--session <id>]`.
  - Сессия: `--session` → `ZAM_SESSION_ID` → id из журнала. Нет на сервере —
    `POST /sessions {id, agent}`. Нет ни одного источника — код 1 и «укажите --session».

- [ ] **Step 1: Тесты** (FAIL):
  - бюджет и устойчивость:
    - `snapshot_deadline_never_delays_stop`;
    - `snapshot_tail_of_growing_file`;
    - `snapshot_bad_path_is_silent`;
  - события хуков:
    - `stop_throttle_16kib_60s` — время «60 с назад» задаётся правкой поля `at` в `snapst-*.json`;
    - `post_compact_and_session_end_accepted_within_100ms` (пустой вывод);
    - `session_end_patches_ended`;
    - `compact_put_before_recall`;
    - `catch_up_before_recall_on_startup`;
  - старый сервер и выключатель:
    - `old_server_three_attempts_then_queue`;
    - `switch_off_never_opens_transcript`;
  - что пишется на диск:
    - `queue_file_mode_0600` (unix);
    - `hooks_log_has_no_snapshot_text`;
    - `ev_snapshot_metadata_only`;
  - `zam session snapshot`:
    - `cli_snapshot_session_source_order`;
    - `cli_snapshot_registers_session`;
    - `cli_snapshot_no_session_exit_1`;
  - сервер: `agent_setup_fragments_have_session_end`.
- [ ] **Step 2: Реализация.** Проверка:
  - `cargo test -p zam-cli --test hooks --test snapshot $T`;
  - `cargo test -p zam-server --test agent_setup $T`.

  Всё — PASS.
- [ ] **Step 3: Commit** `feat(hooks): снимок сессии в хуках, очередь, zam session snapshot`.

### Task 17c: Снимок — сервер, показ, WebUI

**Files.** Create: `crates/zam-server/src/api/snapshot.rs`, `crates/zam-server/tests/snapshot.rs`. Modify:
`crates/zam-server/src/api/{mod.rs,recall.rs,sessions.rs}`, `crates/zam-server/src/{maintenance.rs,config.rs}`,
`web/src/components/SessionsView.svelte`, `web/src/routes/AgentSetup.svelte`, `crates/zam-server/src/agent_setup/guide.{ru,en}.md`.

**Interfaces — Produces:**
- `PUT /sessions/{id}/snapshot {taken_at, trigger, agent, status, skipped, data}` → 204.
  - Без явной сессии — 400 `session-required`; сверх `snapshot.max_bytes` — 413.
  - Сервер повторно маскирует и при находке отдаёт `warnings: ["secrets_masked"]`.
- `GET /sessions/{id}/snapshot` → 200 или 404 (чужая сессия, в том числе участник общего решения).
- Ключи `[recall]`: `snapshot_tokens` (400), `snapshot_stale_min` (30), `snapshot_digest_days` (2).
  Ключи `[snapshot]`: `max_bytes` (16384), `keep_days` (14).
- `maintenance` раз в сутки вызывает `gc_snapshots(now − keep_days)`.
- Показ — §8а: условие «давно не обновлялась», живость по `last_seen_at`, признаки «работа
  продолжилась», квоты частей.
- WebUI: блок «Снимок сессии» — текст без `{@html}`. Ключи снимка описаны на странице «Подключение
  агента → Подсказки хуков».

- [ ] **Step 1: Тесты** (FAIL):
  - запись и права:
    - `put_requires_explicit_session`;
    - `foreign_snapshot_404` — другой пользователь и участник общего решения;
    - `put_413_over_limit`;
    - `server_remasks_secrets`;
    - `gc_sessions_and_keep_days_remove`;
  - показ в дайджесте:
    - `digest_shows_last_state_when_task_stale`;
    - `digest_hides_live_session_by_last_seen`;
    - `digest_hides_when_work_continued` — по `memory_writes`, ревизии задачи другой сессией,
      действующему чужому захвату;
    - `recovery_without_summary`;
    - `snapshot_quota_parts`;
    - `snapshot_tokens_zero_hides`;
    - `digest_with_snapshot_deterministic` (`freeze_now`);
  - e2e: `snapshot text is not html` (XSS: `<script>`, `<img onerror>`).
- [ ] **Step 2: Реализация.** Проверка:
  - `cargo test -p zam-server --test snapshot --test recall --test recall_recovery $T` — PASS;
  - `npm run e2e -- sessions` на release — PASS.
- [ ] **Step 3: Commit** `feat(snapshot): хранение, показ «последнего известного состояния», WebUI`.

### Task 17d: Снимок — бюджеты, ТС-2, полный прогон

- [ ] **Step 1: Бюджеты** — `cargo test -p zam-cli --test m4_acceptance --release $T hook_budgets`
  (release-сборка):
  - журнал 50 МиБ: `checkpoint` ≤ 100 мс;
  - `session-start` с догоняющим снимком ≤ 300 мс.

  PASS; числа — в журнал.
- [ ] **Step 2: ТС-2** — `eval/sessions.toml` получает `transcript` и сессии с «обрывом»; пилот
  `cargo xtask eval --sessions --split dev`.
- [ ] **Step 3: Полный прогон** `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast $T`; итог —
  в журнал.
- [ ] **Step 4: Commit** `test(snapshot): бюджеты хуков и ТС-2 со снимком`.

### Task 13a: Блок v7 и протокол (§13)

**Files.** Modify: `crates/zam-core/src/lib.rs` (`AGENT_BLOCK_VERSION = 7`),
`crates/zam-server/src/agent_setup/{templates.rs,guide.ru.md,guide.en.md}`,
`crates/zam-server/src/mcp/{mod.rs,tools.rs}`, `crates/zam-server/tests/agent_setup.rs`.

- [ ] **Step 1: Тесты** (FAIL):
  - `agent_setup_block_v7_under_500_tokens_ru_en` — оценка ≈ 482 ru / 478 en;
  - `block_version_reminder_v6_to_v7` — одно напоминание;
  - `guide_has_focus_rejected_snapshot` — разделы «Фокусное чтение», «Снимок сессии» и правило
    отвергнутых подходов;
  - `MOVED_TO_GUIDE` получает «; `certainty` меняй с `reason`» и «функция — `путь#Тип::метод`».
- [ ] **Step 2: Реализация** — основной вариант §13:
  - строка поиска с `focus`;
  - правило отвергнутых подходов — в `guide` («Запись», `insight` с `kind: decision`) и в
    `description` поля `data.kind` у `memory_log`;
  - `INSTRUCTIONS`;
  - описания `memory_read`/`memory_search` — `focus`, легенда, «уже выдавалось».

  Проверка: `cargo test -p zam-server --test agent_setup --test mcp $T` — PASS; фактические числа —
  в журнал.
- [ ] **Step 3: Commit** `feat(agent): блок инструкции v7 и протокол`.

### Task 13b: Переснятие на holdout, приёмка, совместимость (§17)

**Files.** Create: `crates/zam-server/tests/m6_acceptance.rs`, `crates/zam-cli/tests/m6_acceptance.rs`. Modify:
`xtask/src/smoke.rs`.

**Interfaces — Produces:**
- Тесты приёмки:
  - `acceptance_ts1_vs_b4` и `acceptance_ts2_vs_b56` — `#[ignore]`, читают отчёт из
    `ZAM_M6_REPORT`; без переменной — пропуск с сообщением;
  - `acceptance_honest_budget`, `acceptance_dates_utc`, `acceptance_mcp_session_subagent`;
  - `acceptance_epochs_and_reread`, `acceptance_focus`, `acceptance_digest_deterministic`;
  - `acceptance_compat_matrix`, `acceptance_migration_0025`;
  - `acceptance_hook_budgets` — в том числе p95 `/recall(prompt)` ≤ v0.0.10-m5.7 + 10 % (§17 п. 15:
    оба бинарника вперемешку, 40 вызовов);
  - `acceptance_snapshot` — §17 п. 19, только с живыми фикстурами, иначе `#[ignore]` с причиной;
  - `acceptance_m57_debts`, `acceptance_misc_outputs` (§17 п. 21).
- Совместимость: `compat_old_zam_new_server`, `compat_new_zam_old_server` — бинарник тега
  `v0.0.10-m5.7` из 1a2.

- [ ] **Step 1: Переснятие на финальном отпечатке.**
  - Плечи — B4, B5.6, B5.7 и M6.
  - Совместный подтверждающий прогон на holdout: fp16 CUDA — критерий, int8 CPU — контроль. Не
    прошло — откатить последнее изменённое значение и повторить (§3.5).
  - Все наборы качества: `queries.toml`, `docs-queries.toml`, `kind-queries.toml`,
    `area-queries.toml`, `hints.toml`, `rerank=on`.
  - Отчёт — `eval/results/<дата>-m6-<модель>-tokens.{json,md}`.
- [ ] **Step 2: Приёмка и smoke.**
  - `$env:ZAM_M6_REPORT="eval/results/…json"; cargo test -p zam-server --test m6_acceptance $T --
    --include-ignored`;
  - `cargo test -p zam-cli --test m6_acceptance $T`;
  - release-сборка → `cargo xtask package --platform windows-x64 --flavor cpu --version
    0.0.11-m6` → `cargo xtask smoke --archive … --expect-device cpu`. Ожидается SMOKE OK: шаги
    M5.7 и шаги M6 — фокус, `zam sessions tokens`, `zam session snapshot` на фикстуре;
  - `npm run e2e` на release;
  - стенд Linux (Debian 13, 2 ядра / 2 ГБ) — проверка работоспособности;
  - полный прогон `ZAM_REQUIRE_ML=1`;
  - CI (CI, Edge, CUDA) — зелёный.

  Числа — в журнал.
- [ ] **Step 3: Commit** `test(m6): приёмка, совместимость, итоговый замер`.

### Task 13c: Документы и выпуск `0.0.11-m6` — последняя

**Files.**
- ТЗ → v1.24 — разделы:
  - §7.11, §7.12, §7.13;
  - §8.3, §9, §10;
  - §10.7 — явный пересмотр принципа «метаданные, не содержимое»;
  - §12–§14, §14.2 — блок v7;
  - §16.3, §17–§19, §21, §23.
- Спецификация — статус «перенесено в ТЗ v1.24», с отступлениями.
- `CHANGELOG.md` `[0.0.11-m6]` — совместимость с 0.0.10-m5.7, откат `bak-v24`.
- `README.md`.
- `deploy/README-install.md`:
  - раздел «С 0.0.10-m5.7 на 0.0.11-m6» — 0025 и `bak-v24`, блок v7 и напоминание, `zam init`
    ради `SessionEnd`/`PostCompact`, снимок и выключатель;
  - «Известные ограничения (0.0.11-m6)».
- `docs/journal.md` — «Этап M6 (экономия токенов): итог».
- Журнал этапа — «Итог M6».
- Версия `0.0.11-m6` — `Cargo.toml`, `Cargo.lock`, `web/package.json`, `web/package-lock.json`;
  `deploy/config.example.toml`.

- [ ] **Step 1: Перенос** по перечню. Результат по цели 30 % и по карте кода записывается фактом.
  Решения составителя плана — с пометкой «[решение составителя плана M6]».
- [ ] **Step 2: Проверка** — `cargo build --workspace $T`; `npm run check`, `npm run build`; `cargo
  xtask smoke` повторно, если менялась версия в бинарниках.
- [ ] **Step 3: Commit** `chore(release): 0.0.11-m6`.

---

## Решения составителя плана

Каждое решение записывается в журнал этапа и переносится в ТЗ (задача 13c).

1. **Порядок** — строго последовательный, по таблице (решение контролёра). 1c идёт до 1a2: 1c
   зависит только от 1a1 и создаёт 50 идей для эталона 1a2. Задача 2 идёт до 1b: разметка долгая, а
   схема нужна задачам 3 и 7a. Задача 15b стоит до 17a: снимок маскируется через `zam-secrets`.
2. **Одна миграция `0025_session_epochs.sql`** для всей схемы этапа — один `bak-v24` и один шаг
   отката. До 7a эпоха всегда 0, поведение не меняется.
3. **Два отпечатка и два корпуса** (рецензия, C1). `eval/corpus` (ТС-1) пополняется только в 1c, до
   заморозки. Сущности ТС-2 и идеи лежат в `eval/sessions-corpus`, его ТС-1 не грузит.
4. **ТС-1 ходит в `POST /mcp` напрямую**, синхронным `McpClient` (`ureq`). ТС-2 идёт через процесс
   `zam mcp` с `ZAM_SESSION_ID`.
5. **Бинарники баз** — `git archive` тега и `--target-dir target/tags/<тег>`, release. После сборки
   остаются только бинарники. Не `git worktree`: см. инцидент с junction.
6. **Разбиение** — `split(&[(id, kind)])`: сортировка внутри вида по `blake3(salt ‖ id)`, первые
   ⌈n/3⌉ — holdout. Сетка с `--split holdout` — ошибка.
7. **Статистика** — расширение `xtask/src/eval/metrics.rs` (параметр числа выборок, 10 000), без
   второго модуля.
8. **Явная сессия** — поля `explicit` и `via_mcp` в существующем `RequestSession`. Признак MCP —
   расширение запроса `McpOrigin`, которое ставит только `Rest`. Учёт MCP пишет `tools::call`, а
   вложенные search/read — нет.
9. **Цепочка предков** — `procs::ancestors()`: ToolHelp32 через `windows` на Windows и `/proc` на
   Linux; подмена — фича `test-hooks` новой `zam-cli`.
10. **Дата без `jiff` в `zam-core`/`zam-cli`** — `agent_ts(&str)` из RFC 3339. Замороженные часы —
    `zam_store::time::now()` и `freeze_now` (задача 6).
11. **Совпадение слов** (рецензия, C2):
    - `WordMatch::Stems` — существующий алгоритм идей (`stem`, `norm_word`, `StemWord`),
      перенесённый в `zam-search` без изменения поведения;
    - стоп-слова — в `zam-core::text`, а не зависимость `zam-search` → `zam-markdown`. Иначе
      поисковый крейт тянул бы comrak/syntect ради одного списка;
    - правило идей «заголовок + ⌈k/2⌉» остаётся отдельной функцией идей;
    - snowball — только справочно в xtask.
12. **Стоп-основы идей** — `IDEA_STOP_STEMS` в форме `stem`, правило «основа + не больше 3
    символов».
13. **`zam-secrets`** (рецензия, I3) — сканер выносится без изменения поведения. Иначе клиент `zam`
    ради маскировки снимка тянул бы comrak/syntect/ammonia и рисковал холодным стартом и бюджетами
    хуков. `zam-markdown` реэкспортирует его — прочие пользователи не меняются.
14. **Эталоны «до правки»** — шаг 0 задач 5, 7a, 10, 11, 14. Эталон подсказок-идей M5.7 снимается в
    1a2 бинарником тега.
15. **Упаковка** (ответы M5.6 п. 13–14) — в задаче 15c, а не в выпуске. Тогда 13c только переносит
    документы и версию.
16. **Страница сессий WebUI — новая** (`SessionsView.svelte`, `#/sessions`): в WebUI её нет, хотя
    спецификация пишет о ней как о существующей. `rules_heavy` — в `GET /index/status` (`rules`) и
    плашкой у папки Rules: отдельной страницы Rules тоже нет.
17. **Нейтральные типы снимка** (`SnapStatus`, `SnapTrigger`, `SnapshotData`) — одно место,
    `zam-core::snapshot`. Их используют клиент, хранилище и сервер.
18. **Задачи, зависящие от владельца** (шаг 0 задачи 17a, прогон A/B), этап не блокируют. Работа
    идёт на фикстурах `-doc-`, и `acceptance_snapshot` остаётся `#[ignore]` с причиной.
19. **Снимок 9C для code-bench** — `HEAD` `045ef40` (прогон 4, ТЗ §18.2), справочно `4cf804f`
    (прогон 3).

## Самопроверка

| Спецификация | Задачи |
|---|---|
| §0, §3.5 правила, неинфериорность | 1a1 (статистика), 5, 8c, 9, 10, 11 (сетки), 13b (holdout) |
| §3.1–§3.2 ТС-1, нагеты, заморозка, отпечатки, корпус | 1c, 1b |
| §3.1 ТС-2, идеи, «обрыв» | 1c, 4, 17d |
| §3.3 базы, обнаружение возможностей, v0.0.9 ≡ v0.0.10 | 1a1, 1a2, 13b |
| §3.4 симулированный агент, политика без оракула | 1a1 |
| §4.1–§4.3, §5, `refs_in_text` | 5 |
| §4.4 дата UTC | 6 |
| §6 фокус | 7b |
| §7.1 привязка, `explicit`/`via_mcp`, общий предок Codex | 3 |
| §7.2–§7.3 эпохи, идеи без эпохи, «уже выдавалось», доля повторов | 2, 7a, 8b |
| §8 Rules, индексы, детерминированность, идеи, учёт, `rules_heavy` | 8a, 8b, 8c |
| §8а снимок сессии | 2, 17a–17d |
| §9 пороги, совпадение слов | 9 |
| §10, §11 | 1c (корпус), 10, 11 |
| §12 A/B, `checkpoint_impact`, `holdout` | 12 |
| §12а карта кода, `recv_count`, ранги при 0 | 2, 14 |
| §13 совместимость, блок v7, миграция, версия | 2, 3, 13a, 13b, 13c |
| §14 конфигурация | 5, 7a, 7b, 8a, 9, 10, 11, 12, 14, 17b, 17c |
| §16 15 долги M5.5/M5.6, упаковка | 15a, 15b, 15c |
| §16 16 долги M5.7 | 16 (и 7a, 9) |
| §17 п. 1–21 | 13b (приёмка); по пунктам — задачи выше |

Проверено:
- имена из Interfaces совпадают между задачами: `RequestSession.explicit`/`via_mcp`, `McpOrigin`,
  `SessionTouch.epoch`, `bump_epoch`, `put_snapshot`, `SnapshotRow.bytes`, `SnapshotData`,
  `prepared_terms`, `StemWord`, `WordMatch`, `agent_ts`, `freeze_now`, `localTime`,
  `RefAgg.recv_count`, `IDEA_STOP_STEMS`;
- тесты Review Focus назначены задачам 1a1, 5, 7a, 13b, 17b;
- требования рецензии I7 получили тесты:
  - p95 `/recall(prompt)` — 13b;
  - `hook_budgets` с `checkpoint_impact` и с привязкой — 12, 3;
  - ранги при `recv_count = 0` — 14;
  - `zam session snapshot` — 17b;
  - `echo: true` — 5;
  - доля «уже выдавалось» — 7a, 8b;
  - `rules_heavy` — 8b.
