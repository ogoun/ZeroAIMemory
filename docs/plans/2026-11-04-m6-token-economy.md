# M6. Экономия токенов — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

Редакция 1 (2026-11-04), по спецификации редакции 4.2. Пункты «ждёт ответа владельца» реализуются
по варианту по умолчанию из спецификации, пока ответа нет.

**Goal.** Сократить токены, которые память выдаёт агенту, без потери фактов ответа. Экономия
подтверждается воспроизводимым замером ТС-1 и ТС-2. Попутно закрываются отложенные вопросы M3–M5.7,
исправляются ошибки `session_seen` после compact и MCP без своей сессии, а также делается снимок
сессии из журнала агента (решение владельца baton-1 = А).

**Architecture.**
- **Сначала мерило, потом выдача.** Каркас замера (xtask, MCP-клиент, статистика, загрузчик с
  обнаружением возможностей) и замороженные нагеты появляются **до** любой правки выдачи (задачи
  1a, 1b). Правки выдачи (5–8) проверяются на dev, приёмка — на holdout в последней задаче (13).
- **Одна миграция `0025`** (задача 2) — вся схема этапа:
  - эпохи `session_seen`;
  - `sessions.seen_epoch` и `sessions.holdout`;
  - `tokens_unattributed`;
  - `session_snapshots`;
  - `code_refs.recv_count`.

  Задачи 3, 7, 12, 14 и 17 только пользуются ею.
- **Сессия — явная или никакая** (задача 3). Учёт, отметки и пометки работают только при явной
  сессии: `X-Zam-Session` или привязка `zam mcp`. Неявный путь R13 остаётся без пометок, а его учёт
  идёт в `unattributed`.
- **Ядро агентонезависимо.** Форматы журналов Claude и Codex разбирают адаптеры клиента `zam`
  (задача 17). Сервер принимает нейтральный снимок, хранит его и показывает в `recovery` и
  дайджесте.
- **Принцип владельца «качество важнее токенов».** Умолчания выбираются правилами §3.5 спецификации,
  записанными до прогона. Если цель 30 % не достигнута, это записывается фактом — выдача ради
  цифры не урезается.

**Tech Stack.** Новых зависимостей нет. Rust (rusqlite, axum, serde, jiff, tokio, tokenizers в xtask
— уже в workspace), WebUI Svelte 5. Токенизатор критерия — bge-m3 (`BgeTokenCounter`).

**Spec:** `docs/specs/2026-11-04-m6-token-economy.md`, редакция 4.2 (далее — «спецификация»,
главный документ; ссылки `§N` — на неё). Исследование — `docs/research/2026-11-04-m6-token-economy.md`,
разбор baton — `docs/research/2026-11-04-baton-review.md`, события хуков —
`docs/research/2026-10-28-agent-hook-events.md`. Основа — ТЗ `docs/specs/2026-10-17-zeroaimemory-tz.md`
v1.23. Журнал этапа — `docs/plans/2026-11-04-m6-token-economy-log.md`.

## Global Constraints

- **Окружение** (PowerShell): перед cargo/npm — `. scripts/dev-env.ps1`; `$env:CARGO_INCREMENTAL=0`;
  сборка и тесты — `-j 2 --target-dir target/r1-new`.
  - **Без `cargo clean` и `npm ci`.** Если на H: меньше 100 ГБ — удалить `target/*/incremental`,
    `target/*/tmp` и `.exe`/`.pdb` старше 6 ч из `target/*/debug/deps`.
  - Полный прогон `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast -j 2 --target-dir
    target/r1-new` (около 2 ч) — только в задачах 8, 17 и 13.
  - e2e WebUI (`npm run e2e`) — только на release-сборке сервера (`E2E_SERVER`).
- **Временные файлы**, логи и снимки прогонов — только `.tmp/m6/taskN/` или `target/`; в тестах —
  `env!("CARGO_TARGET_TMPDIR")`. Запрещены: `/tmp`, `%TEMP%`, `AppData\Local\Temp`, scratchpad,
  `std::env::temp_dir()`, `tempfile` без `dir=`. Субагентам правило передаётся явно в брифе.
- **Вне каталога проекта ничего не читать** — в том числе `~/.claude` и `~/.codex`, пока владелец не
  разрешит (задача 17, шаг 0).
- **Тесты с процессами** — без `ZAM_*` и `AGENT_ENV_VARS`. CLI — через
  `crates/zam-cli/tests/common::zam_cmd`, сторож `repo_clean` зелёный. Время — тестовыми часами
  (фича `test-hooks`) или прямым SQL, без реального ожидания. Синхронизация гонок — барьерами и
  точками остановки `test-hooks`, а не `sleep`.
- **Ключи-приманки** в фикстурах и тестах собираются конкатенацией (`["AK","IA", …].concat()`),
  а не литералом: иначе push protection не пропустит push.
- **Сборка зелёная после каждой задачи.** Задача, меняющая общий тип или протокол (`RefAgg`,
  `SessionTouch`, `ReadItem`, `Kind::accepts`), правит все места его использования.
- **Проверки задачи:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j 2
  --target-dir target/r1-new -- -D warnings`, затронутые наборы тестов. WebUI — `npm run
  check|build|size` и `npm run test:build`.
- **Единообразие.**
  - Тексты и комментарии — по-русски (doc ≤ 100 символов), идентификаторы — по-английски.
  - Ошибки: `thiserror` в библиотеках, `ApiError` (problem+json) на сервере, коды выхода CLI по ТЗ
    §13.2.
  - `tracing` без текста записей, запросов и снимков.
  - Новый код повторяет соседний шаблон: сервер — `api/sessions.rs`, `api/recall.rs`; CLI —
    `cmd/hook/*`; xtask — `xtask/src/eval/*`.
- **Значения спецификации (дословно).**
  - Дата в тексте агенту — `yyyy-MM-dd HH:mm:ss` UTC, без `T`/`Z`; JSON — RFC 3339 UTC.
  - Ключи и умолчания (до сеток = до M6) — таблица §14. Новое: `search.budget_tokens` 800,
    `search.brief_min_rel` 0, `search.kind_boost_mode` `always`, `search.w_attach` 1,0,
    `read.focus_leaves` 3, `read.focus_min_tokens` 600, `read.focus_timeout_ms` 80,
    `recall.digest_entries` 5, `recall.digest_code_map_tokens` 0, `recall.snippet_tokens` 60,
    `recall.prompt_min_score_fts` 0,6, `recall.snapshot_tokens` 400, `recall.snapshot_stale_min`
    30, `recall.snapshot_digest_days` 2, `snapshot.max_bytes` 16384, `snapshot.keep_days` 14;
    клиент — `hooks.checkpoint_impact` false, `hooks.holdout` 0, `[code] exclude_defaults` true,
    `hooks.session_snapshot` true, `hooks.snapshot_tail_kb` 2048, `hooks.snapshot_ms` 25 (5..=50).
  - Бюджеты хуков ТЗ §10.3 не меняются: `checkpoint` — 100 мс, `session-start` — 300 мс.
  - Блок v7 — ≤ 500 токенов bge-m3, ru и en, худший случай: GUID
    `d6c5a5d9-c4c7-7f6b-a8b6-d8fd4c9fb6f6`, адрес 78 символов, имя 128 → 40, Rules открыты.
  - Статистика — парный бутстреп, 10 000 выборок, фиксированное зерно. Неинфериорность: нижняя
    граница Δ микро-полноты ≥ −0,02, чистые потери `L ≤ 1`; качество поиска — нижняя граница
    Δ nDCG@10 и Δ recall@5 ≥ −0,01; `reliable_first` = 1,000.
- **Миграция** — пользовательская `0025` (последняя на main — `0024_ideas.sql`, проверено
  2026-11-04), откат — `bak-v24`. Системных миграций нет.
- **Версия** `0.0.11-m6`, CHANGELOG `[0.0.11-m6]`, ТЗ v1.24, `AGENT_BLOCK_VERSION = 7`.
- **Коммиты.**
  - После каждой задачи (или её шага, если это сказано в задаче) — в ветке `m6-token-economy`,
    Conventional Commits по-русски.
  - **Только явными путями:** `git add <пути>; git commit -F .tmp/m6/taskN/msg.txt -- <пути>`,
    никаких `-a` и `.`.
  - Trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Без push, merge и tag до финального ревью.
- **Зависит от владельца, не блокирует остальное:**
  - шаг 0 задачи 17 — живые журналы агента;
  - прогон агентного A/B (задача 12) — вне приёмки;
  - чек-лист на живом агенте: передаётся ли `CLAUDE_CODE_SESSION_ID` в MCP, `transcript_path` у
    Codex, `SessionEnd` при закрытии окна, цепочка процессов Codex.

## Review Focus

1. **Снимок не выносит лишнего и не мешает хуку.** Опасные входы — журнал 50 МиБ на HDD, строка
   10 МиБ, журнал, который пишется в момент чтения, `transcript_path` на каталог или на файл без
   прав.
   - **Ожидание:** хук укладывается в бюджет, `decision: block` не задерживается, снимок `partial`
     или его нет, ошибки в stdout нет.
   - **Тесты** (задача 17): `snapshot_deadline_never_delays_stop`,
     `snapshot_tail_of_growing_file`, `snapshot_bad_path_is_silent`.
2. **Эпоха не открывает старое и не прячет нужное.** Сценарий: `PreCompact` пришёл без
   `SessionStart compact`, или наоборот; две эпохи подряд; субагент на общем MCP.
   - **Ожидание:** после любой новой эпохи `(seen)` не прячет выданное до неё; идеи остаются
     показанными; MCP не теряет фрагментов.
   - **Тесты** (задача 7): `epoch_bump_without_session_start`, `double_epoch_bump`,
     `ideas_seen_across_epochs`.
3. **Честный бюджет на краях.** Бюджет 1 и 100; карточка, у которой одна строка длиннее
   бюджета; многобайтовый заголовок, обрезанный посреди символа.
   - **Ожидание:** первая карточка отдаётся всегда, фрагмент режется по границе символа, итог ≤
     бюджета, кроме первой строки, которая не делится.
   - **Тесты** (задача 5): `brief_budget_edges_first_card_only`, `snippet_cut_on_char_boundary`.
4. **Старый клиент и старый сервер.** `zam` 0.0.10-m5.7 против сервера M6 и наоборот, включая
   `SessionEnd`/`PostCompact`, `new_epoch`/`holdout` и `recv_count`.
   - **Ожидание:** без 400 на основной регистрации, без неизвестных полей у старого сервера, пустой
     ответ старого `zam` на новые события.
   - **Тесты** (задача 13): `compat_old_zam_new_server`, `compat_new_zam_old_server` (smoke обоих
     направлений с бинарником тега `v0.0.10-m5.7`).
5. **Замер нельзя «подкрутить».** Нагеты после заморозки изменены; сетка запущена на holdout;
   B5.6 и M6 сняты на разных отпечатках корпуса.
   - **Ожидание:** `eval` падает с понятной ошибкой: несовпадение `nuggets_fingerprint`,
     `--split holdout` вместе с `--grid`, разные отпечатки плеч.
   - **Тесты** (задача 1a): `eval_rejects_fingerprint_mismatch`, `grid_refuses_holdout`,
     `arms_must_share_fingerprint`.

---

## Карта файлов

```
xtask/src/eval/{mcp_client.rs,caps.rs,stats.rs,split.rs,sessions.rs,nuggets.rs (новые)}
xtask/src/eval/{tokens.rs,run.rs,report.rs,hints.rs,grid.rs,metrics.rs}, xtask/src/{eval.rs,main.rs}
xtask/src/ab.rs (новый), xtask/src/{code_bench.rs,smoke.rs,package.rs}
eval/{scenarios.toml,sessions.toml (новый),kind-queries.toml (новый),area-queries.toml (новый),hints.toml,
      nuggets.lock (новый),README.md}, eval/ab/{tasks.toml,runs.toml} (новые), eval/corpus/** (области, идеи)
crates/zam-store/migrations/user/0025_session_epochs.sql
crates/zam-store/src/{sessions.rs,usage.rs,code.rs,ideas.rs,snapshots.rs (новый),user_db.rs,lib.rs}
crates/zam-core/src/{lib.rs,time.rs}          (AGENT_BLOCK_VERSION 7; fmt_agent_ts)
crates/zam-code/src/{proto.rs,rank.rs,extract/*}
crates/zam-search/src/recall.rs               (правило совпадения слов)
crates/zam-markdown/src/secrets.rs            (счётчик повторов)
crates/zam-server/src/{config.rs,maintenance.rs}, deploy/config.example.toml
crates/zam-server/src/api/{sessions.rs,read.rs,recall.rs,snapshot.rs (новый),code.rs,previews.rs,reports.rs}
crates/zam-server/src/api/search/{mod.rs,cache_step.rs}
crates/zam-server/src/mcp/{mod.rs,tools.rs}, crates/zam-server/src/agent_setup/{mod.rs,templates.rs,guide.ru.md,guide.en.md}
crates/zam-server/tests/{sessions.rs,session_tokens.rs,read_focus.rs (новый),recall.rs,ideas_recall.rs,
      snapshot.rs (новый),agent_setup.rs,mcp.rs,code_api.rs,m6_acceptance.rs (новый)}
crates/zam-cli/src/cmd/{mcp.rs,read.rs,sessions.rs,session.rs (новый),report.rs}
crates/zam-cli/src/cmd/hook/{mod.rs,checkpoint.rs,session_start.rs,journal.rs,cache.rs,binding.rs (новый),
      snapshot/{mod.rs,claude.rs,codex.rs} (новые)}
crates/zam-cli/tests/{hooks.rs,mcp_bind.rs (новый),snapshot.rs (новый),m4_acceptance.rs,m6_acceptance.rs (новый)}
crates/zam-cli/tests/fixtures/transcripts/*.jsonl (новые)
web/src/components/{ReportView,IdeasView,SessionsView (новый)}.svelte, web/src/routes/{Shell,IndexStatus}.svelte,
  web/src/lib/{api.ts,router.ts,time.ts,print.ts,ideaButtons.ts (новый)}
web/e2e/{reports.spec.ts,user-login.spec.ts,sessions.spec.ts,ideas.spec.ts}, web/.size-limit.json
deploy/licenses/*, about.toml
```

Порядок выполнения (по зависимостям): **1a → 2 → 1b → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 →
14 → 15 → 16 → 17 → 13**. Задачи 10, 11, 14, 15 от выдачи не зависят и могут идти параллельно с
5–8, если исполнителей несколько. Задача 13 — последняя.

| № | Задача | Зависит от | Объём |
|---|---|---|---|
| 1a | Каркас замера и статистика | — | L |
| 2 | Схема `0025` | — | M |
| 1b | Набор ТС-1, нагеты, заморозка | 1a | L (разметка) |
| 3 | Привязка MCP к сессии | 2 | M |
| 4 | Сессионный набор ТС-2 | 1a, 3 | L |
| 5 | Карточки v2, честный бюджет | 1b | M |
| 6 | Дата UTC в тексте агенту | 5 (общий рендер) | S–M |
| 7 | Эпохи, «уже выдавалось», фокус | 2, 3, 1b | L |
| 8 | Дайджест, восстановление, `brief`, учёт | 4, 5 | L |
| 9 | Пороги подсказок | 1a; сетки после 5, перепроверка после 7 | M |
| 10 | Буст вида | — | M |
| 11 | Области, `boost_area`, `w_attach` | 10 | M |
| 12 | Стенд A/B, `checkpoint_impact`, `holdout` | 2, 8 | M (+ прогон владельца) |
| 14 | Ранжирование карты кода | 2 | M |
| 15 | Долги M5.5 и M5.6 | 6 (даты WebUI) | M |
| 16 | Долги M5.7 | 7, 9 | M |
| 17 | Снимок сессии | 2, 3, 8; шаг 0 — владелец | L |
| 13 | Инструкция, подтверждение, выпуск | все | L |

---

### Task 1a: Каркас замера и статистика (§3, §16 1a)

**Files.**
- Create: `xtask/src/eval/{mcp_client.rs,caps.rs,stats.rs,split.rs,nuggets.rs}`.
- Modify: `xtask/src/eval/{tokens.rs,run.rs,report.rs}`, `xtask/src/{eval.rs,main.rs}`,
  `eval/README.md`.
- Test: модульные тесты в этих файлах и `xtask/tests/eval_cli.rs` (новый).

**Interfaces — Produces:**
```rust
// mcp_client.rs — JSON-RPC поверх POST /mcp (streamable HTTP), сессия явная
pub struct McpClient { /* base, token, solution, session */ }
impl McpClient {
    pub async fn connect(base: &str, token: &str, solution: &str, session: Option<&str>) -> Result<Self>;
    pub async fn call(&self, tool: &str, args: serde_json::Value) -> Result<String>; // текст result.content
}
// caps.rs — обнаружение возможностей (I8)
pub struct ServerCaps { pub version: semver::Version, pub has_focus: bool, pub has_ideas: bool,
                        pub has_snapshot: bool, pub config_keys: BTreeSet<String> }
pub async fn probe(base: &str) -> Result<ServerCaps>;       // GET /health → version, таблица версий
pub fn filter_overrides(caps: &ServerCaps, env: &[(String, String)]) -> Vec<(String, String)>;
// stats.rs
pub fn micro_recall(found: &[(usize, usize)]) -> f64;        // Σ найденных / Σ нагетов
pub fn net_losses(base: &[bool], arm: &[bool]) -> i64;       // L = #(1→0) − #(0→1)
pub struct Ci { pub est: f64, pub lo: f64, pub hi: f64 }
pub fn ratio_ci(pairs: &[(f64, f64)], seed: u64, n: usize) -> Ci;      // Σ arm / Σ base
pub fn delta_recall_ci(pairs: &[(f64, f64, usize)], seed: u64, n: usize) -> Ci; // веса — число нагетов
// split.rs
pub const SPLIT_SALT: &str = "m6-2026-11-04";
pub fn is_holdout(id: &str) -> bool;                         // blake3(salt‖id) — ⅓ holdout, расслоение по kind
// nuggets.rs
pub fn fingerprint(scenarios: &Path, corpus: &Path) -> String; // blake3 нагетов + корпуса
```
CLI xtask: `cargo xtask eval --tokens --via mcp [--server-bin P] [--zam-bin P] [--split
dev|holdout] [--out eval/results/<дата>-m6-<модель>-tokens]`, `--check` (проверки §3.2).

- [ ] **Step 1: Тесты статистики.** `ratio_ci_identity_is_one` (одинаковые плечи → est = 1,0,
  `lo ≤ 1 ≤ hi`), `ratio_ci_seed_reproducible`, `net_losses_counts_both_ways` (база `[1,1,0]`,
  плечо `[0,1,1]` → 0), `holdout_share_is_one_third_per_kind` (на 300 синтетических id доля 0,30–0,37
  в каждом виде), `grid_refuses_holdout`, `eval_rejects_fingerprint_mismatch`,
  `arms_must_share_fingerprint`. Запуск: `cargo test -p xtask -j 2 --target-dir target/r1-new
  eval::` — FAIL.
- [ ] **Step 2: Реализация** `stats.rs`, `split.rs`, `nuggets.rs`, `caps.rs`, `mcp_client.rs`;
  политика §3.4 в `tokens.rs`: `READ_BUDGET` 4000; дочитывание один раз по триггерам `…ещё N
  токенов`, `(budget)`, `[пропущено: <путь> · ~N t]` — сначала `section` из пометки, иначе L2 с
  бюджетом 4000. В JSON отчёта — ответы целиком. Тесты из шага 1 — PASS.
- [ ] **Step 3: Пилот на текущих 20 сценариях.** Прогон B4 (`v0.0.5-m4`), B5.6 (`v0.0.9-m5.6`) и
  B5.7 (`v0.0.10-m5.7`) — бинарники тегов собираются в `target/r1-new/tags/<тег>`. Контроль «v0.0.9
  ≡ v0.0.10-m5.7»: побайтовое сравнение ответов `queries.toml`, `docs-queries.toml` и ТС-1.
  Расхождение — в журнал (тогда база качества — v0.0.10-m5.7, шапка спецификации). Отчёт —
  `.tmp/m6/task1a/pilot.{json,md}`, числа — в журнал.
- [ ] **Step 4: Commit** `feat(eval): каркас замера M6 — MCP, статистика, разбиение`.

### Task 2: Схема `0025` (§13, §16 2)

**Files.**
- Create: `crates/zam-store/migrations/user/0025_session_epochs.sql`,
  `crates/zam-store/src/snapshots.rs`.
- Modify: `crates/zam-store/src/{user_db.rs,sessions.rs,usage.rs,code.rs,lib.rs}`.
- Test: `crates/zam-store/tests/{migrate_rebuild.rs,sessions.rs,snapshots.rs (новый)}`.

**Interfaces — Produces:**
```sql
-- 0025 (rebuild: true): session_seen пересобирается
CREATE TABLE session_seen (… , epoch INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, owner_id, target_kind, target_id, part, epoch));   -- target_kind с 'blob'
ALTER TABLE sessions ADD seen_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD holdout INTEGER NOT NULL DEFAULT 0;
CREATE TABLE tokens_unattributed (day TEXT NOT NULL, event TEXT NOT NULL, calls INTEGER NOT NULL,
  tokens INTEGER NOT NULL, PRIMARY KEY (day, event)) WITHOUT ROWID;
CREATE TABLE session_snapshots (session_id TEXT PRIMARY KEY REFERENCES sessions(id),
  taken_at TEXT NOT NULL, trigger TEXT NOT NULL, agent TEXT, status TEXT NOT NULL
  CHECK (status IN ('complete','partial')), skipped INTEGER NOT NULL, bytes INTEGER NOT NULL,
  data TEXT NOT NULL);
ALTER TABLE code_refs ADD recv_count INTEGER NOT NULL DEFAULT 0;
```
```rust
// sessions.rs
pub async fn bump_epoch(db: &Db, session: &str) -> Result<u32, StoreError>;   // seen_epoch += 1
pub async fn set_holdout(db: &Db, session: &str, on: bool) -> Result<(), StoreError>;
pub async fn session_epoch(db: &Db, session: &str) -> Result<u32, StoreError>;
// mark_seen / seen_targets получают epoch: u32; отбор «выдано» — только epoch = текущей
// seen_entry_ids и any_idea — БЕЗ фильтра по эпохе (идеи, §7.2)
// gc_sessions: явный список таблиц ["session_seen", "session_tokens", "session_snapshots"]
// usage.rs
pub async fn add_unattributed(db: &Db, day: jiff::civil::Date, event: &str, calls: u32, tokens: u64)
    -> Result<(), StoreError>;                                // UPSERT с прибавлением
// snapshots.rs
pub struct SnapshotRow { pub session_id: String, pub taken_at: Timestamp, pub trigger: String,
    pub agent: Option<String>, pub status: SnapStatus, pub skipped: u32, pub data: serde_json::Value }
pub async fn put_snapshot(db: &Db, row: &SnapshotRow) -> Result<(), StoreError>;   // перезапись
pub async fn get_snapshot(db: &Db, session: &str) -> Result<Option<SnapshotRow>, StoreError>;
pub async fn gc_snapshots(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError>;
```

- [ ] **Step 1: Фикстура базы 0024** — в тесте `migrate_0025_from_m57_db`: база 0023 по образцу
  `acceptance_migration_from_m56_db` (M5.7) → миграция до 0024 → идея, вопрос-предложение, задача из
  «начать», отметка `session_seen` идеи, отчёт с картинкой, карта кода с `code_refs`. Сборка вне
  `users/`, на место — жёсткой ссылкой закрытой базы (WAL сброшен, `journal_mode = DELETE`).
- [ ] **Step 2: Тест миграции** (FAIL): строки `session_seen` в `epoch = 0`; объекты
  `sqlite_master`, кроме ожидаемых, совпадают; пять триггеров отчёта срабатывают прямым SQL;
  `foreign_key_check` пуст; `integrity_check` ok; `bak-v24` есть; `recv_count = 0`; идея «показана»
  в своей сессии (`seen_entry_ids`).
- [ ] **Step 3: Тесты функций** (FAIL): `bump_epoch_hides_old_marks`,
  `seen_ideas_ignore_epoch`, `unattributed_upsert_adds`, `snapshot_put_overwrites`,
  `gc_sessions_drops_snapshots`, `gc_snapshots_by_cutoff`.
- [ ] **Step 4: Реализация** миграции и функций; правка вызовов `mark_seen`/`seen_*` в workspace
  (эпоха из `session_epoch`, до задачи 7 — всегда 0). Тесты `zam-store` и `zam-server --test
  sessions` — PASS.
- [ ] **Step 5: Commit** `feat(store): миграция 0025 — эпохи, holdout, учёт, снимки, recv_count`.

### Task 1b: Набор ТС-1, нагеты, заморозка (§3.1–§3.2, §16 1b)

**Files.** Modify: `eval/scenarios.toml`, `eval/corpus/**` (по необходимости), `xtask/src/eval/nuggets.rs`,
`eval/README.md`. Create: `eval/nuggets.lock`.

**Interfaces — Consumes:** `fingerprint`, `--check` (1a). **Produces:** `eval/nuggets.lock` (`{fingerprint,
scenarios, nuggets}`), которым задачи 5–13 проверяют неизменность набора.

- [ ] **Step 1: Проверки `--check`** (тесты FAIL на синтетике): `nugget_occurs_once` (с
  нормализацией пробелов, как у `evidence`), `nugget_not_in_l0`, `every_expect_has_nugget`,
  `low_overlap_share_at_least_40pct` (доля общих основ snowball ru/en без стоп-слов ≤ 0,2).
- [ ] **Step 2: Набор** ≥ 45 сценариев: ≥ 8 длинных заметок, ≥ 6 insight, ≥ 4 вложения
  (`eval/docs`), ≥ 6 с ≥ 3 источниками, ≥ 4 с одним. Нагеты — по 1–4 на источник, 20–200 символов.
  `cargo xtask eval --check` — зелёный.
- [ ] **Step 3: Независимая разметка** ≥ 20 % сценариев (по зерну) отдельным субагентом: ему даются
  корпус и вопросы, но не код M6 и не результаты. Согласие пишется в журнал; если < 0,7 —
  расхождения разбираются до заморозки.
- [ ] **Step 4: Заморозка — отдельный коммит** `test(eval): заморозка нагетов ТС-1`:
  `eval/nuggets.lock`, `eval/scenarios.toml`. Дальнейшие правки — только при доказанной ошибке
  разметки, с записью в журнал и переснятием плеч.

### Task 3: Привязка MCP к сессии (§7.1, §16 3)

**Files.**
- Create: `crates/zam-cli/src/cmd/hook/binding.rs`, `crates/zam-cli/tests/mcp_bind.rs`.
- Modify: `crates/zam-cli/src/cmd/{mcp.rs,hook/session_start.rs,hook/cache.rs}`,
  `crates/zam-server/src/mcp/mod.rs` (`implicit_session`, около стр. 266),
  `crates/zam-server/src/api/{sessions.rs,search/mod.rs,read.rs}`.
- Test: `crates/zam-server/tests/{mcp.rs,session_tokens.rs}`.

**Interfaces — Produces:**
```rust
// binding.rs
pub const BINDING_ANCESTORS_MAX: usize = 4;
pub fn write_binding(root: &Path, session: &str) -> io::Result<()>;   // agent-<pid>.json на предков
pub fn read_binding(root: &Path) -> Option<String>;                   // первый по цепочке предков
// сервер: контекст сессии запроса
pub struct SessionCtx { pub id: String, pub explicit: bool }          // explicit: заголовок или привязка
// учёт: explicit → session_tokens; иначе → add_unattributed(day, event)
// GET /sessions/unattributed?days=N → {items: [{day, event, calls, tokens}]}
```
`zam mcp` ставит `X-Zam-Session` по порядку: `ZAM_SESSION_ID` → `CLAUDE_CODE_SESSION_ID` →
`read_binding` (при `initialize` и раз в 30 с).

- [ ] **Step 1: Тесты** (FAIL): `binding_written_on_session_start_and_clear`,
  `binding_found_via_ancestor_chain` (фикстура дерева процессов: функция предков подменяется через
  `test-hooks`), `mcp_explicit_session_marks_seen`, `implicit_session_no_marks_goes_unattributed`,
  `subagent_on_shared_mcp_keeps_snippet` (карточка с фрагментом и ` (выдавалось)`;
  `skip_seen: true` прячет).
- [ ] **Step 2: Реализация** по §7.1; DEBUG-признак `implicit` в журнале сервера. Тесты — PASS.
- [ ] **Step 3: Commit** `feat(mcp): привязка zam mcp к сессии агента, учёт без сессии`.

### Task 4: Сессионный набор ТС-2 (§3.1, §3.3, §16 4)

**Files.** Create: `eval/sessions.toml`, `xtask/src/eval/sessions.rs`. Modify: `xtask/src/eval/{run.rs,report.rs}`,
`eval/corpus/**` (Rules, задачи, ответы, идеи), `eval/README.md`.

**Interfaces — Consumes:** `McpClient`, `ServerCaps`, статистика (1a); привязка (3). **Produces:**
`cargo xtask eval --sessions [--profile standard|heavy] [--ideas on|off] [--base B5.6|B5.7|B4]` →
`R` по событиям, полнота обязательных строк, `hint_recall` (общие подсказки и отдельно
подсказки-идеи), доля повторной выдачи, совпадение дайджеста при двух стартах, строки «идеи» и
«снимок сессии».

- [ ] **Step 1: Набор** — ≥ 12 сессий: стандартный профиль ≥ 8, тяжёлый ≥ 4. Состав событий — §3.1;
  идеи — по §3.1; фикстуры `transcript_path` для снимка добавляет задача 17 (до неё поле пустое).
  Хуки — бинарником `zam hook <событие>` (stdin, как у Claude Code), MCP — через `zam mcp` с
  привязкой и без.
- [ ] **Step 2: Тесты раннера** (FAIL на мок-сервере): `required_lines_detected`,
  `ideas_excluded_from_b56_hint_recall`, `idea_hints_compared_to_b57`,
  `events_missing_in_b4_excluded_both_arms`.
- [ ] **Step 3: Реализация** и пилот на B5.6, B5.7 и B4 (пересечение событий). Числа — в журнал.
- [ ] **Step 4: Commit** `feat(eval): сессионный набор ТС-2`.

### Task 5: Карточки v2 и честный бюджет (§4.1–§4.3, §5, §16 5)

**Files.** Modify: `crates/zam-server/src/api/search/{mod.rs,cache_step.rs}` (`pack_items` около
стр. 1406, рендер карточек около стр. 1688/1740/2781), `crates/zam-server/src/api/recall.rs`
(`SNIPPET_TOKENS`, строки подсказок), `crates/zam-server/src/api/cache*.rs` (`echo`),
`crates/zam-server/src/mcp/tools.rs`, `crates/zam-server/src/config.rs`,
`deploy/config.example.toml`, `crates/zam-server/src/agent_setup/guide.{ru,en}.md` (п. 1
«Карточки», строка 17 — без «N дн. назад»). Test: `crates/zam-server/tests/{search.rs,cache_api.rs,mcp.rs}`.

**Interfaces — Produces:**
```rust
// config.rs, [search]: budget_tokens: usize (1..=200000, 800), brief_min_rel: f32 (0..=1, 0)
// [recall]: snippet_tokens: usize (0..=200, 60)
fn pack_items(items: Vec<Item>, mode: SearchMode, budget_tokens: usize, counter: &dyn TokenCounter)
    -> Vec<Packed>;     // стоимость = вся строка (номер, id, путь, секция, хвост, stale, via, фрагмент)
fn card_tail(score: f32, rev: u32, updated: Timestamp, tokens: usize, cert: Option<Certainty>,
             status: Option<EntryStatus>) -> String;   // "[.82] r7 · 2026-10-10 14:03:22 · 340t"
// POST /cache: тело {…, echo?: bool}; без echo — ответ без summary
```

- [ ] **Step 1: Тесты** (FAIL): `brief_markdown_within_budget` (бюджеты 100, 400, 800, 2000 —
  счётчиком сервера), `brief_budget_edges_first_card_only`, `snippet_cut_on_char_boundary`,
  `card_v2_tail_format` (строка из §4.1 дословно), `brief_min_rel_drops_weak`,
  `cache_put_without_echo_has_no_summary`, `mcp_cache_put_one_line_le_80_tokens`,
  `mcp_update_entry_one_line_le_80_tokens`, `recall_hint_line_v2` (`ZeroAIMemory: <заголовок>
  [<вид>·<cert>] i:<id>`).
- [ ] **Step 2: Реализация.** Константа `DEFAULT_BUDGET_TOKENS` удаляется; легенда хвоста — в
  описание `memory_search` и `zam guide`. Тесты — PASS; `queries.toml` без регрессии (`cargo xtask
  eval`).
- [ ] **Step 3: Сетки на dev ТС-1** — §3.5 п. 1–2: `search.budget_tokens` {400, 600, 800} ×
  `brief_min_rel` {0; 0,4; 0,5}, затем `recall.snippet_tokens` {40, 60}. Правило выбора — §3.5.
  Числа и выбор — в журнал, умолчания — в `config.rs`.
- [ ] **Step 4: Commit** `feat(search): карточки v2 и честный бюджет`.

### Task 6: Дата UTC в тексте агенту (§4.4, §16 6)

**Files.** Create: `crates/zam-core/src/time.rs`, `web/src/lib/time.ts` (если ещё нет общей
функции). Modify: все места выдачи из инвентаризации (`api/search/*`, `api/recall.rs`,
`api/code.rs`, `mcp/tools.rs`, `zam-cli/src/cmd/{history,log,cache,sessions,code}.rs`).
Test: `crates/zam-server/tests/render.rs`, `crates/zam-cli/tests/*`.

**Interfaces — Produces:**
```rust
pub fn fmt_agent_ts(t: jiff::Timestamp) -> String;     // "yyyy-MM-dd HH:mm:ss", UTC, без T/Z
pub fn fmt_agent_date(t: jiff::Timestamp) -> String;   // "yyyy-MM-dd"
```
```ts
export function localTime(iso: string): string;        // RFC 3339 UTC → локальное время браузера
```

- [ ] **Step 1: Инвентаризация** — `rg -n 'strftime|%Y-%m-%d|to_rfc3339|format!\(".*T' crates`.
  Перечень — в журнал.
- [ ] **Step 2: Тест-ловушка** (FAIL): `markdown_has_no_iso_t` — Markdown MCP, `additionalContext`
  хуков и текстовый вывод `zam` на сценариях ТС-1 и ТС-2 не содержат `\d{4}-\d{2}-\d{2}T`.
  `json_dates_unchanged` — JSON API по-прежнему отдаёт RFC 3339. Аудит `utc_storage_audit`: все
  `*_at` в SQLite оканчиваются на `Z`.
- [ ] **Step 3: Реализация** — единая функция во всех местах; WebUI показывает локальное время.
  Тесты — PASS.
- [ ] **Step 4: Commit** `feat(render): дата UTC yyyy-MM-dd HH:mm:ss в тексте агенту`.

### Task 7: Эпохи, «уже выдавалось», явный фокус (§6, §7.2–§7.3, §16 7)

**Files.** Modify: `crates/zam-server/src/api/{sessions.rs,read.rs,recall.rs}`,
`crates/zam-server/src/api/search/mod.rs`, `crates/zam-server/src/mcp/tools.rs`,
`crates/zam-cli/src/cmd/{read.rs,hook/checkpoint.rs,hook/session_start.rs}`,
`crates/zam-server/src/config.rs`. Create: `crates/zam-server/src/api/read/focus.rs` (если `read.rs`
— файл, то модуль `read_focus.rs`). Test: `crates/zam-server/tests/{read_focus.rs (новый),
sessions.rs,recall.rs,ideas_recall.rs}`, `crates/zam-cli/tests/hooks.rs`.

**Interfaces — Consumes:** `bump_epoch`, `session_epoch`, `SessionCtx`. **Produces:**
```rust
// POST /sessions/{…}: отдельный запрос {id, new_epoch: true} → 200 {epoch}
// эпоху увеличивают: session_start source=compact|resume|clear, event recovery, PreCompact, явный new_epoch
pub struct ReadItem { …, pub focus: Option<String> }    // + tier=focus требует focus
// warnings: "focus skipped: short", "focus lexical"; 400 invalid: "нужен текст focus", focus+section|locator|since_rev
// /read: seen: true и заголовок "· уже выдавалось в этой сессии" при той же ревизии и эпохе
// [read]: focus_leaves 1..=10 (3), focus_min_tokens 0..=20000 (600), focus_timeout_ms 1..=10000 (80)
```
CLI: `zam read <id> --focus "<запрос>"`.

- [ ] **Step 1: Тесты эпох** (FAIL): `compact_resets_seen` (сценарий §17 п. 7 по REST и хукам),
  `epoch_bump_without_session_start`, `double_epoch_bump`, `ideas_seen_across_epochs`,
  `checkpoint_precompact_new_epoch_within_100ms`, `old_server_new_epoch_400_silent`.
- [ ] **Step 2: Тесты «уже выдавалось» и фокуса** (FAIL): `reread_full_text_with_mark`,
  `read_without_focus_byte_equal_l2`, `focus_toc_le_150_tokens`, `focus_skip_marker_format`
  (`[пропущено: <путь заголовков> · ~N t]`), `focus_short_target_skipped`,
  `focus_timeout_lexical_fallback` (задержка — точкой `test-hooks`), `focus_attachment_locator`
  (`d:`/`b:` → `locator_text`), `focus_marks_only_leaves_seen`, `focus_p95` (`--ignored`, CPU int8
  ≤ 120 мс, GPU ≤ 50 мс на 5 целях).
- [ ] **Step 3: Реализация** по §6–§7.3. Тесты — PASS. Справочная политика «явный фокус» — в
  отчёте `eval --tokens`.
- [ ] **Step 4: Commit** `feat(read): эпохи seen, пометка «уже выдавалось», явный фокус`.

### Task 8: Дайджест, восстановление, `brief`, учёт (§8, §16 8)

**Files.** Modify: `crates/zam-server/src/api/recall.rs` (`digest` около стр. 1650, `arrange` около
стр. 3006, `digest_ideas`), `crates/zam-server/src/api/index.rs` (`rules_heavy` в `index/status`),
`crates/zam-server/src/config.rs`, `crates/zam-cli/src/cmd/sessions.rs`; WebUI — **новая** страница
сессий `web/src/components/SessionsView.svelte` (её в WebUI ещё нет; маршрут `#/sessions` в
`router.ts`, пункт в `Shell.svelte`, `GET /sessions` и `GET /sessions/{id}` в `api.ts`),
предупреждение `rules_heavy` — в `routes/IndexStatus.svelte` и плашкой у папки Rules в `Tree.svelte`. Test: `crates/zam-server/tests/{recall.rs,
recall_recovery.rs,recall_brief.rs,session_tokens.rs}`, `crates/zam-cli/tests/sessions.rs`,
`web/e2e/sessions.spec.ts`.

**Interfaces — Produces:**
```rust
// [recall]: digest_tokens 1..=20000 (1500, без Rules), recovery_tokens (2000), brief_tokens (800),
//           digest_entries 1..=20 (5), digest_code_map_tokens 0..=1000 (0)
// Rules: до RULES_MAX (20), L1 ≤ RULE_L1_TOKENS (200), сверх digest_tokens и recovery_tokens
// index/status: warnings += "rules_heavy" (> 10 Rules или Σ L1 > 1500)
// session_tokens события: search, read, mcp
// zam sessions tokens [<id> | --last N] [--by-holdout]
```

- [ ] **Step 1: Тесты** (FAIL): `rules_reserved_never_cut` (`digest_tokens` 100 — все Rules),
  `rules_heavy_warning`, `digest_deterministic_two_sessions` (замороженные часы; разные сессии, те
  же сутки UTC — побайтово равный `text`), `digest_order_ignores_recall_shown`,
  `ideas_section_not_cut_by_budget`, `no_ideas_in_recovery_brief_focus`,
  `changelog_index_line_format` (`c:<id> «заголовок» · Nt`), `session_tokens_search_read_mcp`,
  `cli_sessions_tokens_table`; e2e — страница сессий показывает «событие · вызовов · токенов» и долю
  «уже выдавалось».
- [ ] **Step 2: Реализация** по §8. Тесты — PASS.
- [ ] **Step 3: Сетки на dev ТС-2** — §3.5 п. 3–5 (таблица §8). Условие — полнота обязательных
  строк = 1,0. Выбор — в журнал и `config.rs`.
- [ ] **Step 4: Полный прогон** `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast -j 2
  --target-dir target/r1-new`. Итог — в журнал.
- [ ] **Step 5: Commit** `feat(recall): Rules-резерв, компактный детерминированный дайджест, учёт`.

### Task 9: Пороги подсказок (§9, §16 9)

**Files.** Modify: `eval/hints.toml`, `xtask/src/eval/hints.rs`, `crates/zam-search/src/recall.rs`
(`term_coverage`, `coverage_enough`, около стр. 192–201), `crates/zam-server/src/api/recall.rs`
(подсказки-идеи переходят на общий код), `crates/zam-server/src/config.rs`.
Test: `crates/zam-search` юнит, `crates/zam-server/tests/ideas_recall.rs`.

**Interfaces — Produces:**
```rust
pub enum WordMatch { Plain, Stems }                 // Stems: snowball ru/en, стоп-слова, ё→е
pub struct CoverageRule { pub divisor: u32, pub cap: u32, pub word_match: WordMatch }
pub fn coverage_enough(query_words: &[String], text: &str, rule: &CoverageRule) -> bool;
pub fn prepared_terms(text: &str, m: WordMatch) -> Vec<String>;   // общий для post_tool и идей
// [recall] prompt_min_score_fts 0..=1 (0,6); post_tool_coverage {divisor, cap, word_match}
```

- [ ] **Step 1: Набор** `hints.toml` ≥ 120 случаев, ≥ 48 `post_tool` (половина нерелевантных),
  корзины длин ≤ 3, 4–8, 9–20, > 20 слов — по ≥ 10. Классы ошибок — сборка, тесты, git,
  кодировки, сеть.
- [ ] **Step 2: Тест эквивалентности** (FAIL до переноса): `idea_hints_same_after_shared_match` —
  подсказки-идеи на наборе `ideas_recall` совпадают с эталоном v0.0.10-m5.7.
- [ ] **Step 3: Реализация** общего кода и ключей. Тест — PASS.
- [ ] **Step 4: Сетка** делитель {3, 2} × `COVERAGE_CAP` {3, 4, 6} × `WordMatch` {Plain, Stems}
  на `hints.toml`, отчёт по корзинам; `prompt_min_score_fts` {0,2…0,6} на `--mode fts`. Правила
  — §9. Решение — в журнал. Перепроверка после задачи 7 — тем же прогоном.
- [ ] **Step 5: Commit** `feat(hints): пороги покрытия и FTS-only по eval подсказок`.

### Task 10: Буст вида (§10, §16 10)

**Files.** Create: `eval/kind-queries.toml`. Modify: `eval/corpus/**` (трудные соседи, пары
`contradicts`), `crates/zam-server/src/api/search/mod.rs` (слияние, до отсечения `top_k`),
`crates/zam-server/src/config.rs`, `xtask/src/eval/grid.rs`. Test: `crates/zam-server/tests/search_rerank_boosts.rs`.

**Interfaces — Produces:** `[search] kind_boost_mode = "always" | "unless_contradicted" |
"reliable_only"`; `unless_contradicted` проверяет противоречие среди **всех кандидатов после
слияния, до `top_k`**.

- [ ] **Step 1: Набор** ≥ 24 запроса (ожидаемый ответ — `gotcha`/`failure`; ≥ 8 пар противоречия).
  Проверка «не на потолке»: при бусте 1,0 nDCG@10 < 1 у ≥ 60 % запросов — в журнал **до** сетки.
- [ ] **Step 2: Тесты** (FAIL): `unless_contradicted_sees_cut_candidates` (противоречащая запись
  за пределами `top_k` всё равно снимает буст), `reliable_only_boosts_theorem_axiom`,
  `always_is_v009_byte_equal`.
- [ ] **Step 3: Реализация.** Затем сетка §10 (гибрид CUDA и CPU, `rerank=on` на CUDA), правило
  §10, выбор — в журнал.
- [ ] **Step 4: Commit** `feat(search): режим буста вида по набору вне потолка`.

### Task 11: Области, `boost_area`, `w_attach` (§11, §16 11)

**Files.** Create: `eval/area-queries.toml`. Modify: `eval/corpus/**` (области `backend`,
`frontend`, `infra`, ≥ 8 на каждую), загрузчик xtask (`X-Zam-Area`),
`crates/zam-server/src/api/search/mod.rs` (`w_attach`), `crates/zam-server/src/config.rs`.
Test: `crates/zam-server/tests/attachments_search.rs`.

**Interfaces — Produces:** `[search] w_attach: f32` (0..=10, 1,0).

- [ ] **Step 1: Тест** (FAIL): `w_attach_1_byte_equal_v009` на выдаче `docs-queries.toml`.
- [ ] **Step 2: Реализация** и набор ≥ 30 запросов (`same`/`other`/`none`, по ≥ 10).
- [ ] **Step 3: Сетки** `boost_area` {1,0; 1,05; 1,10; 1,20; 1,30} и `w_attach` {0,7; 1,0; 1,3;
  1,6}, правила §11, выбор — в журнал.
- [ ] **Step 4: Commit** `feat(search): boost_area и w_attach по eval`.

### Task 12: Стенд A/B, `checkpoint_impact`, `holdout` (§12, §16 12)

**Files.** Create: `xtask/src/ab.rs`, `eval/ab/{tasks.toml,runs.toml}`. Modify:
`crates/zam-cli/src/cmd/hook/{checkpoint.rs,session_start.rs}`, `crates/zam-server/src/api/{sessions.rs,recall.rs}`,
`eval/README.md`. Test: `xtask/tests/ab.rs` (новый), `crates/zam-cli/tests/hooks.rs`,
`crates/zam-server/tests/sessions.rs`.

**Interfaces — Produces:**
```text
cargo xtask ab --transcripts <каталог> --runs eval/ab/runs.toml --server <url>
  → по плечам: input/cache_read/cache_creation/output токены, токены памяти, вызовы, доля pass, CI
  (из транскриптов читаются только поля usage сообщений; файлы не копируются)
POST /sessions {id, holdout: true} (отдельный запрос; на 400 старого сервера — молча)
GET /sessions/{id} → holdout; /recall в holdout-сессии → text "" (учёт ведётся)
клиент: hooks.checkpoint_impact (false), hooks.holdout (0..=0,5, 0)
```

- [ ] **Step 1: Тесты** (FAIL): `ab_report_on_fixture` (два транскрипта, ответы `/sessions`,
  `runs.toml` → отчёт с интервалами), `holdout_empty_text_and_inherited` (субагент с `parent_id` и
  сессия после compact), `checkpoint_impact_lines_inside_single_reminder`,
  `checkpoint_impact_timeout_60ms_no_lines`.
- [ ] **Step 2: Реализация.** `POST /code/impact` идёт параллельно с `GET /sessions/{id}`, срок 60
  мс внутри 100 мс; до 3 строк внутри единственного напоминания `Stop`. Тесты — PASS.
- [ ] **Step 3: Инструкция владельцу** в `eval/README.md`: ≥ 10 задач × 2 плеча × 2 повтора, плечи
  `memory/off`, `map/nomap`, `impact`. **Прогон — владелец, вне приёмки M6.**
- [ ] **Step 4: Commit** `feat(ab): стенд агентного A/B, checkpoint_impact, holdout`.

### Task 14: Ранжирование карты кода (§12а, §16 14)

**Files.** Modify: `crates/zam-code/src/{proto.rs,rank.rs}` (`EXTRACT_VERSION` 2 → 3, `RefAgg`,
`ref_kind_weight` около стр. 95), экстракторы `crates/zam-code/src/extract/*`,
`crates/zam-store/src/code.rs`, `crates/zam-cli/src/cmd/code/{sync.rs,state.rs}` (исключения по
умолчанию, обнаружение версии сервера), `crates/zam-server/src/api/code.rs`,
`web/src/components/CodeView.svelte`, `crates/zam-server/src/mcp/tools.rs`.
Test: `crates/zam-code/tests/*`, `crates/zam-server/tests/{code_api.rs,code_memory.rs}`,
`crates/zam-cli/tests/code*.rs`.

**Interfaces — Consumes:** `code_refs.recv_count` (2). **Produces:**
```rust
pub struct RefAgg { pub name: String, pub kind: RefKind, pub count: u32, pub first_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub recv_count: Option<u32> }
fn ref_kind_weight(kind: RefKind) -> f64;   // type 2,0; impl 2,0; call 1,0; import 1,0; other 1,0
// вклад call = (count − recv_count) × 1,0 + recv_count × 0,25
// [code] exclude по умолчанию: "**/tests/fixtures/**"; exclude_defaults = false — отключить
```

- [ ] **Step 1: Тесты** (FAIL): `recv_call_detected_rust_ts_py` (`x.f()`, `self.f()`, `a.b.f()`
  против `f()`, `T::f()`, `use …::f`), `old_server_gets_no_recv_count`,
  `fixtures_excluded_by_default_and_pruned`, `weights_table_fixed`, `extractor_metrics_1_000`,
  `line_anchor_symbol_link_by_id` (WebUI и MCP для `L10`).
- [ ] **Step 2: Реализация.** Веса объявлены до замера и не меняются (§12а).
- [ ] **Step 3: Замер** `cargo xtask code-bench` на снимке `HEAD` 9C и на финальном `HEAD`; абляция
  (каждое изменение и все вместе) — справочно; контроль прочих целей code-bench не хуже 9C + 10 %.
  Факт (≥ 6/10 или число) — в журнал.
- [ ] **Step 4: Commit** `feat(code): ранжирование карты — fixtures, вызов по получателю, веса`.

### Task 15: Долги M5.5 и M5.6 (§16 15)

**Files.** Modify: проверка ссылок отчёта — CLI `crates/zam-cli/src/cmd/report.rs`, сервер
`crates/zam-server/src/api/reports.rs`; `web/src/components/ReportView.svelte`, `web/src/lib/print.ts`;
лента отчётов (миниатюры, `api/previews.rs` — права отчёта); `crates/zam-markdown/src/secrets.rs`
(около стр. 358); `crates/zam-server/tests/code_api.rs`; `web/e2e/{reports.spec.ts,user-login.spec.ts}`;
`xtask/src/package.rs`, `about.toml`, `deploy/licenses/`.

**Interfaces — Produces:**
```rust
pub const REPORT_LINK_SCHEMES: [&str; 5] = ["http", "https", "mailto", "vscode", "file"];
pub struct ScanOutcome { pub hits: Vec<SecretHit>, pub passes: u32, pub depth_capped: bool }
pub fn scan_secrets_ext(text: &str) -> ScanOutcome;    // scan_secrets остаётся обёрткой
// запись: warnings += "secrets_scan_depth" при depth_capped; WARN без текста находок
```

- [ ] **Step 1: Тесты** (FAIL): `report_link_schemes` (201 для пяти разрешённых схем, 422 для
  `ftp:`/`javascript:`), `report_preview_token_404`, `scan_depth_capped_warns` (вход-генератор,
  закреплённый фикстурой), `ordinary_inputs_le_3_passes`; e2e — кнопка «копировать путь» у
  `file:`/`vscode:`, `file:` без перехода, лента грузит `kind=thumb`, кнопка «PDF / печать» с
  подсказкой, `user-login` с повтором после `Retry-After`.
- [ ] **Step 2: Флейк** `focus_inputs_load_survives_timeout_single_read` — барьер или точка
  `test-hooks` вместо времени; 20 прогонов `code_api` подряд без падений (лог —
  `.tmp/m6/task15/`).
- [ ] **Step 3: Упаковка** — `LICENSES/` (cargo-about для `zam`) в `zam-*.zip`; тексты LICENSE
  tree-sitter-typescript и tree-sitter-java — в `deploy/licenses/`; `clarify.git` → `clarify.files`
  с теми же SHA-256; `cargo xtask package` без сети.
- [ ] **Step 4: Реализация остального.** Все тесты — PASS; `npm run size` в пределах.
- [ ] **Step 5: Commit** `fix: долги M5.5 и M5.6 — схемы ссылок, миниатюры, PDF, сканер, упаковка`.

### Task 16: Долги M5.7 (§16 16)

**Files.** Modify: `crates/zam-server/src/api/recall.rs` (лёгкий поиск идей, стоп-список,
doc-ссылки), `crates/zam-server/src/api/search/mod.rs` (параметры глубины для лёгкого поиска),
`crates/zam-store/src/ideas.rs` (`on_entry_restored_tx`), `web/src/components/IdeasView.svelte`,
`web/src/lib/ideaButtons.ts` (новый), `web/.size-limit.json` (не поднимать).
Test: `crates/zam-server/tests/{ideas_recall.rs,m57_acceptance.rs}`, `crates/zam-store/tests/ideas.rs`.

**Interfaces — Consumes:** `prepared_terms`, `WordMatch::Stems` (9); эпохи (7). **Produces:**
```rust
pub const IDEA_STOP_STEMS: &[&str];           // основы: сделать, добавить, исправить, обновить,
    // поправить, починить, модуль, тест, функция, файл, make, add, fix, update, change, module, test
pub struct LightSearch { pub channel_depth: usize, pub snippets: bool } // идеи: max(4·top_k, 20), false
```

- [ ] **Step 1: Эталон** — ответы v0.0.10-m5.7 на наборе ≥ 40 запросов (положительные и
  отрицательные) и на `ideas_recall` → `crates/zam-server/tests/fixtures/idea_hints_m57.json`.
- [ ] **Step 2: Тесты** (FAIL): `idea_hints_top5_match_m57`, `idea_stop_verbs_negatives` (≥ 6 новых,
  например «добавь тест на модуль авторизации» к идее «Экспорт отчётов в PDF» → нет; пять запросов
  таблицы финального ревью M5.7 — прежние ответы), `restored_idea_done_at_is_task_time`,
  `inner_search_ideas_median` (`--ignored`, 50 идей, 200 вызовов, печать `ZAM_TIMING m6`).
- [ ] **Step 3: Реализация.** Если топ-5 расходится при урезанной глубине — глубина 50, снимаются
  только сниппеты (запасной вариант спецификации). `cargo doc --no-deps` с
  `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links"` — чисто.
- [ ] **Step 4: `IdeasView`** — общие кнопки решения и `localTime` (6) вынесены; `npm run size` —
  `IdeasView` ≤ 7,2 КБ.
- [ ] **Step 5: Замер.** Микрозамер — цель медиана ≤ 4 мс в debug. Стенд Linux (release): медиана
  p95 `prompt` из 5 прогонов по 40 вызовов на решение вперемешку, прирост с 50 идеями — цель ≤
  +7,5 мс, критерий +15. Числа — в журнал.
- [ ] **Step 6: Commit** `perf(ideas): дешёвый поиск подсказок-идей; fix: долги M5.7`.

### Task 17: Снимок сессии (§8а, §16 17)

**Files.**
- Create: `crates/zam-cli/src/cmd/hook/snapshot/{mod.rs,claude.rs,codex.rs}`,
  `crates/zam-cli/src/cmd/session.rs`, `crates/zam-server/src/api/snapshot.rs`,
  `crates/zam-cli/tests/snapshot.rs`, `crates/zam-server/tests/snapshot.rs`,
  `crates/zam-cli/tests/fixtures/transcripts/*.jsonl`.
- Modify: `crates/zam-cli/src/cmd/hook/{mod.rs (Kind::accepts),checkpoint.rs,session_start.rs,
  journal.rs (Ev::Snapshot),cache.rs (gc_cache: snapq-, snapst-)}`,
  `crates/zam-server/src/agent_setup/mod.rs` (`claude_settings`, `codex_hooks`),
  `crates/zam-server/src/api/{recall.rs,sessions.rs}`, `crates/zam-server/src/maintenance.rs`,
  `crates/zam-server/src/config.rs`, `web/src/components/SessionsView.svelte` (блок «Снимок сессии»),
  `crates/zam-server/src/agent_setup/guide.{ru,en}.md`.

**Interfaces — Consumes:** `put_snapshot`/`get_snapshot`/`gc_snapshots` (2), `SessionCtx` (3), рендер
дайджеста (8). **Produces:**
```rust
// клиент, snapshot/mod.rs — нейтральная форма (сервер знает только её)
pub struct Snapshot { pub trigger: Trigger, pub agent: AgentKind, pub status: SnapStatus,
    pub skipped: u32, pub compact_summary: Option<String>, pub prompts: Vec<String>,
    pub files: Vec<String>, pub commands: Vec<Command> }
pub struct Command { pub cmd: String, pub failed: Option<bool> }
pub enum Trigger { PostCompact, PreCompact, SessionStartCompact, Stop, SessionEnd, CatchUp, Manual }
pub trait TranscriptAdapter { fn accept_line(&mut self, line: &str); fn finish(self) -> Snapshot; }
pub fn take(path: &Path, root: &Path, adapter: AgentKind, tail_kb: u32, deadline: Duration)
    -> Option<Snapshot>;   // отдельный поток; срок — весь ввод-вывод; после срока — None/partial
pub fn mask_and_limit(s: Snapshot, max_bytes: usize) -> Snapshot;  // mask_text, порядок обрезки §8а
pub const PREFILTER: [&str; 7] = ["\"isCompactSummary\"", "\"compact_boundary\"", "\"type\":\"user\"",
    "\"tool_use\"", "\"compacted\"", "\"user_message\"", "\"session_meta\""];
// Kind::Checkpoint принимает "Stop" | "PreCompact" | "PostCompact" | "SessionEnd"
// сервер
// PUT /sessions/{id}/snapshot {taken_at, trigger, agent, status, skipped, data} → 204;
//   без явной сессии — 400 session-required; > snapshot.max_bytes — 413; secrets → warnings
// GET /sessions/{id}/snapshot → 200 | 404 (чужая, в том числе участник общего решения)
// SessionEnd → PATCH /sessions/{id} {ended: true}
// [recall] snapshot_tokens 0..=2000 (400), snapshot_stale_min 0..=1440 (30), snapshot_digest_days 1..=30 (2)
// [snapshot] max_bytes 1024..=65536 (16384), keep_days 1..=90 (14)
// CLI: zam session snapshot --transcript <файл> [--format claude|codex] [--session <id>]
```

- [ ] **Step 0 (зависит от владельца): живые фикстуры.** Владелец разрешил чтение (вопрос 2,
  вариант А) — по одному свежему журналу Claude Code и Codex: короткие фрагменты, обезличенные
  (текст, пути, имена, ключи), в `crates/zam-cli/tests/fixtures/transcripts/<агент>-<версия>-<случай>.jsonl`.
  Сами журналы не копируются. Вариант Б — фрагменты от владельца там же. Сверка форматов §8а
  («проверить на живом журнале») — в журнал этапа. **До шага 0** шаги 1–6 выполняются на фикстурах
  `-doc-`, а §17 п. 19 остаётся открытым.
- [ ] **Step 1: Тесты адаптеров** (FAIL): `claude_summary_record_is_not_compact`,
  `claude_compact_after_boundary`, `claude_whitelist_drops_inserts` (`<bash-stdout>`,
  `<local-command-stdout>`, `<command-name>`, `<system-reminder>`, картинка, `isSidechain`, `isMeta`,
  чужой `sessionId`), `codex_compacted_and_user_message`, `codex_ignores_user_instructions_and_encrypted`,
  `broken_and_unknown_lines_skipped`, `huge_line_skipped`, `path_outside_root_dropped`,
  `secrets_masked_in_prompts_and_flags` (`-pXXX`, `--password X`, `curl -u u:p`, `Bearer`; приманки
  — конкатенацией), `limit_order_commands_prompts_summary`, `summary_keeps_tail_sections`.
- [ ] **Step 2: Тесты хуков** (FAIL): `snapshot_deadline_never_delays_stop`,
  `snapshot_tail_of_growing_file`, `snapshot_bad_path_is_silent`, `stop_throttle_16kib_60s`,
  `post_compact_and_session_end_accepted_within_100ms` (пустой вывод), `session_end_patches_ended`,
  `compact_put_before_recall`, `catch_up_before_recall_on_startup`, `old_server_three_attempts_then_queue`,
  `switch_off_never_opens_transcript`, `hooks_log_has_no_snapshot_text`, `ev_snapshot_metadata_only`.
- [ ] **Step 3: Тесты сервера** (FAIL): `put_requires_explicit_session`, `foreign_snapshot_404`
  (другой пользователь и участник общего решения), `put_413_over_limit`, `server_remasks_secrets`,
  `gc_sessions_and_keep_days_remove`, `digest_shows_last_state_when_task_stale`,
  `digest_hides_live_session_by_last_seen`, `digest_hides_when_work_continued` (по `memory_writes`,
  ревизии задачи другой сессией, действующему чужому захвату), `recovery_without_summary`,
  `snapshot_quota_parts`, `snapshot_tokens_zero_hides`, `digest_with_snapshot_deterministic`.
- [ ] **Step 4: Реализация** по §8а: хуки и фрагменты сервера `claude_settings` (`SessionEnd`,
  `PostCompact`) и `codex_hooks` (`SessionEnd`), воркер обслуживания (раз в сутки
  `gc_snapshots`), `zam session snapshot`, блок WebUI (текст без `{@html}`; e2e на XSS). Тесты —
  PASS.
- [ ] **Step 5: Бюджеты** — `hook_budgets` на release-сборке с журналом 50 МиБ (`checkpoint` ≤ 100
  мс, `session-start` с догоняющим снимком ≤ 300 мс). Фикстура ТС-2 (`transcript_path`, сессии с
  «обрывом») — в `eval/sessions.toml`.
- [ ] **Step 6: Полный прогон** `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast -j 2
  --target-dir target/r1-new`; итог — в журнал.
- [ ] **Step 7: Commit** `feat(hooks): снимок сессии из журнала агента`.

### Task 13: Инструкция, подтверждение, выпуск `0.0.11-m6` (§13, §16 13) — последняя

**Files.**
- Блок и протокол: `crates/zam-core/src/lib.rs` (`AGENT_BLOCK_VERSION = 7`),
  `crates/zam-server/src/agent_setup/{templates.rs,guide.ru.md,guide.en.md}`,
  `crates/zam-server/src/mcp/{mod.rs (INSTRUCTIONS),tools.rs}`,
  `crates/zam-server/tests/agent_setup.rs` (`MOVED_TO_GUIDE`).
- Приёмка: `crates/zam-server/tests/m6_acceptance.rs`, `crates/zam-cli/tests/m6_acceptance.rs`,
  `xtask/src/smoke.rs`.
- Документы и версия: ТЗ `docs/specs/2026-10-17-zeroaimemory-tz.md` → v1.24 (§7.11, §7.12, §7.13,
  §8.3, §9, §10, §10.7 — явный пересмотр «метаданные, не содержимое», §12–§14, §14.2 блок v7,
  §16.3, §17–§19, §21, §23); спецификация — статус «перенесено в ТЗ v1.24» с отступлениями;
  `CHANGELOG.md` `[0.0.11-m6]` (совместимость с 0.0.10-m5.7, откат `bak-v24`); `README.md`;
  `deploy/README-install.md` — раздел «С 0.0.10-m5.7 на 0.0.11-m6» (миграция 0025 и `bak-v24`, блок
  v7 и напоминание, хуки `SessionEnd`/`PostCompact` после `zam init`, снимок сессии и выключатель) и
  «Известные ограничения (0.0.11-m6)»; `docs/journal.md` — «Этап M6 (экономия токенов): итог»;
  журнал этапа — «Итог M6»; версия `0.0.11-m6` в `Cargo.toml`, `Cargo.lock`, `web/package.json`,
  `web/package-lock.json`; `deploy/config.example.toml`.

**Interfaces — Consumes:** задачи 1a–17. **Produces:** тесты приёмки по §17 спецификации —
`acceptance_ts1_vs_b4` и `acceptance_ts2_vs_b56` (читают отчёт eval на holdout),
`acceptance_honest_budget`, `acceptance_dates_utc`, `acceptance_mcp_session_subagent`,
`acceptance_epochs_and_reread`, `acceptance_focus`, `acceptance_digest_deterministic`,
`acceptance_compat_matrix`, `acceptance_migration_0025`, `acceptance_hook_budgets`,
`acceptance_snapshot` (§17 п. 19, только с живыми фикстурами), `acceptance_m57_debts`,
`acceptance_misc_outputs` (§17 п. 21); `compat_old_zam_new_server`, `compat_new_zam_old_server`.

- [ ] **Step 1: Блок v7.** Основной вариант §13: строка поиска с `focus`; «; `certainty` меняй с
  `reason`» и «функция — `путь#Тип::метод`» — в протокол и в `MOVED_TO_GUIDE`. Правило отвергнутых
  подходов — в `guide` («Запись», `insight` с `kind: decision`) и в `description` поля
  `data.kind` у `memory_log`. Тест `agent_setup_block_v7_under_500_tokens_ru_en` (оценка ≈ 482
  ru / 478 en); `block_version_reminder_v6_to_v7` — одно напоминание. `INSTRUCTIONS`, описания
  `memory_read`/`memory_search` (`focus`, легенда, «уже выдавалось»), разделы `guide` «Фокусное
  чтение» и «Снимок сессии». Фактические числа — в журнал.
- [ ] **Step 2: Переснятие на финальном отпечатке** — B4, B5.6, B5.7 и M6. Совместный
  подтверждающий прогон всех выбранных значений на holdout: fp16 CUDA (критерий) и int8 CPU
  (контроль). Не прошло — откатить последнее изменённое значение и повторить (§3.5). Все наборы
  качества: `queries.toml`, `docs-queries.toml`, `kind-queries.toml`, `area-queries.toml`,
  `hints.toml`, `rerank=on`. Отчёт — `eval/results/<дата>-m6-<модель>-tokens.{json,md}`.
- [ ] **Step 3: Приёмка и совместимость.** Тесты приёмки, smoke обоих направлений с бинарником
  тега `v0.0.10-m5.7`, release-сборка → `cargo xtask package --platform windows-x64 --flavor cpu
  --version 0.0.11-m6` → `cargo xtask smoke --archive … --expect-device cpu` (SMOKE OK: шаги
  M5.7 и шаги M6 — фокус, `zam sessions tokens`, снимок `zam session snapshot` на фикстуре), `npm
  run e2e` на release, стенд Linux (Debian 13, 2 ядра / 2 ГБ: проверка работоспособности), полный прогон
  `ZAM_REQUIRE_ML=1`. CI (CI, Edge, CUDA) — зелёный. Числа — в журнал.
- [ ] **Step 4: Перенос и выпуск** по перечню Files; результат по цели 30 % и по критерию карты
  кода записывается фактом. Решения составителя плана — с пометкой «[решение составителя плана
  M6]». Коммит `chore(release): 0.0.11-m6`.

---

## Решения составителя плана

Каждое решение записывается в журнал этапа и переносится в ТЗ (задача 13).

1. **Порядок задач** — 1a → 2 → 1b → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 14 → 15 → 16 → 17 →
   13. Задача 2 идёт перед 1b: 1b — долгая разметка, а схема нужна задачам 3 и 7.
2. **Одна миграция `0025_session_epochs.sql`** для всей схемы этапа, включая `code_refs.recv_count`
   и `session_snapshots`. Две миграции за этап дали бы два `bak-v*` и два шага отката. До задачи 7
   эпоха всегда 0, и поведение не меняется.
3. **ТС-1 ходит в `POST /mcp` напрямую** из xtask (`McpClient`), ТС-2 — через процесс `zam mcp`.
   ТС-1 меряет выдачу сервера, а ТС-2 — путь агента вместе с привязкой (§3.1).
4. **Бинарники баз** собираются из тегов в `target/r1-new/tags/<тег>` один раз и переиспользуются.
   Переснятие баз на финальном отпечатке — только в задаче 13.
5. **Разбиение dev/holdout** — `blake3(SPLIT_SALT ‖ id)`, соль `m6-2026-11-04`. Сетка с
   `--split holdout` — ошибка xtask.
6. **Дата** — одна функция `zam_core::time::fmt_agent_ts`; WebUI — одна `localTime` в
   `web/src/lib/time.ts`, её же берёт `IdeasView` (задача 16).
7. **Правило совпадения слов** — общий код в `zam-search` (`prepared_terms`, `WordMatch`).
   Подсказки-идеи переходят на него без изменения поведения, а `post_tool` выбирает правило по сетке.
8. **Фокус** — отдельный модуль чтения, без реранкера; срок `focus_timeout_ms` проверяется точкой
   `test-hooks`, а не реальным ожиданием.
9. **Снимок — нейтральная форма `Snapshot` в клиенте**, адаптеры по агентам за трейтом
   `TranscriptAdapter`, сервер в `api/snapshot.rs` знает только JSON формы.
10. **Признаки «работа продолжилась»** — только наблюдаемые сервером: `memory_writes`,
    `entry_revisions.session_id` + `ts`, действующий захват задачи (`data.claimed_by.session`).
11. **Полный прогон** `ZAM_REQUIRE_ML=1` — в задачах 8, 17 и 13; в остальных — затронутые наборы.
12. **Эталон подсказок-идей M5.7** хранится фикстурой
    `crates/zam-server/tests/fixtures/idea_hints_m57.json`. Его снимает однажды бинарник тега
    `v0.0.10-m5.7`, а задачи 9 и 16 сравнивают с ним.
13. **Задачи, зависящие от владельца** (шаг 0 задачи 17, прогон A/B), этап не блокируют: работа идёт
    на фикстурах `-doc-`, а соответствующие пункты приёмки помечаются открытыми.
14. **Страница сессий WebUI — новая** (`SessionsView.svelte`, маршрут `#/sessions`). Спецификация
    (§8, §8а, §17 п. 21) говорит о ней как о существующей, но в WebUI её нет (проверено 2026-11-04:
    в `web/src/routes` и `web/src/components` страницы сессий нет). Предупреждение `rules_heavy` —
    в «Состоянии индекса» и плашкой у папки Rules: отдельной страницы Rules тоже нет.

## Самопроверка

| Спецификация | Задачи |
|---|---|
| §0 принцип, §3.5 правила выбора, неинфериорность | 1a (статистика), 5, 8, 9, 10, 11 (сетки), 13 (holdout) |
| §3.1 ТС-1, §3.2 нагеты, заморозка, независимая разметка | 1b |
| §3.1 ТС-2, идеи, «обрыв» | 4, 17 (фикстуры) |
| §3.3 базы B4/B5.6/B5.7, обнаружение возможностей, v0.0.9 ≡ v0.0.10 | 1a, 13 |
| §3.4 симулированный агент и дочитывание | 1a |
| §3.6 отчёт | 1a, 4, 13 |
| §4.1–§4.3 карточки v2, честный бюджет, строки подсказок | 5 |
| §4.4 дата UTC | 6 |
| §5 ответы записи ≤ 80 токенов, `echo` | 5 |
| §6 фокус | 7 |
| §7.1 привязка MCP, `unattributed`, общий предок Codex | 3 |
| §7.2–§7.3 эпохи, идеи без эпохи, «уже выдавалось» | 2, 7 |
| §8 Rules-резерв, индексы, детерминированность, идеи, учёт | 8 |
| §8а снимок сессии | 2 (таблица), 17 |
| §9 пороги и правило совпадения слов | 9 |
| §10 буст вида | 10 |
| §11 области, `boost_area`, `w_attach` | 11 |
| §12 A/B, `checkpoint_impact`, `holdout` | 12 |
| §12а карта кода, `recv_count` | 2 (колонка), 14 |
| §13 совместимость, блок v7, миграция, версия | 2, 13 |
| §14 конфигурация | 5, 7, 8, 9, 10, 11, 12, 14, 17 |
| §16 15 долги M5.5/M5.6 | 15 |
| §16 16 долги M5.7 | 16 (и 7, 9) |
| §17 п. 1–21 | 13 (приёмка), по пунктам — задачи выше |

Проверено: имена из блоков Interfaces совпадают между задачами (`SessionCtx`, `bump_epoch`,
`put_snapshot`, `prepared_terms`, `WordMatch`, `fmt_agent_ts`, `localTime`, `RefAgg.recv_count`).
Тестам Review Focus назначены задачи 1a, 5, 7, 13, 17.
