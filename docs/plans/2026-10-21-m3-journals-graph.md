# M3. Журналы, граф и совместный доступ — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal.** Память решения становится общей и полной:
- решение хранится в собственной базе `solutions/<agent_solution_id>.db`, владелец открывает его другим пользователям с подтверждением и отзывом (§11.6);
- у решения есть журналы Changelog, Releases (со сравнением), Insights (статусы, замещение, отзыв, дедупликация, триггеры);
- у пользователя — Vault с шифрованием и аудитом, сканер секретов перед индексацией;
- между заметками и записями строится граф (`edges`, wiki-ссылки, `expand=graph`);
- поиск идёт сразу по нескольким базам, учитывает точки папок и статусы записей, а eval меряет «вспомнил отменённое».

Этап закрывают e2e приёмки «изоляция», «замещение без потери», «секрет через агента» и версия `0.0.4-m3`.

**Architecture.**
- Новое понятие **тенант** (`zam_core::Tenant`): личная память пользователя `users/<id>.db` или память решения `solutions/<guid>.db`. Схема у обеих баз одна (миграции `migrations/memory/`), содержимое разное: в базе пользователя — Rules, Human, Knowledge, личный Vault; в базе решения — одно пространство решения и его журналы.
- Реестр решений и доступа — в `system.db` (`solutions`, `solution_access`). Доступ проверяется на каждом запросе без кэша: отзыв действует сразу.
- `UserDbs` становится `MemoryDbs` (ключ — тенант). Индекс, воркер, reindex и поиск обходят тенантов, а не только пользователей. В Qdrant payload несёт `tenant` (`u:<id>` / `s:<guid>`, `is_tenant`), коллекция переходит на `schema: 2` и пересобирается из `embeddings` без инференса.
- Записи журналов (`entries`) проходят тот же конвейер, что заметки: чанки → FTS → `index_jobs` (`target_kind = 'entry'`) → Qdrant. Vault хранится отдельной таблицей `vault_items` вне конвейера индексации: значения структурно не могут попасть в FTS и Qdrant.
- Граф — таблица `edges` и рекурсивные CTE за трейтом `GraphStore`. Ссылки разрешаются одним кодом для рендера и для рёбер.

**Tech Stack.** К стеку M2 добавляется (лицензии — по `about.toml`, аудит каждой — в журнал задачи):
- `chacha20poly1305` 0.10 (Apache-2.0 OR MIT) — XChaCha20-Poly1305 для Vault (§5.2 «Криптография»);
- `zeroize` 1 (Apache-2.0 OR MIT) — затирание ключей;
- `regex` 1 (MIT OR Apache-2.0; уже в дереве через `tokenizers`/`syntect`, теперь прямая зависимость) — сканер секретов и проверка триггеров (обратных ссылок нет по построению, ReDoS невозможен);
- `globset` 0.4 (Unlicense OR MIT, берём MIT) — проверка `file_glob` триггеров.

Новых npm-зависимостей нет: визуализация графа (`d3-force`, `sigma`) — этап M7. `elkjs` (EPL-2.0) не добавляется — ждёт решения владельца.

**Spec:** `docs/specs/2026-10-17-zeroaimemory-tz.md` (v1.8, в задаче 1 становится v1.9). Нужны:
- §2.2, §2.3, §3;
- §6.1–6.4;
- §7 целиком;
- §8.1–8.3, §8.5;
- §9.1–9.2 (поправки п. 4, точки папок), §9.4 (модель для общих решений);
- §11.1, §11.2, §11.6;
- §12 (журналы, vault, граф, решения, настройки);
- §13.2 (`zam log|insight|release|vault|link|solution`), §13.3 (`reindex`);
- §15.1, §15.4;
- §16.1;
- §17 (первый старт Qdrant);
- §18.1 (e2e), §18.2 («вспомнил отменённое», гейт int8);
- §19 (строка M3).

Журнал M2 с backlog и ответами владельца: `docs/plans/2026-10-19-m2-index-ml-log.md`. Журнал этого этапа: `docs/plans/2026-10-21-m3-journals-graph-log.md`.

## Global Constraints

- **Временные файлы**, логи прогонов, тестовые данные — только внутри проекта: `.tmp/` (под `.gitignore`) или `target/` (в тестах `env!("CARGO_TARGET_TMPDIR")`, модели — `target/models/`, бинарники — `target/deps/<платформа>/`). Запрещены `/tmp` (на Windows это корень диска), `%TEMP%`, `AppData\Local\Temp`, scratchpad Claude Code, `std::env::temp_dir()`, `tempfile` без явного `dir=`/`tempdir_in`, `os.tmpdir()`. **Правило передаётся каждому субагенту в тексте задания.**
- **Окружение.** Rust portable лежит в `.toolchain/`: перед `cargo` и `npm` выполнять `. scripts/dev-env.ps1` (PowerShell) или `. scripts/dev-env.sh`. Данные проекта на HDD — пределы ожидания в тестах с запасом на fsync.
- **Платформы:** Windows x64 и Linux x64. GPU — только CUDA. Без Docker.
- **Зависимости** — только перечисленные в Tech Stack или с аудитом лицензии (разрешённые — список `accepted` в `about.toml`; проект под MIT). Новая зависимость и её лицензия записываются в журнал задачи. EPL-2.0 (`elkjs`) не добавлять.
- **Проверки каждой задачи:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test … --no-fail-fast` (без `--no-fail-fast` упавший крейт скрывает остальные — урок M2); для WebUI — `npm run check`, `npm run build`, `npm run size`, `npm run e2e`.
- **Единообразие с M0–M2:** русские комментарии, документы и тексты UI; английские идентификаторы; `thiserror` в библиотеках, `anyhow` в бинарниках; ошибки API — RFC 9457 через `ApiError`; логи — `tracing` (без текста заметок, записей, секретов, токенов и ключей); конфигурация — слой «файл → `ZAM_*` → флаги»; пути через `Path::join` по компонентам; один писатель на файл SQLite (`Db::write`).
- **Миграции** — номера по факту: следующая системная — `0003`, следующая для баз памяти — `0007` (каталог `migrations/user/` в задаче 3 переименовывается в `migrations/memory/`). Перед миграцией — копия файла (`migrate::apply`), миграция существующих данных — идемпотентна и с проверкой.
- **Инварианты (не нарушаются ни одной задачей):**
  - Human никогда не попадает в Qdrant и не виден по токену ни в каком виде, в том числе участникам общего решения;
  - значения Vault никогда не попадают в FTS, Qdrant, ответы поиска и логи; Vault решения не разделяется между участниками;
  - узлы с `index: false` не векторизуются;
  - история только дописывается (`revisions`, `entry_revisions`, `vault_revisions` — append-only триггерами), удаление мягкое, замещение и отзыв ничего не удаляют;
  - SQLite — источник истины, Qdrant пересобирается из `embeddings` без инференса.
- **Совместный доступ (§11.6) — `[предложение, ждёт подтверждения владельца]`.** Так помечаются соответствующие места ТЗ и кода (doc-комментарий модуля). Модель хранения изолирована в задаче 3 (`MemoryDbs`, перенос), чтобы её можно было поменять, не трогая журналы и граф.
- **Коммиты** — после каждой задачи, ветка `m3-journals`, Conventional Commits на русском, trailer `Co-Authored-By` по реальной модели исполнителя.

## Review Focus

1. **Обновление установки M2.** Решения, созданные в M1–M2, лежат в базах пользователей, коллекция Qdrant — `schema: 1`. После обновления каждое решение переносится в `solutions/<guid>.db` без потерь (ревизии, чанки, эмбеддинги, вложения). Повторный старт ничего не делает. Прерывание посреди переноса восстанавливается при следующем старте. Векторный индекс пересобирается без инференса. Тесты — в задачах 3 и 5.
2. **Отзыв или отклонение доступа, пока агент работает с тем же токеном.** Уже следующий запрос (чтение узла, запись, поиск, граф, записи журналов) получает 403 `solution-access` или, в поиске, исключение решения с предупреждением. Кэша решений о доступе нет. Тесты — в задачах 4, 6 и 18.
3. **Ложные срабатывания сканера секретов на идентификаторах:** git SHA, sha256, UUID, base32-id (26 символов), длинные URL, хэши в логах. Они не маскируются, иначе ломается поиск по точному идентификатору. Настоящие ключи и токены маскируются. Тест — в задаче 11.
4. **Мастер-ключ Vault потерян, подменён или расходится с `ZAM_MASTER_KEY`.** Сервер работает, Vault «заблокирован» с понятной причиной (503). При существующих ключах пользователей новый мастер-ключ молча не создаётся. Тест — в задаче 12.
5. **Вики-ссылки при переименовании, переносе, удалении и позднем создании цели**, а также две заметки с одним именем. Рёбра и рендер разрешают ссылку одинаково, backlinks обновляются, неразрешённая ссылка разрешается, когда цель появляется. Тест — в задаче 10.

---

## Карта файлов

```
docs/specs/2026-10-17-zeroaimemory-tz.md        # v1.9 (задача 1), итог (задача 19)
crates/zam-core/src/{solution,target}.rs        # SolutionGuid, Tenant, TargetKind, TargetRef
crates/zam-store/migrations/system/0003_solutions.sql      # реестр решений и доступ
crates/zam-store/migrations/system/0004_index_skipped_tenant.sql
crates/zam-store/migrations/system/0005_vault_keys.sql
crates/zam-store/migrations/memory/0007_attribution.sql    # revisions.user_id (каталог user/ → memory/)
crates/zam-store/migrations/memory/0008_entries.sql        # entries, entry_revisions, triggers
crates/zam-store/migrations/memory/0009_edges.sql
crates/zam-store/migrations/memory/0010_vault.sql          # vault_items, vault_revisions
crates/zam-store/src/{access,memory_db,solution_move,graph,links,vault}.rs
crates/zam-store/src/entries/{mod,write,read,chunks,triggers}.rs
crates/zam-markdown/src/{secrets.rs,frontmatter.rs}        # сканер; relations
crates/zam-index/src/{vector,qdrant,worker,reindex,rollup,edge}.rs, supervisor/*
crates/zam-search/src/{hybrid,adjust}.rs                   # бусты, канал папок
crates/zam-server/src/api/{scope,solutions,spaces,entries,releases,insights,triggers,graph,vault,admin}.rs
crates/zam-server/src/api/search/{mod,targets,merge}.rs    # бывший search.rs
crates/zam-server/src/vault/{mod,keys,crypto}.rs
crates/zam-cli/src/cmd/{solution,log,release,insight,vault,link}.rs
web/src/components/{ChangelogView,ReleasesView,InsightsView,VaultView,Links}.svelte, routes/{Solutions,Settings}.svelte
eval/{insights.toml,queries.toml,grid-m3.toml,README.md}, xtask/src/eval/*, xtask/src/bench.rs
crates/zam-server/tests/m3_acceptance.rs
```

---

### Task 1: ТЗ v1.9 — хранение памяти решений, совместный доступ, журналы M3; закрытие пунктов M2

**Files:**
- Modify: `docs/specs/2026-10-17-zeroaimemory-tz.md`
- Modify: `docs/plans/2026-10-19-m2-index-ml-log.md` (раздел «Ответы владельца (2026-10-21)», пункт 13)

**Interfaces:**
- Produces: текст ТЗ, на который ссылаются задачи 2–19. Имена и схемы берутся из Interfaces задач 2, 3, 7, 10 и 12 этого плана без изменений.

- [ ] **Step 1: Правки ТЗ.** Каждое место дизайна §11.6 помечается `[предложение, ждёт подтверждения владельца]`.
  - Шапка: версия 1.9, дата.
  - §2.2: пункт «Совместные решения нескольких пользователей» убрать из не-целей. Совместное редактирование в реальном времени (CRDT) остаётся не-целью.
  - §3: термины «Решение» (идентификатор `agent_solution_id`), «Владелец решения», «Участник решения», «База памяти» (`users/<id>.db` или `solutions/<guid>.db`).
  - §6.1: `solutions/<agent_solution_id>.db`, `blobs/solutions/<agent_solution_id>/…`.
  - §6.2: таблицы `solutions`, `solution_access` (схема задачи 2), `user_keys` (задача 12), `index_skipped` по тенанту (задача 5).
  - §6.3: «Базы памяти» — общая схема для обеих баз. Где что лежит: в базе решения — пространство решения, заметки, ревизии, журналы, `edges`, чанки, `embeddings`, `index_jobs`; в базе пользователя — Rules, Human, Knowledge (с журналом insights `scope=global`) и Vault. Поля `revisions.user_id`, `entries.user_id`. Vault — отдельные таблицы `vault_items`/`vault_revisions` вместо `entries.kind='vault'` **[решение исполнителя]**: значения структурно вне конвейера индексации. Рёбра — только внутри одной базы.
  - §6.4: payload `tenant` (`u:<user_id>` | `s:<agent_solution_id>`, `is_tenant`) вместо `user_id`; `status` у заметок — `active`; `metadata.schema = 2`; коллекция `schema 1` пересобирается из `embeddings`.
  - §7.1: Knowledge в базе пользователя, в том числе insights `scope=global` и личный Vault.
  - §7.2: журналы решения хранятся в базе решения.
  - §7.6: Vault каждого участника — в его базе пользователя, привязан к `agent_solution_id`, не разделяется никогда. Проверка мастер-ключа (`vault.key_check`), расхождение с `ZAM_MASTER_KEY` → Vault заблокирован, новый ключ молча не создаётся.
  - §8.5: формат `relations:` во frontmatter (задача 10), рёбра внутри базы, межбазовые связи — не в M3.
  - §9.1: `X-Zam-Area` — текущая область для буста (§11.2), `scope.area` — строгий фильтр; `filters.kinds` по умолчанию без `folder`.
  - §9.2: канал точек папок `w_folder` (задача 14); бусты `gotcha`/`failure` и области — задача 9.
  - §9.4 и §10: для общего решения кэш выжимок и проактивное вспоминание общие на решение, личный Knowledge подмешивается только свой — реализация на M4.
  - §11.1: ключ `agent_solution_id` (старый `id` читается).
  - §11.6: дизайн целиком — хранение (задача 3), реестр и переходы состояний (задача 2), режимы `rw`/`ro`, подтверждение без WebUI (CLI `zam solution access …` и REST), число ожидающих запросов в `GET /spaces` и `zam status` (дайджест старта сессии — M4), атрибуция `user_id`, Qdrant-фильтр по набору тенантов. Убрать «в очереди», этап — M3.
  - §12: новые эндпоинты задач 4, 8–10 и 12; заголовок `X-Zam-Area`.
  - §13.2: `zam solution access list|requests|approve|reject|revoke`, `zam log …`, `zam insight …`, `zam release …`, `zam vault …`, `zam link …`.
  - §13.3: `zam-server reindex [--user ID | --solution GUID] [--space S] [--full]`.
  - §18.2: гейт int8 — нижняя граница 95 % интервала парного бутстрепа Δ nDCG@10 и Δ recall@5 (int8 − fp32) ≥ −0,01, косинус и Спирмен — справочно (решение владельца 2026-10-21); то же — в §8.4.
  - §19: в строку M3 — совместный доступ (§11.6), поиск по точкам папок, eval «вспомнил отменённое»; критерий — плюс e2e совместного доступа.
  - §23: запись v1.9.
- [ ] **Step 2: Журнал M2.** Пункт 13 «Ответов владельца»: решение владельца 2026-10-21 — перенарезку баз `chunked_approx` оставить как есть. Пояснение: перенарезка разовая (флаг снимается после успеха), а повтор после прерывания переиспользует кэш `embeddings` по `embed_key`. Ссылка на журнал M3.
- [ ] **Step 3: Самопроверка.** `rg -n "в очереди|claude_solution_id|Совместные решения" docs/specs` — нет устаревших формулировок; `rg -n "предложение, ждёт подтверждения владельца" docs/specs` — есть в §6.1, §6.2, §6.3, §6.4, §9.4, §11.6.
- [ ] **Step 4:** Коммит `docs: ТЗ v1.9 — память решений, совместный доступ, журналы M3`.

### Task 2: Доменные типы и реестр решений с доступом в `system.db`

**Files:**
- Create: `crates/zam-core/src/solution.rs`, `crates/zam-core/src/target.rs`
- Modify: `crates/zam-core/src/lib.rs`
- Create: `crates/zam-store/migrations/system/0003_solutions.sql`, `crates/zam-store/src/access.rs`, `crates/zam-store/tests/access.rs`
- Modify: `crates/zam-store/src/{lib.rs,system.rs}` (`SYSTEM_MIGRATIONS`, реэкспорт)

**Interfaces:**
- Produces (`zam-core`):
  ```rust
  /// agent_solution_id: UUID с дефисами, хранится в нижнем регистре.
  pub struct SolutionGuid(String);
  impl FromStr for SolutionGuid { type Err = IdError; }   // только 36 символов с дефисами (как canonical_guid сервера)
  impl SolutionGuid { pub fn as_str(&self) -> &str; pub fn new_v7() -> SolutionGuid; }
  impl Display, Serialize, Deserialize, Hash, Eq, Ord, Clone for SolutionGuid;
  pub enum Tenant { User(Id), Solution(SolutionGuid) }
  impl Tenant { pub fn key(&self) -> String /* "u:<id>" | "s:<guid>" */; pub fn parse_key(s: &str) -> Option<Tenant>; }
  pub enum TargetKind { Node, Entry }                      // serde lowercase
  pub struct TargetRef { pub kind: TargetKind, pub id: Id } // "n:<id>" | "e:<id>" в Display/FromStr
  ```
- Схема `0003_solutions.sql`:
  ```sql
  CREATE TABLE solutions (
      guid        TEXT PRIMARY KEY,                -- agent_solution_id
      space_id    TEXT NOT NULL UNIQUE,            -- id пространства в базе решения
      name        TEXT NOT NULL,
      owner_id    TEXT NOT NULL REFERENCES users(id),
      created_at  TEXT NOT NULL,
      archived_at TEXT
  );
  CREATE TABLE solution_access (
      guid         TEXT NOT NULL REFERENCES solutions(guid),
      user_id      TEXT NOT NULL REFERENCES users(id),
      access       TEXT NOT NULL CHECK (access IN ('rw','ro')),
      state        TEXT NOT NULL CHECK (state IN ('pending','approved','rejected','revoked')),
      requested_at TEXT NOT NULL,
      token_id     TEXT,                           -- токен, которым запрошено
      decided_at   TEXT,
      decided_by   TEXT,                           -- 'admin' | id владельца
      reason       TEXT,
      PRIMARY KEY (guid, user_id)
  );
  CREATE INDEX solution_access_user ON solution_access (user_id, state);
  CREATE INDEX solution_access_pending ON solution_access (guid) WHERE state = 'pending';
  ```
- Produces (`zam-store`, `impl SystemStore` в `access.rs`):
  ```rust
  pub struct SolutionInfo { pub guid: SolutionGuid, pub space_id: Id, pub name: String, pub owner_id: Id, pub created_at: Timestamp }
  pub enum AccessState { Pending, Approved, Rejected, Revoked }
  pub enum SolutionRole { Owner, Member(Access), Pending, Rejected, Revoked }
  pub enum AccessDecision { Approve(Access), Reject { reason: Option<String> } }
  pub struct AccessEntry { pub user_id: Id, pub user_name: String, pub access: Access, pub state: AccessState,
                           pub requested_at: Timestamp, pub decided_at: Option<Timestamp>, pub decided_by: Option<String> }
  pub async fn register_solution(&self, guid: &SolutionGuid, space_id: Id, name: &str, owner: Id) -> Result<(SolutionInfo, bool /*создано*/), StoreError>;
  pub async fn get_solution(&self, guid: &SolutionGuid) -> Result<Option<SolutionInfo>, StoreError>;
  pub async fn list_solutions(&self) -> Result<Vec<SolutionInfo>, StoreError>;
  pub async fn solution_role(&self, guid: &SolutionGuid, user: Id) -> Result<Option<SolutionRole>, StoreError>; // None — запроса нет
  pub async fn request_access(&self, guid: &SolutionGuid, user: Id, token: Option<Id>) -> Result<AccessState, StoreError>;
  pub async fn decide_access(&self, guid: &SolutionGuid, user: Id, d: AccessDecision, by: &str) -> Result<(), StoreError>;
  pub async fn revoke_access(&self, guid: &SolutionGuid, user: Id, by: &str) -> Result<(), StoreError>;
  pub async fn list_access(&self, guid: &SolutionGuid) -> Result<Vec<AccessEntry>, StoreError>;
  pub async fn accessible_solutions(&self, user: Id) -> Result<Vec<(SolutionInfo, SolutionRole /*Owner|Member*/)>, StoreError>;
  pub async fn pending_requests_for_owner(&self, owner: Id) -> Result<u64, StoreError>;
  ```
- **Переходы** (иначе `StoreError::Conflict`):
  - `pending → approved | rejected`;
  - `approved → revoked`, `approved → approved` (смена режима);
  - `rejected → approved`, `revoked → approved`.
  
  Повторный `request_access` в состоянии `rejected` или `revoked` состояние не меняет (без спама владельцу) **[решение исполнителя]**. Владелец не может быть участником (`BadInput`).
- **Аудит** — в той же транзакции `system.db`: `solution.register`, `solution.access.request`, `solution.access.approve` (`details: {access}`), `solution.access.reject`, `solution.access.revoke`, `solution.access.mode`.

- [ ] **Step 1: Тесты `zam-core`.**
  - `guid_canonical_forms`: верхний регистр → нижний; без дефисов, 32 символа, пустая строка → ошибка;
  - `tenant_key_roundtrip`: `parse_key(t.key()) == Some(t)` для обоих вариантов, `"x:1"` → `None`;
  - `target_ref_display_parse`.
- [ ] **Step 2: Тесты `tests/access.rs`.**
  - `register_is_idempotent_and_owner_fixed`: второй вызов с другим владельцем → `(info, false)`, владелец прежний;
  - `request_access_idempotent_pending`;
  - `roles_owner_member_pending` — `solution_role` для владельца, участника `rw`/`ro`, ожидающего, незнакомого (`None`);
  - `transition_matrix`: все допустимые переходы проходят, недопустимые (`pending → revoked`, `rejected → revoked`, approve владельца) — ошибка;
  - `rejected_not_reset_by_new_request`;
  - `audit_rows_written` — по строке на каждое действие, `details` без лишних полей;
  - `accessible_solutions_only_owner_and_approved`;
  - `pending_count_for_owner`;
  - `migration_v2_to_v3_keeps_data` (пользователи и токены на месте).
- [ ] **Step 3:** Реализация. `cargo test -p zam-core -p zam-store --no-fail-fast`, clippy, fmt.
- [ ] **Step 4:** Коммит `feat(store): реестр решений и доступ участников в system.db (§11.6)`.

### Task 3: Базы памяти решений — `MemoryDbs`, `solutions/<guid>.db`, атрибуция автора, перенос существующих решений

**Files:**
- Rename: `crates/zam-store/migrations/user/` → `migrations/memory/` (`git mv`), `src/user_db.rs` → `src/memory_db.rs`; `UserDbs` → `MemoryDbs`, `USER_MIGRATIONS` → `MEMORY_MIGRATIONS`; механическая правка всех вызовов в `zam-index`, `zam-server`, `xtask`
- Create: `crates/zam-store/migrations/memory/0007_attribution.sql`, `crates/zam-store/src/solution_move.rs`, `crates/zam-store/tests/{memory_db,solution_move}.rs`
- Modify: `crates/zam-store/src/{datadir.rs,blobs.rs,lib.rs}`, `src/nodes/{mod.rs,write.rs,revise.rs,tree_ops.rs}` (запись `user_id`), `list_spaces` (без `archived_at`)

**Interfaces:**
- Consumes: `SolutionGuid`, `Tenant`, `SystemStore::{register_solution, get_solution, request_access}` (задача 2).
- Схема `0007_attribution.sql`: `ALTER TABLE revisions ADD COLUMN user_id TEXT;` — `NULL` в базе пользователя означает «владелец базы». Ключ `meta.db_kind` (`user` | `solution`) ставится при открытии.
- Produces:
  ```rust
  pub enum DbKind { User, Solution }
  pub struct MemoryDbs { … }   // дёшево клонируется; ячейка на тенанта, как в UserDbs
  impl MemoryDbs {
      pub fn new(dir: &DataDir) -> MemoryDbs;
      pub async fn get(&self, t: &Tenant) -> Result<Db, StoreError>;      // решение без файла → NotFound (файл не создаётся)
      pub async fn user(&self, id: Id) -> Result<Db, StoreError>;         // = get(&Tenant::User(id)); фиксированные пространства
      pub async fn create_solution_db(&self, guid: &SolutionGuid, space_id: Id, name: &str) -> Result<Db, StoreError>; // идемпотентно: одно пространство 'solution'
      pub async fn wait_rechunk(&self, t: &Tenant);
  }
  impl DataDir { pub fn solutions_dir(&self) -> PathBuf; pub fn solution_db(&self, g: &SolutionGuid) -> PathBuf; pub fn blobs_dir_for(&self, t: &Tenant) -> PathBuf; }
  pub struct Attribution { /* поля M1 */ pub user_id: Option<Id> }
  pub struct MoveConflict { pub user: Id, pub guid: SolutionGuid, pub new_guid: SolutionGuid }
  pub struct MoveReport { pub moved: Vec<SolutionGuid>, pub conflicts: Vec<MoveConflict>, pub already: usize }
  pub async fn has_unmoved_solutions(db: &Db) -> Result<bool, StoreError>;
  pub async fn move_user_solutions(sys: &SystemStore, dbs: &MemoryDbs, dir: &DataDir, user: Id) -> Result<MoveReport, StoreError>;
  ```
- **Перенос** (одно решение; источник — пространство `solution` без отметки `meta.moved:<space_id>`):
  1. Целевая база `solutions/<guid>.db` создаётся (`create_solution_db` с тем же `space_id`). Затем в одной транзакции писателя цели выполняется `ATTACH <users/<id>.db> AS src` (только чтение) и `INSERT … SELECT` пространства, его `nodes`, `revisions` (`user_id = coalesce(user_id, <user>)`), `chunks` (FTS — триггерами), `attachments`, `blobs` и строк `embeddings`, на которые ссылаются `embed_key` перенесённых чанков или у которых `owner` — перенесённый узел. Если пространство уже есть в цели, копирование пропускается (повтор после сбоя).
  2. Проверка: число узлов, ревизий и чанков совпадает; blake3 от `group_concat(node_id‖rev‖content_hash ORDER BY node_id, rev)` совпадает. Иначе — `Corrupt`, источник не трогается.
  3. Файлы блобов копируются в `blobs/solutions/<guid>/` (жёсткая ссылка, при ошибке — копия). Если они уже есть и SHA-256 совпадает, файл пропускается.
  4. `register_solution(guid, space_id, name, user)`.
  5. Источник: `spaces.archived_at` и `meta.moved:<space_id> = <guid>`, удаление `index_jobs` перенесённых узлов. Данные источника остаются (удалит `doctor`, M7).
- **Конфликт.** GUID уже зарегистрирован с другим владельцем: решение переносится под новый `SolutionGuid::new_v7()` с именем «<имя> (перенос <пользователь>)», и для исходного GUID ставится `request_access` (pending). Аудит `solution.move_conflict`, WARN **[решение исполнителя; вопрос владельцу]**.
- **Где вызывается** (код — в задаче 4, здесь только функция): при старте сервера до `IndexRuntime::start` и в `zam-server migrate`. Ошибка одного пользователя — WARN, перенос повторится при следующем старте.
- **Windows MAX_PATH.** Самый глубокий файл — `solutions\<36>.db.bak-v<NN>-journal`, на 14 символов длиннее `users\<26>…`: `MAX_DATA_DIR_CHARS_WINDOWS` пересчитывается от него (190 → 176) **[решение исполнителя; вопрос владельцу]**.

- [ ] **Step 1: Тесты `tests/memory_db.rs`.**
  - `user_db_has_fixed_spaces_solution_db_only_solution_space`;
  - `solution_db_missing_is_not_found` (файл не создан);
  - `create_solution_db_idempotent`;
  - `revision_records_user_id`;
  - `archived_space_hidden_from_list_spaces`;
  - `windows_limit_covers_solutions_dir`: путь `<предел>\solutions\<guid>.db.bak-v99-journal` ≤ 259 символов UTF-16.
- [ ] **Step 2: Тесты `tests/solution_move.rs`.** База пользователя строится API хранилища M2: две заметки с ревизиями, вложение, эмбеддинги.
  - `move_copies_everything_and_verifies`: узлы, ревизии с текстом, чанки, FTS находит заметку в базе решения, эмбеддинги по `embed_key`, файл блоба в `blobs/solutions/<guid>/`, `refcount`, реестр с владельцем;
  - `move_is_idempotent` (второй вызов — `moved` пуст, `already = 1`);
  - `move_recovers_after_crash_before_mark` (шаги 1–4 выполнены, отметки нет → повтор только отмечает, число строк в цели прежнее);
  - `move_conflict_same_guid_two_users` (у второго — новый GUID, pending к исходному, аудит);
  - `verification_failure_leaves_source_intact` (искусственно испорченная копия → `Corrupt`, отметки нет).
- [ ] **Step 3:** Реализация и механическое переименование. `cargo test -p zam-store -p zam-index -p zam-server --no-fail-fast` — регрессия M2 зелёная. Clippy, fmt.
- [ ] **Step 4:** Коммит `feat(store): базы памяти решений solutions/<guid>.db, атрибуция user_id, перенос решений из баз пользователей`.

### Task 4: Сервер — доступ к решениям, `agent_solution_id`, API доступа, CLI, настройки пространств

**Files:**
- Modify: `crates/zam-server/src/api/{scope.rs,solutions.rs,spaces.rs,admin.rs,blobs.rs,render.rs,mod.rs}`, `api/nodes/{read,write,history}.rs`, `src/{app.rs,attrib.rs,error.rs,main.rs,bootstrap.rs}`
- Modify: `web/src/lib/api.ts` и места использования `solution_guid` (только переименование поля, чтобы e2e WebUI оставались зелёными)
- Create: `crates/zam-cli/src/cmd/solution.rs`; Modify: `crates/zam-cli/src/{main.rs,target.rs,client.rs}`, `cmd/{mod.rs,status.rs}`
- Test: `crates/zam-server/tests/solutions_access.rs`, `crates/zam-cli/tests/solution_access.rs`

**Interfaces:**
- Consumes: задачи 2 и 3.
- Produces (сервер):
  ```rust
  pub enum Role { Personal, Owner, Member(Access), Admin }
  pub struct MemScope { pub user: User /*кто действует*/, pub tenant: Tenant, pub db: Db, pub space: Space, pub role: Role }
  pub struct NodeScope { /* поля M1 */ pub tenant: Tenant, pub role: Role }
  pub async fn space_scope(st: &AppState, p: &Principal, h: &HeaderMap, raw: &str, mode: Mode) -> Result<MemScope, ApiError>;
  pub async fn node_scope(st: &AppState, p: &Principal, h: &HeaderMap, raw_id: &str, mode: Mode) -> Result<NodeScope, ApiError>;
  pub async fn readable_tenants(st: &AppState, p: &Principal, user: &User) -> Result<Vec<(Tenant, Role)>, ApiError>;
  pub fn effective_write(p: &Principal, role: Role) -> Result<(), ApiError>;   // токен rw И (Owner | Member(Rw) | Personal); Admin — всегда
  ApiError::SolutionAccess { guid: SolutionGuid, state: &'static str /*pending|rejected|revoked*/ }  // 403, type "/problems/solution-access"
  ```
  - `space_scope` для GUID решения: реестр → роль (каждый запрос читает `system.db`, без кэша) → `MemoryDbs::get(Tenant::Solution)`. GUID не зарегистрирован → 404.
  - `node_scope` ищет узел в личной базе, затем в решении из `X-Zam-Solution`, затем в остальных доступных решениях (чтение по PK в каждой базе).
  - Атрибуция получает `user_id` пользователя запроса. `RenderGenerations` ключуется тенантом.
- **REST:**
  - `POST /solutions {agent_solution_id | id, name}`:
    - перед регистрацией, если у пользователя есть неперенесённое решение (`has_unmoved_solutions`), вызывается `move_user_solutions`;
    - новый GUID → создаётся база решения, 201;
    - владелец или одобренный участник → 200 (если файла базы нет после сбоя — `create_solution_db` идемпотентно создаёт его);
    - чужое решение без запроса или с ожидающим запросом → 202 `{agent_solution_id, state: "pending"}`;
    - запрос отклонён или доступ отозван → 403 `solution-access`.
  - `GET /solutions` — доступные решения с ролью (администратору — все).
  - `GET /solutions/{guid}` — пространство, `agent_solution_id`, `owner {id, name}`, `role`, `access`.
  - `GET /solutions/{guid}/access` (владелец любым своим токеном или администратор) → `{owner, members: [AccessEntry], pending: n}`.
  - `POST /solutions/{guid}/access/{user}/approve {access?: "rw"|"ro"}` (по умолчанию `rw` **[решение исполнителя]**), `…/reject {reason?}`, `…/revoke`. Только `rw`-токен владельца или администратор, недопустимый переход → 409.
  - `GET /spaces`: личные пространства и доступные решения (у каждого — `agent_solution_id`, `role`, `shared`), поле `access_requests_pending` (для владельца).
  - `GET/PATCH /admin/settings` — пока только `rules.agent_write: bool`, аудит `settings.update`.
  - JSON-поле решения везде называется `agent_solution_id` (`Space` сериализует `solution_guid` под этим именем).
  - Гонка двух первых регистраций одного GUID: `register_solution` вернул чужого владельца → проигравший получает 202 `pending` (лишний файл базы не создаётся: база создаётся после успешной регистрации).
  - Вложения: `POST /blobs` пишет в базу и каталог тенанта узла (`blobs_dir_for`); `GET /blobs/{sha256}/{filename}` ищет блоб в личной базе, затем в доступных решениях.
- **Старт:** `app::finish` и `zam-server migrate` для каждого пользователя вызывают `move_user_solutions` до `IndexRuntime::start`. Итог переноса — в журнал (счётчики, без имён заметок).
- **CLI:**
  - `solution.toml` читается с ключом `agent_solution_id`, старый `id` — через `serde(alias)`;
  - `zam solution access list|requests|approve <user> [--ro]|reject <user> [--reason R]|revoke <user> [--solution GUID]` — пользователь по имени или id, решение по умолчанию — из `solution.toml`;
  - `zam status` печатает роль в решении и «ожидают подтверждения доступа: N (zam solution access requests)».

- [ ] **Step 1: Тесты `tests/solutions_access.rs`** (пользователи A — владелец, B, C; токены через API администратора):
  - `register_new_guid_creates_owner_solution_db` (201, файл есть, аудит);
  - `second_user_gets_pending_no_db_created` (202; дерево решения для B — 403 `state=pending`; у B своего решения с этим GUID нет);
  - `owner_approves_member_reads_writes_with_attribution` (ревизия с `user_id = B`);
  - `ro_member_and_ro_token_cannot_write` (оба → 403);
  - `revoke_takes_effect_on_next_request_same_token` (Review Focus 2: чтение узла → 403 сразу после отзыва);
  - `reject_then_repeat_request_stays_rejected`;
  - `only_owner_rw_or_admin_manage_access` (B одобряет C → 403; `ro`-токен владельца → 403);
  - `member_never_sees_owner_human_or_knowledge` (id узла Human и Knowledge владельца по токену B → 404);
  - `node_lookup_across_dbs` (`GET /nodes/{id}` находит узел базы решения без заголовка);
  - `admin_sees_and_manages_all`;
  - `settings_rules_agent_write_toggle` (после `PATCH` агент пишет в Rules);
  - `startup_moves_m2_solution` (каталог данных с решением в базе пользователя → после старта узлы читаются, `GET /spaces` показывает решение с ролью `owner`).
- [ ] **Step 2: Тесты CLI.**
  - `solution_toml_old_id_key_read` (unit);
  - `solution_access_flow`: B регистрирует → A `requests` видит B → `approve B --ro` → B читает, запись B → ошибка 403 с текстом.
- [ ] **Step 3:** Реализация, `cargo test -p zam-server -p zam-cli --no-fail-fast`, WebUI `check`, `build`, `e2e` (регрессия), clippy, fmt.
- [ ] **Step 4:** Коммит `feat(server): совместный доступ к решению — запрос, подтверждение, отзыв; agent_solution_id; настройки Rules`.

### Task 5: Индекс по тенантам — payload `schema 2`, воркер и reindex по базам решений

**Files:**
- Modify: `crates/zam-index/src/{vector.rs,qdrant.rs,worker.rs,reindex.rs,rollup.rs,edge.rs,lib.rs}`, `tests/{common/cases.rs,qdrant.rs,worker.rs}`
- Create: `crates/zam-store/migrations/system/0004_index_skipped_tenant.sql` (таблица с `tenant TEXT PRIMARY KEY`, перенос строк как `u:<user_id>`); Modify: `zam-store/src/system.rs` (`index_skip|unskip|skipped` по `Tenant`), `jobs.rs` (`NodePayload.tenant`)
- Modify: `crates/zam-server/src/{index_state.rs,reindex.rs,main.rs}`, `api/index.rs`, `api/nodes/write.rs` (`rollup_hint` по тенанту)
- Test: `crates/zam-server/tests/{index_worker.rs,index_api.rs}` (новые случаи)

**Interfaces:**
- Consumes: `Tenant`, `TargetKind`, `TargetRef` (задача 2), `MemoryDbs` (задача 3), `SystemStore::list_solutions`.
- Produces:
  ```rust
  pub const SCHEMA_VERSION: u32 = 2;
  pub struct PointPayload { pub tenant: Tenant, pub space_id: Id, pub space_kind: String, pub agent_solution_id: Option<SolutionGuid>,
      pub node_id: Option<Id>, pub entry_id: Option<Id>, pub path_ancestors: Vec<String>, pub area: Option<String>,
      pub level: String, pub kind: String /*note|folder|insight|changelog|release*/, pub insight_kind: Option<String>,
      pub status: String /*у заметок и папок — "active"*/, pub tags: Vec<String>, pub updated_at: i64, pub rev: i64, pub text_hash: String }
  pub struct VectorFilter { pub tenants: Vec<Tenant> /*пусто → пустая выдача*/, pub space_ids: Vec<Id>, pub path_prefix: Option<String>,
      pub levels: Vec<String>, pub kinds: Vec<String>, pub statuses: Vec<String>, pub insight_kinds: Vec<String>, pub area: Option<String> }
  pub struct VectorHit { pub chunk_id: String, pub tenant: Tenant, pub target: TargetRef, pub score: f32, pub level: String }
  pub struct IndexMeta { /* поля M2 */ pub schema: u32 }
  // VectorIndex: delete_nodes(user, …) заменяется на
  fn delete_targets<'a>(&'a self, t: &'a Tenant, ids: &'a [Id]) -> IndexFuture<'a, ()>;   // по node_id ИЛИ entry_id
  fn nearest_dense<'a>(&'a self, v: &'a [f32], f: &'a VectorFilter, limit: usize) -> IndexFuture<'a, Vec<VectorHit /*score = косинус*/>>;
  WorkerHandle::rollup_hint(&self, t: Tenant, folder: Id);
  ReindexRequest { tenant: Option<Tenant>, space: Option<Id>, full: bool }
  ```
- **Payload-индексы:** `tenant` (keyword, `is_tenant`), `space_id`, `node_id`, `entry_id`, `path_ancestors`, `level`, `kind`, `status`, `insight_kind`, `area`, `updated_at`. Индекс `user_id` снимается.
- **Коллекция `schema 1`** (M2) при старте удаляется и пересобирается из `embeddings` по всем тенантам без инференса (существующий путь «коллекция другой модели пересоздаётся»), аудит `index.schema_upgrade`.
- **Воркер:**
  - обходит тенантов: `list_users()` и `list_solutions()` по кругу;
  - пропуск неоткрывающейся базы и `rebuild_skipped` — по тенанту;
  - `index_jobs` с `target_kind = 'entry'` пропускаются с WARN до задачи 7.
- **`/index/status`:** токену — сумма по личной базе и доступным решениям плюс массив `solutions: [{agent_solution_id, queued, failed}]`; администратору — как в M2 плюс решения.
- **reindex:** `POST /admin/reindex {user?, solution?, space?, full?}`, `zam-server reindex [--user ID | --solution GUID] [--space S] [--full]`.

- [ ] **Step 1: Тесты `zam-index`** (настоящий Qdrant, общий макрос `vector_index_cases!`):
  - `tenant_filter_isolates_users_and_solutions`: точки `u:A`, `u:B`, `s:X`; фильтр `[u:A, s:X]` → ни одной точки `u:B`;
  - `status_kind_insight_kind_area_filters`;
  - `delete_targets_by_node_and_entry`;
  - `nearest_dense_returns_cosine` (точка сама с собой — 1 ± 1e-3);
  - `ensure_reports_schema`.
- [ ] **Step 2: Тесты сервера** (MiniLM + Qdrant):
  - `solution_notes_indexed_with_solution_tenant` (count по фильтру `s:<guid>` > 0, по `u:<владелец>` — только личные);
  - `schema1_collection_rebuilt_without_inference` (коллекция с `schema: 1` → после старта `schema 2`, точки на месте, счётчик `embed_passages` = 0) — Review Focus 1;
  - `reindex_cli_solution_flag`;
  - `index_status_token_sees_own_and_accessible_solutions`;
  - регрессия: Human не в Qdrant, `qdrant_lifecycle` зелёный.
- [ ] **Step 3:** Реализация. `cargo test -p zam-index -p zam-server --no-fail-fast`, `cargo clippy -p zam-index --features edge -- -D warnings`, fmt.
- [ ] **Step 4:** Коммит `feat(index): тенанты в Qdrant (schema 2), индексация и reindex баз решений`.

### Task 6: Поиск по нескольким базам памяти

**Files:**
- Split: `crates/zam-server/src/api/search.rs` → `api/search/{mod.rs (обработчик), targets.rs (область), merge.rs (слияние каналов FTS разных баз)}` — без смены поведения M2 в пределах одной базы
- Modify: `crates/zam-store/src/search.rs` (`FtsHit`), `crates/zam-search/src/*` (переименование `node_id` → `target`)
- Test: `crates/zam-server/tests/search_multi.rs`; unit-тесты `merge.rs`

**Interfaces:**
- Consumes: `readable_tenants`, `MemScope` (задача 4), `VectorFilter.tenants`, `VectorHit.tenant` (задача 5).
- Produces:
  ```rust
  // zam-store
  pub struct FtsHit { /* поля M2, node_id → */ pub target: TargetRef, /* … */ pub raw_score: f64 /*bm25 канала, меньше — лучше*/ }
  // zam-search: ChunkMeta.node_id / Ranked.node_id → target: TargetRef (группировка «≤ max_per_node» — по target)
  // server api/search/targets.rs
  pub struct SearchTarget { pub tenant: Tenant, pub db: Db, pub spaces: Vec<Space>, pub role: Role }
  pub async fn resolve_targets(st: &AppState, p: &Principal, h: &HeaderMap, sc: &ScopeReq) -> Result<(Vec<SearchTarget>, Vec<String> /*warnings*/), ApiError>;
  // merge.rs
  pub struct MergedChannels { pub bm25: Vec<(usize /*цель*/, FtsHit)>, pub trigram: Vec<(usize, FtsHit)>, pub relaxed: bool /*как FtsChannels::relaxed: хоть одна база ушла в ослабленный запрос*/ }
  pub fn merge_channels(per_db: Vec<(usize /*индекс цели*/, FtsChannels)>, top_k: usize) -> MergedChannels; // каждый канал — по raw_score, score = 1/место
  ```
- **Область:**
  - решение из `scope.solution` или `X-Zam-Solution` входит, только если роль `Owner` или `Member`; иначе оно исключается, а в `warnings` идёт `solution access pending|rejected|revoked`;
  - `all_solutions` — все доступные;
  - Knowledge и Rules — из личной базы;
  - Human — только администратору с `include_human`;
  - `scope.spaces` — ссылки разрешаются по личной базе и доступным решениям.
- **Каналы:**
  - FTS — параллельно по каждой базе, затем `merge_channels`;
  - вектор — один запрос с `tenants` всех целей без Human;
  - хиты направляются в базу своего тенанта для `live_chunks`. Все последующие чтения (листы, эмбеддинги, `expand`, `pending_count`) группируются по базе; `pending_count` суммируется.
- **Элемент выдачи** получает `agent_solution_id`, если он из решения. Формат `md` прежний.

- [ ] **Step 1: Тесты.**
  - unit `merge_orders_by_raw_score_across_dbs`, `merge_keeps_top_k_per_channel`;
  - `search_finds_in_personal_and_solution_dbs_together`;
  - `revoked_solution_excluded_with_warning` (Review Focus 2);
  - `member_search_sees_solution_and_own_knowledge_only`;
  - `degraded_fts_only_multi_db` (Qdrant остановлен);
  - `expand_section_and_neighbors_in_solution_db`;
  - регрессия `tests/{search,hybrid}.rs` без изменения ожиданий.
- [ ] **Step 2:** Реализация, `cargo test -p zam-store -p zam-search -p zam-server --no-fail-fast`, clippy, fmt.
- [ ] **Step 3:** Коммит `feat(search): поиск по личной памяти и базам решений одним запросом`.

### Task 7: Записи журналов — схема, хранилище, чанки, индексация, выдача в поиске

**Files:**
- Create: `crates/zam-store/migrations/memory/0008_entries.sql`, `crates/zam-store/src/entries/{mod.rs,write.rs,read.rs,chunks.rs}`, `crates/zam-store/tests/entries.rs`
- Modify: `crates/zam-store/src/{jobs.rs (target_kind 'entry', chunks_for_index записи), search.rs (хиты записей, фильтры), lib.rs}`, `crates/zam-index/src/worker.rs` (задания записей, payload), `crates/zam-server/src/{attrib.rs (X-Zam-Area), api/search/mod.rs}`
- Test: `crates/zam-server/tests/entries_index.rs`

**Interfaces:**
- Consumes: `TargetRef`, `Tenant` (задача 2), `Attribution.user_id` (задача 3), `PointPayload` (задача 5), `FtsHit.target` (задача 6).
- Схема `0008_entries.sql`:
  ```sql
  CREATE TABLE entries (
      id TEXT PRIMARY KEY, space_id TEXT NOT NULL REFERENCES spaces (id),
      kind TEXT NOT NULL CHECK (kind IN ('changelog', 'release', 'insight')),
      title TEXT NOT NULL, title_norm TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
      data TEXT NOT NULL DEFAULT '{}', tags TEXT NOT NULL DEFAULT '[]', area TEXT,
      status TEXT CHECK (status IN ('active', 'revisit', 'superseded', 'retracted')),  -- только insight
      supersedes_id TEXT REFERENCES entries (id), superseded_by_id TEXT REFERENCES entries (id),
      retract_reason TEXT, idempotency_key TEXT UNIQUE, current_rev INTEGER NOT NULL DEFAULT 1,
      author_kind TEXT NOT NULL CHECK (author_kind IN ('human', 'agent', 'system')),
      user_id TEXT, token_id TEXT, session_id TEXT, machine TEXT,
      index_state TEXT NOT NULL DEFAULT 'pending' CHECK (index_state IN ('pending','indexed','failed','excluded')),
      created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT,
      CHECK ((kind = 'insight') = (status IS NOT NULL))
  );
  CREATE INDEX entries_feed ON entries (space_id, kind, created_at) WHERE deleted_at IS NULL;
  CREATE INDEX entries_title ON entries (space_id, kind, title_norm) WHERE deleted_at IS NULL;
  CREATE TABLE entry_revisions (entry_id TEXT NOT NULL REFERENCES entries (id), rev INTEGER NOT NULL, title TEXT NOT NULL,
      body TEXT NOT NULL, data TEXT NOT NULL, tags TEXT NOT NULL, area TEXT, status TEXT, ts TEXT NOT NULL,
      author_kind TEXT NOT NULL, user_id TEXT, token_id TEXT, session_id TEXT, machine TEXT, PRIMARY KEY (entry_id, rev));
  -- + триггеры entry_revisions_no_update / _no_delete (как 0003)
  CREATE TABLE triggers (id TEXT PRIMARY KEY, entry_id TEXT NOT NULL REFERENCES entries (id),
      type TEXT NOT NULL CHECK (type IN ('file_glob', 'command_regex', 'error_regex', 'keyword')),
      pattern TEXT NOT NULL, tools TEXT, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL);
  CREATE INDEX triggers_entry ON triggers (entry_id);
  ```
- Produces:
  ```rust
  pub enum EntryKind { Changelog, Release, Insight }
  pub enum EntryStatus { Active, Revisit, Superseded, Retracted }
  pub struct AuthorView { pub kind: AuthorKind, pub user_id: Option<Id>, pub token_id: Option<Id>, pub session_id: Option<String>, pub machine: Option<String> }
  pub struct EntryRevision { pub rev: i64, pub title: String, pub body: String, pub data: serde_json::Value, pub tags: Vec<String>,
                             pub area: Option<String>, pub status: Option<EntryStatus>, pub ts: Timestamp, pub author: AuthorView }
  pub struct NewEntry { pub space_id: Id, pub kind: EntryKind, pub title: String, pub body: String, pub data: serde_json::Value,
                        pub area: Option<String>, pub tags: Vec<String>, pub status: Option<EntryStatus>, pub idempotency_key: Option<String> }
  pub struct Entry { pub id: Id, pub space_id: Id, pub kind: EntryKind, pub title: String, pub body: String, pub data: serde_json::Value,
                     pub tags: Vec<String>, pub area: Option<String>, pub status: Option<EntryStatus>, pub supersedes_id: Option<Id>,
                     pub superseded_by_id: Option<Id>, pub retract_reason: Option<String>, pub rev: i64, pub author: AuthorView,
                     pub index_state: IndexState, pub created_at: Timestamp, pub updated_at: Timestamp, pub deleted_at: Option<Timestamp> }
  pub struct EntryPatch { pub title: Option<String>, pub body: Option<String>, pub append_body: Option<String>, pub data: Option<serde_json::Value>,
                          pub tags: Option<Vec<String>>, pub area: Option<Option<String>>, pub status: Option<EntryStatus /*только Active|Revisit*/> }
  pub struct EntryFilter { pub space_id: Id, pub kind: Option<EntryKind>, pub statuses: Vec<EntryStatus>, pub area: Option<String>,
                           pub tag: Option<String>, pub since: Option<Timestamp>, pub until: Option<Timestamp>, pub cursor: Option<String>, pub limit: usize /*1..=200*/ }
  pub async fn create_entry(db: &Db, e: NewEntry, a: &Attribution) -> Result<(Entry, bool /*создана*/), StoreError>; // повтор Idempotency-Key → (та же, false)
  pub async fn get_entry(db: &Db, id: Id) -> Result<Entry, StoreError>;
  pub async fn list_entries(db: &Db, f: EntryFilter) -> Result<(Vec<Entry>, Option<String> /*курсор*/), StoreError>;
  pub async fn update_entry(db: &Db, id: Id, if_rev: Option<i64>, p: EntryPatch, a: &Attribution) -> Result<Entry, StoreError>;
  pub async fn delete_entry(db: &Db, id: Id, a: &Attribution) -> Result<(), StoreError>;
  pub async fn restore_entry(db: &Db, id: Id, a: &Attribution) -> Result<Entry, StoreError>;
  pub async fn entry_revisions(db: &Db, id: Id) -> Result<Vec<EntryRevision>, StoreError>;
  pub async fn supersede_entry(db: &Db, old: Id, new: NewEntry, a: &Attribution) -> Result<(Entry /*новая*/, Entry /*старая*/), StoreError>; // одна транзакция
  pub async fn retract_entry(db: &Db, id: Id, reason: &str, a: &Attribution) -> Result<Entry, StoreError>; // пустая причина → BadInput
  pub fn normalize_title(s: &str) -> String;  // NFKC-подобно: нижний регистр, пунктуация → пробел, пробелы схлопнуты
  ```
- **Чанки записи:** документ `# <title>\n\n<body>` режется `chunk_note`. `context_prefix` — `[<пространство>/<kind>] <title> | <теги>`; в FTS идут уровни `chunk` и `note`. Задание `upsert` с `target_kind = 'entry'` ставится в той же транзакции; удаление ставит `delete`; смена статуса — `upsert` (обновить payload).
- **Payload записи:** `entry_id`, `kind`, `insight_kind = data.kind` (для insight), `status` (у changelog и release — `active`), `area`.
- **Поиск:**
  - элемент записи: `entry_id`, `kind`, `status`, `insight_kind`, `title`;
  - карточка `md` по §9.3: `i:<id> insight/<kind> «<title>»  [score] <status> · <space> · ~N tok`, у changelog — `c:`, у release — `r:`;
  - `filters.kinds` (по умолчанию `note, insight, changelog, release`);
  - `filters.status` (по умолчанию `active, revisit`), `include_history: true` снимает фильтр статуса;
  - фильтр действует в Qdrant (payload) и в SQLite (`live_chunks`).
- **`X-Zam-Area`** (до 128 печатных символов) — область записи по умолчанию для узлов и записей, `Attribution.area`.

- [ ] **Step 1: Тесты `tests/entries.rs`.**
  - `create_entry_chunks_fts_and_job`;
  - `idempotency_key_returns_same_entry`;
  - `patch_creates_revision_and_enqueues`, `append_body_appends_with_blank_line`;
  - `entry_revisions_append_only` (UPDATE/DELETE → ошибка триггера);
  - `supersede_marks_old_links_new_one_tx`, `retract_requires_reason`, `superseded_cannot_return_to_active`;
  - `deleted_entry_not_in_fts_restore_returns`;
  - `list_entries_cursor_and_filters`;
  - `migration_v6_to_v8_keeps_data`.
- [ ] **Step 2: Тесты `tests/entries_index.rs`** (MiniLM + Qdrant; записи создаются вызовами хранилища, API — задачи 8–9):
  - `entry_indexed_with_payload` (count по `kind=insight`, `status=active`);
  - `search_returns_entry_items_with_kind_and_status`;
  - `default_excludes_superseded_and_retracted_include_history_returns` (оба канала);
  - `status_change_updates_payload`.
- [ ] **Step 3:** Реализация, `cargo test -p zam-store -p zam-index -p zam-server --no-fail-fast`, clippy, fmt.
- [ ] **Step 4:** Коммит `feat(store): записи журналов — хранение, ревизии, чанки, индексация и выдача в поиске`.

### Task 8: Changelog и Releases — REST, сравнение релизов, CLI

**Files:**
- Create: `crates/zam-server/src/api/{entries.rs,releases.rs}`, `crates/zam-cli/src/cmd/{log.rs,release.rs}`
- Modify: `crates/zam-server/src/api/mod.rs` (маршруты), `crates/zam-cli/src/main.rs`
- Test: `crates/zam-server/tests/journals.rs`, `crates/zam-cli/tests/journals.rs`

**Interfaces:**
- Consumes: задача 7 (хранилище записей), `space_scope`/`effective_write` (задача 4).
- **REST** (§12):
  - `GET /spaces/{space}/entries?kind=&status=&area=&tag=&since=&until=&cursor=&limit=`;
  - `POST /spaces/{space}/entries` с заголовком `Idempotency-Key`: 201, повтор — 200 с той же записью;
  - `GET/PATCH/DELETE /entries/{id}`: у `PATCH` необязательный `If-Match: <rev>`, расхождение → 409;
  - `POST /entries/{id}/restore`, `GET /entries/{id}/revisions`;
  - `GET /releases/{id}/compare?with=prev|<id>` → `{from: Entry|null, to: Entry, changelog: {feat: [...], fix: [...], …}, notes_diff: "<unified diff>", commits: [{sha, subject}]}`.
  
  Записи ищутся по id так же, как узлы (`node_scope`-аналог `entry_scope`: личная база, затем решения).
- **Проверки:**
  - changelog: `data.kind ∈ {feat, fix, refactor, docs, research, ops, other}`, `data.refs {commits, files, notes, issues}` — массивы строк до 200;
  - release: `data.version` (непустая, до 64 символов, уникальна в решении → 409), `data.date` (YYYY-MM-DD, по умолчанию — сегодня UTC), `data.git_tag?`, `data.commits?` (до 5000 `{sha, subject}`);
  - changelog и release — только в пространстве решения, иначе 422;
  - тело — тот же предел, что у заметки (`note_body_limit`).
- **Сравнение.**
  - Интервал — записи changelog с `created_at` в `(from.created_at, to.created_at]`.
  - У первого релиза `with=prev` берёт начало журнала (`from: null`) **[решение исполнителя]**.
  - Дифф — `unified_diff` тел заметок к релизам.
  - Коммиты — из `to.data.commits`. Сервер к git не обращается.
- **CLI:**
  - `zam log changelog --kind K --title T [--body-file F | stdin] [--ref commit:SHA|file:P|note:ID|issue:X]… [--area A]`;
  - `zam release add <version> [--date D] [--tag T] [--notes-file F] [--from-git [FROM..TO]]` — `git log --format=%H%x09%s FROM..TO` через `std::process::Command` в текущем каталоге; без диапазона — от предыдущего тега до `HEAD`;
  - `zam release compare <version|id> [--with prev|<version>]`.
  
  Вывод — Markdown, `--json` — как у сервера.

- [ ] **Step 1: Тесты сервера.**
  - `changelog_crud_idempotency_and_revisions`;
  - `changelog_kind_validated_422`;
  - `release_compare_groups_by_kind_and_diffs_notes`;
  - `compare_with_explicit_id_and_first_release`;
  - `release_version_unique_409`;
  - `changelog_outside_solution_422`;
  - `ro_member_cannot_log_403`;
  - `member_entries_attributed_to_member`.
- [ ] **Step 2: Тест CLI** `release_from_git_collects_commits`: git-репозиторий в `CARGO_TARGET_TMPDIR`, два тега, три коммита — в `data.commits` ровно 3.
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt.
- [ ] **Step 4:** Коммит `feat(journals): Changelog и Releases — API, сравнение релизов, zam log/release`.

### Task 9: Insights — статусы, замещение, отзыв, дедупликация, триггеры, поправки поиска

**Files:**
- Create: `crates/zam-server/src/api/{insights.rs,triggers.rs}`, `crates/zam-store/src/entries/triggers.rs`, `crates/zam-cli/src/cmd/insight.rs`
- Modify: `crates/zam-search/src/adjust.rs` (бусты), `crates/zam-server/src/{config.rs ([search] boost_*, [insights]), api/search/mod.rs, api/mod.rs}`, `crates/zam-store/src/search.rs` (`FtsQuery.area`), `deploy/config.example.toml`
- Test: `crates/zam-server/tests/insights.rs`, unit-тесты `adjust.rs` и `triggers.rs`, `crates/zam-cli/tests/insights.rs`

**Interfaces:**
- Consumes: задачи 5 (`nearest_dense`), 7, 8 (`entry_scope`, маршруты записей).
- Produces:
  ```rust
  pub struct NewTrigger { pub kind: TriggerKind, pub pattern: String, pub tools: Option<Vec<String>> }
  pub enum TriggerKind { FileGlob, CommandRegex, ErrorRegex, Keyword }
  pub struct TriggerRow { pub id: Id, pub entry_id: Id, pub kind: TriggerKind, pub pattern: String, pub tools: Option<Vec<String>>,
                          pub title: String, pub insight_kind: String, pub space_id: Id }
  pub fn validate_trigger(t: &NewTrigger) -> Result<(), String>; // globset::Glob; regex::RegexBuilder size_limit 1 МиБ; keyword 1..=200; pattern ≤ 1000; tools ≤ 16
  pub async fn set_triggers(db: &Db, entry: Id, ts: Vec<NewTrigger>) -> Result<(), StoreError>; // ≤ 32 на запись, замена набора в транзакции записи
  pub async fn active_triggers(db: &Db, space_ids: &[Id]) -> Result<Vec<TriggerRow>, StoreError>; // только active|revisit, живые
  pub struct DedupCandidate { pub id: Id, pub title: String, pub status: EntryStatus, pub reason: &'static str /*title|vector*/, pub score: Option<f32> }
  // zam-search
  pub struct Boosts { pub kinds: HashMap<String, f64> /*insight_kind → множитель*/, pub area: Option<(String, f64)> }
  pub fn apply_boosts(r: &mut [Ranked], meta: &HashMap<String, ChunkMeta>, b: &Boosts);
  ```
- **Создание insight** (`POST /spaces/{space}/entries` с `kind: insight` или `zam log insight`). Тело — по §7.5: `data.kind`, `scope`, `status` (по умолчанию `active`), `triggers`, `tags`, `data.experiment`, `data.evidence`, `data.confidence`, `force`, `supersedes_id`.
  1. `scope: global` → запись идёт в Knowledge пользователя запроса, какое бы пространство ни стояло в пути (§7.5).
  2. Дедупликация (если нет `force` и `supersedes_id`):
     - совпадение `title_norm` с `active`/`revisit` той же области (`area`; `NULL` совпадает с `NULL`);
     - при доступном векторном канале — `embed_query(title + "\n" + body)` (первые 512 токенов) → `nearest_dense(tenants = [тенант пространства], kinds = [insight], statuses = [active, revisit], area)`, максимум косинуса по записи ≥ `insights.dedup_threshold` (0,90);
     - есть кандидаты → 409 `/problems/possible-duplicate` с `candidates: [DedupCandidate]` (до 5) и `dedup: "full" | "degraded"`;
     - без модели — только заголовок и `dedup: "degraded"`, в том числе в успешном ответе.
  3. `supersedes_id` → `supersede_entry`.
  4. «Дописать к существующему» — `PATCH /entries/{id} {append_body}`.
- **Операции:**
  - `POST /entries/{id}/supersede {новая запись}`;
  - `POST /entries/{id}/retract {reason}` (пустая причина → 422);
  - `PATCH {status: "revisit"|"active"}`; переход из `superseded`/`retracted` → 422 **[решение исполнителя]**;
  - `PATCH {triggers}` заменяет набор триггеров.
- **`GET /triggers?solution=<guid>`** — триггеры `active`/`revisit` решения (если оно доступно) и Knowledge пользователя: `{etag, triggers: [{id, entry_id, type, pattern, tools, title, insight_kind, space}]}`. `ETag` — blake3 канонического JSON, `If-None-Match` → 304. Сопоставление триггеров и кэш в CLI — M4.
- **Поправки поиска (§9.2 п. 4):**
  - `search.boost_gotcha = 1.15`, `search.boost_failure = 1.15`, `search.boost_area = 1.10` — множители слитого балла; текущая область — из `X-Zam-Area`;
  - `scope.area` — строгий фильтр (Qdrant `area`, FTS `FtsQuery.area`);
  - значения — **[решение исполнителя]**, проверяются сеткой eval в задаче 15.
- **`POST /entries/{id}/feedback`** и таблица `usage` — этап M4 (§10.5), здесь не делаются.
- **CLI:**
  - `zam log insight --kind K --title T [--scope solution|global] [--trigger file_glob:**/*.sh]… [--tag]… [--force] [--supersedes ID] < body.md`; ответ 409 печатает кандидатов и подсказки (`--force`, `--supersedes`, `zam insight append`);
  - `zam insight list [--status] [--kind]|show <id>|supersede <id> …|retract <id> --reason R|revisit <id>|append <id> < text`.

- [ ] **Step 1: Unit-тесты.**
  - `validate_trigger_rejects_bad_glob_regex_and_lengths`;
  - `regex_without_backrefs_is_only_dialect` (`(a)\1` → ошибка);
  - `boost_gotcha_orders_above_equal_note`;
  - `area_boost_multiplies_only_matching`;
  - `normalize_title_cases` (регистр, пунктуация, ё/е не сливаются).
- [ ] **Step 2: Тесты сервера** (MiniLM + Qdrant):
  - `dedup_title_409_with_candidates`;
  - `dedup_vector_409` (перефраз того же текста);
  - `dedup_degraded_without_model`;
  - `force_writes_duplicate`;
  - `append_body_to_existing`;
  - `supersede_old_superseded_new_linked_history_kept`;
  - `retract_requires_reason_422`;
  - `revisit_listed_and_searchable`;
  - `invalid_status_transition_422`;
  - `global_scope_goes_to_knowledge`;
  - `triggers_validation_422_with_index`;
  - `triggers_etag_304_and_changes_after_edit`;
  - `triggers_exclude_superseded_and_retracted`;
  - `strict_area_filter`.
- [ ] **Step 3: Тест CLI** `insight_log_with_triggers_dedup_and_supersede`.
- [ ] **Step 4:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(journals): Insights — статусы, замещение, отзыв, дедупликация, триггеры, бусты поиска`.

### Task 10: Граф — `edges`, wiki-ссылки, связи frontmatter, API графа, `expand=graph`

**Files:**
- Create: `crates/zam-store/migrations/memory/0009_edges.sql`, `crates/zam-store/src/{graph.rs,links.rs}`, `crates/zam-store/tests/graph.rs`, `crates/zam-server/src/api/graph.rs`, `crates/zam-cli/src/cmd/link.rs`
- Modify: `crates/zam-markdown/src/{frontmatter.rs,doc.rs}` (`Frontmatter.relations`), `crates/zam-store/src/nodes/{chunks.rs,write.rs,revise.rs,tree_ops.rs}` и `entries/write.rs` (рёбра в транзакции записи), `crates/zam-server/src/api/{render.rs (общий резолвер),search/mod.rs (expand=graph),insights.rs (links, system supersedes)}`
- Test: `crates/zam-markdown/tests/parse.rs` (relations), `crates/zam-server/tests/graph.rs`

**Interfaces:**
- Consumes: `TargetRef` (задача 2), записи (задача 7), `supersede_entry` (задача 7).
- Схема `0009_edges.sql`:
  ```sql
  CREATE TABLE edges (
      id TEXT PRIMARY KEY, space_id TEXT NOT NULL,
      src_kind TEXT NOT NULL CHECK (src_kind IN ('node', 'entry')), src_id TEXT NOT NULL,
      dst_kind TEXT CHECK (dst_kind IN ('node', 'entry')), dst_id TEXT, dst_raw TEXT, dst_anchor TEXT,
      type TEXT NOT NULL CHECK (type IN ('links_to','relates_to','depends_on','part_of','caused_by','fixes','supersedes','derived_from','mentions')),
      origin TEXT NOT NULL CHECK (origin IN ('wikilink', 'frontmatter', 'explicit', 'system')),
      user_id TEXT, token_id TEXT, created_at TEXT NOT NULL, deleted_at TEXT,
      CHECK (dst_id IS NOT NULL OR dst_raw IS NOT NULL)
  );
  CREATE INDEX edges_src ON edges (src_kind, src_id) WHERE deleted_at IS NULL;
  CREATE INDEX edges_dst ON edges (dst_kind, dst_id) WHERE deleted_at IS NULL;
  CREATE INDEX edges_unresolved ON edges (space_id, dst_raw) WHERE dst_id IS NULL AND deleted_at IS NULL;
  CREATE UNIQUE INDEX edges_explicit ON edges (src_kind, src_id, dst_kind, dst_id, type)
      WHERE origin IN ('explicit', 'system') AND deleted_at IS NULL;
  INSERT INTO meta (key, value) VALUES ('edges_backfill', 'pending');
  ```
- Produces:
  ```rust
  pub enum EdgeType { LinksTo, RelatesTo, DependsOn, PartOf, CausedBy, Fixes, Supersedes, DerivedFrom, Mentions }
  pub enum EdgeOrigin { Wikilink, Frontmatter, Explicit, System }
  pub struct EdgeView { pub id: Id, pub src: TargetRef, pub dst: Option<TargetRef>, pub dst_raw: Option<String>, pub dst_anchor: Option<String>,
                        pub kind: EdgeType, pub origin: EdgeOrigin, pub created_at: Timestamp }
  pub struct GraphNode { pub r: TargetRef, pub title: String, pub path: Option<String>, pub status: Option<EntryStatus>, pub deleted: bool }
  pub struct Graph { pub nodes: Vec<GraphNode>, pub edges: Vec<EdgeView> }
  pub struct NewEdge { pub src: TargetRef, pub dst: TargetRef, pub kind: EdgeType }
  pub trait GraphStore: Send + Sync {          // §8.5: хранилище графа заменяемо
      fn neighbors(&self, start: &[TargetRef], depth: u8 /*1..=3*/, types: &[EdgeType]) -> impl Future<Output = Result<Graph, StoreError>> + Send;
      fn backlinks(&self, target: TargetRef) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;   // живые источники
      fn outgoing(&self, src: TargetRef) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;      // с неразрешёнными
      fn add_edge(&self, e: NewEdge, a: &Attribution) -> impl Future<Output = Result<EdgeView, StoreError>> + Send; // origin explicit
      fn remove_edge(&self, id: Id, a: &Attribution) -> impl Future<Output = Result<(), StoreError>> + Send;       // только explicit
  }
  impl GraphStore for Db;
  // links.rs — одно разрешение для рендера и рёбер
  pub fn resolve_link(c: &Connection, space: Id, from_path: &str, link: &Link) -> rusqlite::Result<Option<Id>>;
  ```
- **Правила:**
  - Вики-ссылка `[[Имя#Якорь|текст]]`: сначала путь, затем `find_by_name` — первая по `path`, ровно как сейчас в `api/render.rs::resolve_one`. Логика переносится в `zam-store::links`, рендер вызывает её же.
  - Markdown-ссылка на относительный путь заметки — тоже `links_to`; внешние URL рёбер не дают.
  - Неразрешённая ссылка → `dst_raw`.
  - `relations:` во frontmatter — список строк `"<тип>: <цель>"` (flow или block), цель — `[[Имя]]` или путь в пространстве **[решение исполнителя]**. Неизвестный тип пропускается, в ответе записи — `warnings: ["unknown relation type: <тип>"]`.
  - Рёбра `wikilink`/`frontmatter` пересоздаются при каждой записи узла (DELETE + INSERT — они выводимы из ревизии). `explicit` и `system` удаляются мягко.
  - Создание, переименование, перенос и восстановление узла разрешают неразрешённые рёбра пространства с подходящим `dst_raw`. Удаление узла рёбра на него не трогает; `backlinks` возвращает только живые источники.
  - Insight: поле `links: [{type, target: "node:<id>"|"entry:<id>"|"[[Имя]]"}]` → `explicit`; `supersede_entry` создаёт `system supersedes` (новая → старая).
  - Межбазовая цель (узел личной памяти из записи решения и наоборот) → 422 «связи между базами — не в M3» **[решение исполнителя]**.
  - **Дозаполнение:** при открытии базы с `edges_backfill = pending` фоновая задача (по образцу перечанковки R1) строит рёбра всех живых заметок и записей и `system supersedes` из `entries.supersedes_id`, затем снимает флаг.
- **API:**
  - `GET /graph?node=<id>|entry=<id>&depth=1..3&types=a,b` → `Graph`;
  - `POST /edges {src, type, dst}` (только `explicit`, `supersedes` → 422);
  - `DELETE /edges/{id}` (только `explicit`);
  - `GET /nodes/{id}/backlinks`, `GET /entries/{id}/backlinks`, `GET /nodes/{id}/links` (исходящие и неразрешённые).
- **`expand=graph`** (снимается 400 M2):
  - соседи первых `top_k` элементов на 1 шаг в обе стороны;
  - до `ceil(top_k / 2)` новых элементов с баллом `родитель × search.graph_weight` (0,5);
  - сначала `active`/`revisit` insights, затем заметки; `superseded`/`retracted` — только с `include_history`;
  - `sources: ["graph"]`, `via: {type, from}`; текст в `chunks` — L0 и первый лист.
- **CLI:** `zam link <src> <type> <dst>`, `zam link rm <edge-id>`, `zam links <id>` (исходящие и backlinks).

- [ ] **Step 1: Тесты `zam-markdown`:** `frontmatter_relations_flow_and_block`, `relations_unknown_type_kept_raw`.
- [ ] **Step 2: Тесты `tests/graph.rs`** (хранилище):
  - `wikilinks_create_resolved_and_unresolved`;
  - `unresolved_resolves_when_target_created`;
  - `rename_and_move_reresolve`;
  - `same_name_resolves_first_by_path_like_render` (Review Focus 5);
  - `rewrite_replaces_derived_keeps_explicit`;
  - `delete_source_hides_backlinks_restore_returns`;
  - `neighbors_depth2_cte_no_cycles_blowup` (цикл A→B→A);
  - `supersede_creates_system_edge`;
  - `backfill_existing_notes_on_open`.
- [ ] **Step 3: Тесты сервера:**
  - `graph_api_and_backlinks`;
  - `explicit_edge_crud_system_protected`;
  - `cross_db_edge_422`;
  - `expand_graph_adds_neighbors_with_graph_source`;
  - `render_and_edges_agree_on_resolution`;
  - `member_graph_limited_to_accessible_db`.
- [ ] **Step 4:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(graph): рёбра, wiki-ссылки и relations, API графа, expand=graph`.

### Task 11: Сканер секретов перед индексацией (§7.6)

**Files:**
- Create: `crates/zam-markdown/src/secrets.rs`, `crates/zam-markdown/tests/secrets.rs`
- Modify: `crates/zam-markdown/src/lib.rs`, `crates/zam-store/src/nodes/chunks.rs` и `entries/chunks.rs` (маскирование текста чанков), ответы записи в `crates/zam-server/src/api/{nodes/write.rs,entries.rs,insights.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub const SECRET_MARK: &str = "‹secret›";
  pub enum SecretKind { KnownPrefix(&'static str), PrivateKey, KeyValue, UrlPassword, Jwt, HighEntropy }
  pub struct SecretHit { pub kind: SecretKind, pub range: Range<usize>, pub line: usize /*с 1*/ }
  pub fn scan_secrets(text: &str) -> Vec<SecretHit>;            // не пересекаются, по порядку
  pub fn mask_secrets(text: &str, hits: &[SecretHit]) -> String; // каждый диапазон → SECRET_MARK
  ```
- **Правила** (регулярные выражения `regex`, компилируются один раз):
  - Известные префиксы:
    - `AKIA|ASIA[0-9A-Z]{16}`;
    - `gh[pousr]_[A-Za-z0-9]{36}`, `github_pat_[A-Za-z0-9_]{60,}`;
    - `glpat-[A-Za-z0-9_-]{20,}`;
    - `xox[abprs]-[A-Za-z0-9-]{10,}`;
    - `sk-(ant-)?[A-Za-z0-9_-]{20,}`;
    - `AIza[0-9A-Za-z_-]{35}`;
    - токены ZeroAIMemory по формату `zam_core::token`.
  - JWT: `eyJ…\.…\.…` — три части от 10 символов.
  - Блок `-----BEGIN [A-Z ]*PRIVATE KEY-----` … `-----END … -----` целиком.
  - Ключ-значение: `(?i)(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret)\s*[:=]\s*["']?([^\s"'`]{6,})`. Маскируется только значение. Не маскируются значения в `<…>`, `${…}`, `{{…}}`, из одних `*` или `x`, а также `changeme` и `example`.
  - Пароль в URL: `scheme://user:<пароль>@`.
  - Высокая энтропия: слово `[A-Za-z0-9+/_=-]{32,}`, в котором есть строчные, заглавные и цифры, с энтропией Шеннона ≥ 4,5 бит/символ. Только hex, только цифры, UUID и base32-id Crockford (26 символов) этим правилом не помечаются никогда (Review Focus 3).
- **Где действует:**
  - текст чанков (FTS и эмбеддинг) заметок и записей маскируется при нарезке;
  - исходник заметки и записи не меняется;
  - ответ записи: `warnings: ["secrets_detected"]` и `secrets_detected: [{kind, line}]` — без значений, с подсказкой перенести значение в Vault (`zam vault set`).
- **Бюджет:** ≤ 2 мс на 100 КБ текста (release).

- [ ] **Step 1: Тесты `tests/secrets.rs`.**
  - `detects_each_rule` (по положительному примеру на правило);
  - `ignores_identifiers` (git SHA 40 hex, sha256 64 hex, UUID, `Id` 26 символов, URL с query, `password=<your-password>`, `token: ${TOKEN}`);
  - `masks_only_value_in_key_value`;
  - `private_key_block_whole`;
  - property (proptest, 500 случаев): нет паники, байты вне диапазонов совпадают, диапазоны на границах UTF-8;
  - `#[ignore] bench_scan_100kb` (отчёт времени).
- [ ] **Step 2: Тесты сервера:**
  - `secret_in_note_not_in_fts_nor_qdrant_text` (FTS по значению — 0 хитов, по `‹secret›` — есть; payload и текст чанка без значения);
  - `note_read_returns_original`;
  - `write_response_has_secrets_detected`;
  - `entry_body_secret_masked`.
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(markdown): сканер секретов перед индексацией, предупреждение secrets_detected`.

### Task 12: Vault — шифрование, ключи, REST, аудит, CLI

**Files:**
- Create: `crates/zam-store/migrations/system/0005_vault_keys.sql`, `crates/zam-store/migrations/memory/0010_vault.sql`, `crates/zam-store/src/vault.rs` (хранение без криптографии), `crates/zam-server/src/vault/{mod.rs,keys.rs,crypto.rs}`, `crates/zam-server/src/api/vault.rs`, `crates/zam-cli/src/cmd/vault.rs`
- Modify: `crates/zam-server/src/{app.rs,config.rs ([vault] master_key_file),error.rs (ApiError::VaultLocked → 503 /problems/vault-locked),api/mod.rs}`, корневой `Cargo.toml` (`chacha20poly1305`, `zeroize`), `deploy/config.example.toml`
- Test: `crates/zam-server/tests/vault.rs`, unit-тесты `crypto.rs`/`keys.rs`, `crates/zam-cli/tests/vault.rs`

**Interfaces:**
- Consumes: `space_scope`, `Role` (задача 4), `SystemStore::audit`.
- Схемы:
  ```sql
  -- system 0005
  CREATE TABLE user_keys (user_id TEXT PRIMARY KEY REFERENCES users (id), wrapped BLOB NOT NULL, created_at TEXT NOT NULL);
  -- settings: 'vault.key_check' — зашифрованная мастер-ключом известная строка
  -- memory 0010 (используется только в базах пользователей)
  CREATE TABLE vault_items (
      id TEXT PRIMARY KEY, scope TEXT NOT NULL CHECK (scope IN ('knowledge', 'solution')), agent_solution_id TEXT,
      title TEXT NOT NULL, data TEXT NOT NULL DEFAULT '{}', secret BLOB NOT NULL, current_rev INTEGER NOT NULL DEFAULT 1,
      author_kind TEXT NOT NULL, user_id TEXT, token_id TEXT, session_id TEXT, machine TEXT,
      created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT,
      CHECK ((scope = 'solution') = (agent_solution_id IS NOT NULL))
  );
  CREATE UNIQUE INDEX vault_live_title ON vault_items (scope, coalesce(agent_solution_id, ''), title) WHERE deleted_at IS NULL;
  CREATE TABLE vault_revisions (item_id TEXT NOT NULL REFERENCES vault_items (id), rev INTEGER NOT NULL, title TEXT NOT NULL,
      data TEXT NOT NULL, secret BLOB NOT NULL, ts TEXT NOT NULL, author_kind TEXT NOT NULL, user_id TEXT, token_id TEXT,
      session_id TEXT, machine TEXT, PRIMARY KEY (item_id, rev));   -- + append-only триггеры
  ```
- Produces:
  ```rust
  pub struct VaultKeys { … }   // в AppState; мастер-ключ и кэш ключей пользователей — Zeroizing<[u8; 32]>
  pub enum VaultState { Ready, Locked { reason: String } }
  impl VaultKeys {
      pub fn load(cfg: &ServerConfig, sys: &SystemStore) -> impl Future<Output = Result<VaultKeys, anyhow::Error>>; // Locked — не ошибка старта
      pub fn state(&self) -> &VaultState;
      pub async fn encrypt(&self, user: Id, item: Id, rev: i64, plain: &[u8]) -> Result<Vec<u8>, ApiError>;
      pub async fn decrypt(&self, user: Id, item: Id, rev: i64, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, ApiError>;
  }
  // zam-store vault.rs
  pub enum VaultScope { Knowledge, Solution(SolutionGuid) }
  pub struct VaultMeta { pub id: Id, pub scope: VaultScope, pub title: String, pub data: serde_json::Value, pub rev: i64, pub author: AuthorView,
                         pub created_at: Timestamp, pub updated_at: Timestamp }
  pub async fn vault_list(db: &Db, scope: &VaultScope, q: Option<&str>) -> Result<Vec<VaultMeta>, StoreError>; // по title, data.url, data.description
  pub async fn vault_get(db: &Db, id: Id) -> Result<(VaultMeta, Vec<u8> /*secret blob*/), StoreError>;
  // id записи создаёт сервер (Id::new()) и шифрует секрет с (user, id, rev = 1) до вызова — шифрование асинхронное и вне хранилища
  pub async fn vault_put(db: &Db, id: Id, scope: &VaultScope, title: &str, data: serde_json::Value, secret_blob: Vec<u8>, a: &Attribution) -> Result<VaultMeta, StoreError>;
  // новая ревизия = expected_rev + 1 (сервер шифрует под неё); ревизия сменилась между чтением и записью → Conflict (повтор на сервере)
  pub async fn vault_update(db: &Db, id: Id, expected_rev: i64, title: Option<String>, data: Option<serde_json::Value>, secret_blob: Option<Vec<u8>>, a: &Attribution) -> Result<VaultMeta, StoreError>;
  pub async fn vault_delete(db: &Db, id: Id, a: &Attribution) -> Result<(), StoreError>;
  ```
- **Криптография.**
  - XChaCha20-Poly1305. Блоб секрета — `0x01 ‖ nonce(24) ‖ ciphertext‖tag`, AAD — `b"zam-vault-v1" ‖ user_id ‖ item_id ‖ rev`: блоб не переносится между записями и ревизиями.
  - Ключ пользователя — 32 случайных байта, обёрнут мастер-ключом с AAD `b"zam-ukey-v1" ‖ user_id`, создаётся при первом секрете.
  - `vault.key_check` — шифротекст `b"zam-key-check"`.
- **Мастер-ключ:**
  - `ZAM_MASTER_KEY` (base64, 32 байта), иначе файл `[vault] master_key_file` (по умолчанию `<data_dir>/master.key`, base64);
  - файл создаётся только при первом запуске, когда нет ни `user_keys`, ни `key_check`; на Linux права 0600, на Windows — ACL каталога данных (оговорено в документации);
  - ключ не прошёл `key_check`, или файла нет при существующих ключах → `Locked` с причиной, WARN одной строкой, Vault отвечает 503; остальной сервер работает (Review Focus 4).
- **REST (§12):**
  - `GET /spaces/{space}/vault?q=` — только метаданные. `space` = `knowledge` или GUID доступного решения: свои записи пользователя запроса, привязанные к решению; записи других участников не видны никому, кроме администратора с `X-Zam-User`. `rules` и `human` → 404.
  - `POST /spaces/{space}/vault {title, data: {type, username?, url?, description?}, secret}` (только `rw`): 201, занятое имя → 409.
  - `GET /vault/{id}` — метаданные; `GET /vault/{id}?reveal=1` — `{…, secret}` и аудит `vault.reveal` (`details: {item, session, machine}`, `actor` — id токена).
  - `PATCH /vault/{id}`: новый `secret` → новая ревизия.
  - `DELETE /vault/{id}` — мягко, аудит `vault.delete`.
  - `GET /vault/{id}/revisions` — метаданные; раскрытие старой ревизии `?reveal=1&rev=N` — только администратору.
  - `data.type ∈ {password, token, ssh_key, connection_string, url, repo, note}`.
- **CLI:**
  - `zam vault list [--q Q] [--global]`;
  - `zam vault get <имя> [--global]` — в stdout только значение, без перевода строки при `--raw`;
  - `zam vault set <имя> --type T [--username U] [--url URL] [--description D] [--global]` — значение только из stdin или скрытого ввода, никогда из аргументов.

- [ ] **Step 1: Unit-тесты.**
  - `encrypt_decrypt_roundtrip`;
  - `aad_binding_swap_blob_fails` (чужой `item_id` или `rev` → ошибка);
  - `wrapped_user_key_roundtrip`;
  - `master_key_created_once_with_0600_on_linux`;
  - `env_master_key_has_priority`.
- [ ] **Step 2: Тесты сервера:**
  - `agent_sets_and_reveals_secret_audited` (в аудите есть сессия и машина, значения нет);
  - `vault_list_never_returns_values`;
  - `vault_values_not_in_fts_qdrant_logs` (FTS и поиск по значению — 0; count Qdrant без точек vault; в файле журнала сервера и в байтах `users/<id>.db` значения нет);
  - `wrong_master_key_locks_vault_server_works` (503 `vault-locked`, заметки и поиск работают);
  - `missing_key_file_with_existing_keys_no_regen` (новый `master.key` не создан);
  - `participant_vault_isolated_in_shared_solution` (A и B в одном решении: списки разные, `GET /vault/{id_A}` по токену B → 404);
  - `ro_token_cannot_set_403`;
  - `title_unique_per_scope_409`;
  - `copy_without_master_key_reveals_nothing` (каталог данных скопирован без `master.key` → Vault заблокирован; в файлах копии значения нет).
- [ ] **Step 3: Тест CLI** `vault_set_from_stdin_get_raw`. Реализация, тесты (`--no-fail-fast`), clippy, fmt; аудит лицензий `chacha20poly1305` и `zeroize` — в журнал.
- [ ] **Step 4:** Коммит `feat(vault): шифрование XChaCha20-Poly1305, мастер-ключ, API и аудит раскрытия, zam vault`.

### Task 13: WebUI — журналы, Vault, связи, совместный доступ, настройки

**Files:**
- Create: `web/src/components/{ChangelogView,ReleasesView,InsightsView,VaultView,Links}.svelte`, `web/src/routes/{Solutions,Settings}.svelte`, `web/e2e/{journals,access}.spec.ts`
- Modify: `web/src/lib/{api.ts,router.ts}`, `web/src/components/{Tree,DocView,Search}.svelte`, `web/src/routes/Shell.svelte`, `web/e2e/seed.ts`

**Interfaces:**
- Consumes: REST задач 4, 7–10 и 12.
- **Поведение:**
  - **Дерево:** решения из `GET /spaces` с меткой роли (`владелец`, `участник rw/ro`, `общее`); у решения сначала закреплённые виртуальные узлы `Changelog`, `Releases`, `Insights`, `Vault`, затем дерево. У Knowledge — `Insights` и `Vault`.
  - **Changelog:** лента с фильтром по `kind` и области, подгрузка курсором.
  - **Releases:** список и сравнение двух релизов: группы changelog, дифф заметок (существующий `DiffView`), коммиты.
  - **Insights:** таблица с фильтрами (вид, статус, область, теги), действия «Заменить», «Отозвать» (с причиной), «Пересмотреть», правка триггеров с ошибками проверки сервера; цепочка замещения со ссылками.
  - **Vault:** значения скрыты маской, «Показать» вызывает `?reveal=1`, подсказка «раскрытие записывается в аудит»; при `vault-locked` — баннер с причиной и ссылкой на раздел документации о мастер-ключе.
  - **Вкладка «Связи» документа:** исходящие (с неразрешёнными) и обратные ссылки списком; визуальный граф — M7.
  - **Администрирование → «Решения»:** реестр, владелец, участники с режимом и состоянием, входящие запросы — одобрить (`rw`/`ro`), отклонить (с причиной), отозвать.
  - **Администрирование → «Настройки»:** `rules.agent_write`.
  - **Поиск Ctrl+K:** карточки записей (`insight`, `changelog`, `release`) и статус; предупреждения о доступе к решению.
- **Бюджет:** начальный бандл ≤ 300 КБ gzip (`size-limit`), новых npm-зависимостей нет.

- [ ] **Step 1: e2e** (`seed.ts` создаёт решение, записи, секрет, второго пользователя с запросом доступа):
  - `journals.spec.ts`: лента changelog фильтруется; сравнение релизов показывает группы; insight заменяется, старый виден со статусом «заменён»; секрет скрыт и раскрывается; «Связи» показывают backlink;
  - `access.spec.ts`: запрос виден, одобрение `ro`, отзыв; переключатель `rules.agent_write`.
  
  Снимки — в `.tmp/e2e/screens/`.
- [ ] **Step 2:** `npm run check`, `build`, `size`, `e2e`. Коммит `feat(web): журналы, Vault, связи, управление доступом к решениям, настройки`.

### Task 14: Поиск по точкам папок (§8.3, §9.2)

**Files:**
- Modify: `crates/zam-index/src/qdrant.rs` (уровень `folder` в prefetch сводок), `crates/zam-search/src/hybrid.rs` (канал `folder`), `crates/zam-store/src/search.rs` (`live_folders`), `crates/zam-server/src/{api/search/mod.rs,config.rs}` (`[search] w_folder`, элементы `folder`)
- Test: `crates/zam-search` unit, `crates/zam-server/tests/folders_search.rs`

**Interfaces:**
- Consumes: `VectorHit` (задача 5), `fuse` (zam-search).
- Produces:
  ```rust
  pub struct FolderHit { pub folder: Id, pub path_prefix: String, pub rank: usize }
  pub struct SearchInputs { /* поля M2 */ pub folders: Vec<FolderHit> }
  // fuse: чанку узла под path_prefix папки прибавляется w_folder / (rrf_k + rank) — один раз, по лучшей папке-предку
  pub async fn live_folders(db: &Db, ids: Vec<Id>, scope: ChunkScope) -> Result<Vec<FolderRow /*id, space_id, path, l0, child_count*/>, StoreError>;
  ```
- **Поведение:**
  - Qdrant prefetch dense top-10 идёт по `note | section | folder`.
  - Точка папки проверяется по SQLite (`live_folders`): жива, не Human, в области.
  - Папка не занимает место в выдаче, а поднимает листы своих потомков каналом `folder` (иерархическое вспоминание, `w_folder`).
  - Карточка папки — только если `filters.kinds` содержит `folder`: `f:<id> <space>/<path>/  [score] · <N> детей` и L0; не больше `search.max_folders` (2).
  - Значение `w_folder` по умолчанию выбирается сеткой eval задачи 15 из `{0, 0.3, 0.5, 1.0}`; без значимого выигрыша — 0 (запись в журнал).

- [ ] **Step 1: Тесты.**
  - unit `folder_channel_boosts_descendants_once`, `no_folder_hits_equals_m2_order`;
  - сервер (MiniLM): `broad_query_lifts_notes_under_matching_folder`, `folder_cards_only_with_kind_folder_and_capped`, `deleted_or_human_folder_ignored`.
- [ ] **Step 2:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(search): точки папок — канал иерархического вспоминания и карточки папок`.

### Task 15: Eval — запросы на устаревание, «вспомнил отменённое», решения и FTS-only в наборе

**Files:**
- Create: `eval/insights.toml`, `eval/grid-m3.toml`
- Modify: `eval/queries.toml`, `eval/README.md`, `xtask/src/eval/{corpus.rs,run.rs,metrics.rs,report.rs,grid.rs}`, корпус (frontmatter `index: false` у выбранных заметок)

**Interfaces:**
- Consumes: REST записей и insights (задачи 8–9), `scope.solution` (задача 6), `w_folder` (задача 14), бусты (задача 9).
- **Набор:**
  - `eval/insights.toml` — `[[insight]] key, lang, kind, scope, area?, title, body, triggers?, supersedes = "<key>"?, retract_reason?`: не меньше 36 инсайтов, из них ≥ 12 цепочек замещения (старый и новый с разными выводами) и ≥ 4 отзыва;
  - в `queries.toml` — не меньше 30 запросов `kind = "superseded"` с `expect = ["insight:<новый>"]` и `forbid = ["insight:<старый>" | "insight:<отозванный>"]`;
  - заметки группы `e` (документы проекта) пишутся в решение с фиксированным GUID набора, поиск идёт с `scope.solution` — проверка слияния баз;
  - ≥ 10 заметок с `index: false` и запросами `exact` к ним — находки только FTS (открытый пункт M2).
- **Метрики:**
  - `cancelled@5` — доля запросов `superseded`, у которых в top-5 есть `forbid` (цель — 0 в режиме по умолчанию);
  - контрольный прогон с `include_history: true` — `cancelled@5 > 0`, иначе запросы ничего не проверяют, и `eval --check` падает;
  - recall@5, MRR@10, nDCG@10 по видам, включая `superseded`.
  
  Метрики — в отчёте и JSON, с бутстреп-интервалами.
- **Сравнение с M2:** парная разность nDCG@10 по неизменённым id запросов против `eval/results/2026-10-20-*` (hybrid int8 и fp16). Падение с интервалом целиком ниже нуля — дефект, разбирается в задаче.
- **Сетка `eval/grid-m3.toml`** (≤ 8 конфигураций, fp16 CUDA): `w_folder ∈ {0, 0.3, 0.5, 1.0}`, бусты `1.0` против `1.15/1.10`. Выбор — по парному бутстрепу. Значения пишутся в умолчания `[search]` и в журнал.
- **Прогоны:** fts, hybrid int8 CPU, hybrid fp16 CUDA, hybrid+rerank GPU. Результаты — `eval/results/<дата>-*.{json,md}`.

- [ ] **Step 1:** Набор и тест `eval_set_is_valid` (расширить): ключи `forbid` и `supersedes` существуют, цепочки без циклов, проверка «без общих слов» прежняя, отпечаток набора обновлён.
- [ ] **Step 2:** Загрузчик записей и метрики в xtask; unit-тест `cancelled_at_5_counts_forbidden_in_top5`.
- [ ] **Step 3:** Прогоны и сетка; результаты и выбор весов — в `eval/results/` и журнал. Коммит `feat(eval): запросы на устаревание, метрика «вспомнил отменённое», решения и FTS-only в наборе`.

### Task 16: Гейт качества int8 по поисковым метрикам

**Files:**
- Modify: `xtask/src/eval/gate.rs`, `eval/README.md`

**Interfaces:**
- Consumes: `paired_diff` (`xtask/src/eval/metrics.rs`).
- **Вердикт** (решение владельца 2026-10-21): «пройден», если нижняя граница 95 % интервала парного бутстрепа (10 000 выборок, фиксированный seed) разности int8 − fp32 ≥ −0,01 и для nDCG@10, и для recall@5. Косинус dense и Спирмен sparse остаются в отчёте в разделе «справочно» и на вердикт не влияют.
- **Векторы прогона** (`load_vectors`, `user_db`) читаются из всех баз памяти каталога прогона (`users/*.db` и `solutions/*.db`): после задачи 15 часть корпуса лежит в решении.
- **Ожидание по данным M2:** нижняя граница recall@5 была −0,015, поэтому гейт может оказаться «не пройден». Исполнитель порог не меняет, а пишет итог и интервалы в журнал как вопрос владельцу. Варианты — набор больше, вторая метрика, статическое int8.

- [ ] **Step 1: Unit-тесты.**
  - `verdict_uses_lower_bound_only`: косинус 0,5 и обе границы ≥ −0,01 → пройден; точечная оценка 0, но граница −0,02 → не пройден;
  - `cosine_is_informational`.
- [ ] **Step 2:** Реализация; прогон `cargo xtask eval --quality-gate` на наборе задачи 15; отчёт `eval/results/<дата>-quality-gate.{json,md}`.
- [ ] **Step 3:** Коммит `feat(eval): гейт int8 по нижней границе разности nDCG@10 и recall@5`.

### Task 17: Исследование — первый старт Qdrant после массовой индексации

**Files:**
- Modify: `xtask/src/bench.rs` (сценарий `--scenario first-start`), `crates/zam-index/src/supervisor/{config.rs,mod.rs}`, `crates/zam-index/src/qdrant.rs` (`wal_config` коллекции), `crates/zam-server/src/config.rs` (`[qdrant]`)
- Test: `crates/zam-index/tests/supervisor.rs` (новые случаи)

**Interfaces:**
- **Варианты:**
  - A — перед остановкой воркер перестаёт писать, супервизор ждёт `flush_interval_sec` + 1 с после последнего upsert (не дольше `qdrant.shutdown_flush_wait_ms`, 6000), затем останавливает Qdrant;
  - B — `wal_capacity_mb` коллекции 32 → 8 и 4 (`update_collection` для существующей);
  - C — `optimizers.flush_interval_sec` 5 → 1;
  - A+B.
- **Сценарий замера** (GPU и CPU, 10 тыс. заметок, HDD): массовая индексация → жёсткая остановка в момент записи (как в M2) → время до готовности Qdrant при следующем старте. Также меряются время штатной остановки и скорость индексации (токены/с). По 3 повтора на вариант: медиана и максимум, число операций WAL при повторе — из журнала Qdrant.
- **Критерий выбора:**
  - первый старт ≤ 5 с;
  - штатная остановка дольше не более чем на 5 с;
  - скорость индексации падает не больше чем на 5 %.
  
  Из вариантов, прошедших критерий, берётся лучший по первому старту. Ни один не прошёл — ближайший к цели, с записью остатка.
- **Внедрение:** ключи `qdrant.shutdown_flush_wait_ms`, `qdrant.wal_capacity_mb`, `qdrant.flush_interval_sec`, умолчания — по итогу. Итог и таблица — в журнал M3 и `eval/results/<дата>-bench-first-start.md`.

- [ ] **Step 1: Тесты.**
  - `config_renders_flush_interval_and_wal` (генерация `zam.yaml`, параметры коллекции);
  - `shutdown_waits_for_flush_bounded` (останов не дольше предела плюс 1 с).
- [ ] **Step 2:** Замеры по сценарию, выбор, внедрение, повторный замер выбранного варианта.
- [ ] **Step 3:** Коммит `perf(index): первый старт Qdrant после массовой индексации — <выбранный вариант>`.

### Task 18: e2e приёмки M3

**Files:**
- Create: `crates/zam-server/tests/m3_acceptance.rs` (настоящий процесс `zam-server`, `tests/common/process.rs`, MiniLM, Qdrant из `target/deps`)
- Modify: `.github/workflows/ci.yml` (job на обеих ОС, если время `check` растёт больше 15 мин)

- [ ] **Step 1: Сценарий «Изоляция»** (§18.1):
  - Human и Vault — нет в Qdrant (count по `space_kind=human` и по тексту секрета = 0);
  - их нет в выдаче поиска по токену; секрета нет в журнале сервера (`logs/`); значения Vault нет в FTS (прямой запрос `chunks_fts` по значению);
  - пользователь B без доступа к решению A — 403 `solution-access` на дерево, узел, записи, граф и поиск (решение исключено с предупреждением);
  - после одобрения `ro` — чтение есть, запись 403;
  - после отзыва тот же токен на следующем запросе — 403;
  - Human и Vault владельца B не видит ни в каком виде.
- [ ] **Step 2: Сценарий «Замещение без потери»:**
  - insight X с триггером → `supersede` новым Y;
  - поиск по умолчанию находит Y и не находит X (оба канала);
  - `include_history` находит обоих; `GET /entries/X` отдаёт X со статусом `superseded` и `superseded_by_id = Y`;
  - ревизии X целы; есть ребро `supersedes` Y → X;
  - триггеры X не выдаются в `GET /triggers`;
  - `retract` с причиной сохраняет запись.
- [ ] **Step 3: Сценарий «Секрет через агента»:**
  - агент токеном `rw` пишет секрет (`POST …/vault`) с `X-Zam-Session`/`X-Zam-Machine`, читает его `?reveal=1` и получает то же значение;
  - в аудите строка `vault.reveal` с сессией и машиной без значения;
  - в байтах базы и в журнале значения нет;
  - перезапуск сервера с тем же мастер-ключом раскрывает, с чужим — 503 `vault-locked`.
- [ ] **Step 4:** Прогон локально (Windows) и в CI (push ветки, обе ОС). Коммит `test: e2e приёмки M3 — изоляция, замещение без потери, секрет через агента`.

### Task 19: Документация, версия `0.0.4-m3`, приёмка M3

**Files:**
- Modify:
  - `CHANGELOG.md`, `README.md`, `docs/journal.md`;
  - `deploy/README-install.md` (мастер-ключ и его резервирование, каталог `solutions/`, совместный доступ), `deploy/config.example.toml` (`[vault]`, `[insights]`, новые ключи `[search]` и `[qdrant]`);
  - `docs/specs/2026-10-17-zeroaimemory-tz.md` (§17 по итогу задачи 17, §18.2 — итог гейта, §19 — статус, §23);
  - `Cargo.toml` и `web/package.json` (версия `0.0.4-m3`);
  - `xtask/src/smoke.rs` (шаги M3);
  - `docs/plans/2026-10-21-m3-journals-graph-log.md`.

- [ ] **Step 1: Smoke комплекта, шаги M3:**
  - регистрация решения;
  - запись insight → поиск находит его в top-3 с `kind=insight`;
  - `vault set` → `vault get` возвращает то же значение;
  - после остановки сервера процессов Qdrant нет.
- [ ] **Step 2: Полный прогон:**
  - fmt, clippy `-D warnings`;
  - `cargo test --workspace --no-fail-fast` дважды;
  - тесты `#[ignore]` с реальными моделями (отчёт);
  - WebUI: `check`, `build`, `size`, `e2e`;
  - `cargo xtask package` cpu и gpu + `smoke` → `SMOKE OK`;
  - CI зелёный на обеих ОС.
- [ ] **Step 3: Документы:**
  - CHANGELOG `## [0.0.4-m3]`: совместный доступ, журналы, граф, Vault, сканер, папки, eval, гейт, Qdrant WAL, известные ограничения;
  - `docs/journal.md` — итог M3 и проверка критериев §19 со ссылками на тесты и результаты eval;
  - журнал решений M3 — решения, rulings, отложенное, «Уточнить у владельца».
- [ ] **Step 4:** Коммит `docs: M3 — CHANGELOG, журнал, ТЗ, версия 0.0.4-m3`.
