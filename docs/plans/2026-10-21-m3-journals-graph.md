# M3. Журналы, граф, совместный доступ и эпистемический статус — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Редакция 2 (2026-10-21)** — по ответам владельца на вопросы первой редакции:
> - память решения **остаётся в базе пользователя-владельца**: отдельных `solutions/<guid>.db` и переноса нет, в `system.db` — реестр решений и разрешения;
> - добавлен эпистемический статус `certainty` (`axiom | theorem | hypothesis`);
> - раскладка ELK для Mermaid разрешена (EPL-2.0);
> - добавлена задача 17 «Нарезка с учётом силы границ» по исследованию `docs/research/2026-10-21-chunking-semantic.md`; её eval-прогоны идут на общем наборе задачи 16, а финальные прогоны eval и гейт (задача 18) — после смены нарезки.
>
> Коммит `c908977` (задача 1 первой редакции, ТЗ v1.9 по старому дизайну) уже в ветке. Задача 1 ниже правит **уже изменённое** ТЗ v1.9 под новый дизайн (fix round задачи 1).

**Goal.** Память решения становится общей и полной:
- владелец открывает память своего решения другим пользователям с подтверждением и отзывом (§11.6), память остаётся в его базе;
- у решения есть журналы Changelog, Releases (со сравнением), Insights (статусы, замещение, отзыв, дедупликация, триггеры);
- у важных записей есть эпистемический статус `axiom | theorem | hypothesis` с правилами «кто и как может его менять»;
- у пользователя — Vault с шифрованием и аудитом; перед индексацией работает сканер секретов;
- между заметками и записями строится граф (`edges`, wiki-ссылки, `expand=graph`);
- поиск идёт по личной базе и базам владельцев доступных решений, учитывает точки папок, статусы и надёжность записей;
- нарезка учитывает силу границ и заголовки H4–H6 (без висячих заголовков);
- eval меряет «вспомнил отменённое», «надёжное выше гипотезы» и chunk-recall на длинных заметках.

Этап закрывают e2e приёмки «изоляция», «замещение без потери», «секрет через агента» и версия `0.0.4-m3`.

**Architecture.**
- **Хранение — решение владельца 2026-10-21.**
  - Решение — пространство `solution` в `users/<owner>.db`, как в M1–M2.
  - В `system.db` лежит реестр решений (`solutions`: GUID → владелец, `space_id`) и разрешения (`solution_access`: участник, `rw`/`ro`, запросы `pending/approved/rejected/revoked`).
  - Сервер по GUID находит базу владельца и пускает участника **только в пространство этого решения**. Human, Rules, Knowledge и Vault владельца участнику недоступны никогда.
  - Доступ проверяется на каждом запросе без кэша.
- **Индекс.** Тенант Qdrant остаётся прежним — пользователь-владелец базы (`user_id`, `is_tenant`). Фильтр поиска — дизъюнкция областей: «мой `user_id` и мои пространства» ИЛИ «`user_id` владельца и `space_id` решения». Перестройки коллекции не нужно: новые поля payload (`entry_id`, `status`, `insight_kind`, `certainty`) добавляются с индексами в существующую коллекцию. Фильтр статуса — «не `superseded`/`retracted`», поэтому точки M2 без поля `status` проходят.
- **Записи журналов** (`entries`) проходят тот же конвейер, что заметки: чанки → FTS → `index_jobs` (`target_kind = 'entry'`) → Qdrant.
- **Vault** — отдельные таблицы `vault_items` вне конвейера индексации: значения структурно не могут попасть в FTS и Qdrant. Vault участника для общего решения — в базе участника, с привязкой к `agent_solution_id`.
- **Граф** — таблица `edges` и рекурсивные CTE за трейтом `GraphStore`. Ссылки разрешаются одним кодом для рендера и для рёбер.
- **Эпистемический статус** (`certainty`) ортогонален статусу insight:
  - `axiom` создаёт и меняет только человек;
  - `theorem` агент ставит только с доказательством;
  - `hypothesis` — по умолчанию для записей агента.
  
  Человек — `author_kind = human`: сессия WebUI или «человеческий» токен.

**Tech Stack.** К стеку M2 добавляется (лицензии — по `about.toml` для Rust и `ALLOWED` в `web/vite.config.ts` для бандла; аудит каждой — в журнал задачи):
- `chacha20poly1305` 0.10 (Apache-2.0 OR MIT) — XChaCha20-Poly1305 для Vault;
- `zeroize` 1 (Apache-2.0 OR MIT) — затирание ключей;
- `regex` 1 (MIT OR Apache-2.0; уже в дереве, теперь прямая зависимость) — сканер секретов и проверка триггеров (обратных ссылок нет по построению);
- `globset` 0.4 (Unlicense OR MIT, берём MIT) — проверка `file_glob`;
- `rust-stemmers` =1.2.0 (MIT/BSD-3-Clause, уже в xtask) — становится зависимостью `zam-markdown` для флага `chunk_cohesion` (задача 17);
- WebUI: `elkjs` (EPL-2.0, уже в `package-lock.json` как зависимость `mermaid` 12; сейчас заменён заглушкой `src/lib/hydrate/elk-stub.ts`) — разрешён владельцем 2026-10-21, задача 14.

**Spec:** `docs/specs/2026-10-17-zeroaimemory-tz.md` (v1.9 после `c908977`; задача 1 правит его под редакцию 2). Нужны:
- §2.2, §2.3, §3;
- §6.1–6.4;
- §7 целиком;
- §8.1–8.3, §8.5;
- §9.1–9.2, §9.4;
- §11.1, §11.2, §11.4, §11.6;
- §12;
- §13.2, §13.3;
- §15.1–15.4;
- §16.1;
- §17;
- §18.1, §18.2;
- §19 (строка M3).

Журнал M2: `docs/plans/2026-10-19-m2-index-ml-log.md`. Журнал этого этапа: `docs/plans/2026-10-21-m3-journals-graph-log.md`.

## Global Constraints

- **Временные файлы**, логи прогонов, тестовые данные — только внутри проекта: `.tmp/` (под `.gitignore`) или `target/` (в тестах `env!("CARGO_TARGET_TMPDIR")`, модели — `target/models/`, бинарники — `target/deps/<платформа>/`). Запрещены `/tmp` (на Windows это корень диска), `%TEMP%`, `AppData\Local\Temp`, scratchpad Claude Code, `std::env::temp_dir()`, `tempfile` без явного `dir=`/`tempdir_in`, `os.tmpdir()`. **Правило передаётся каждому субагенту в тексте задания.**
- **Окружение.** Rust portable лежит в `.toolchain/`: перед `cargo` и `npm` выполнять `. scripts/dev-env.ps1` (PowerShell) или `. scripts/dev-env.sh`. Данные проекта на HDD — пределы ожидания в тестах с запасом на fsync.
- **Платформы:** Windows x64 и Linux x64. GPU — только CUDA. Без Docker.
- **Зависимости** — только перечисленные в Tech Stack или с аудитом лицензии (Rust — список `accepted` в `about.toml`, бандл WebUI — `ALLOWED` в `web/vite.config.ts`; проект под MIT; EPL-2.0 разрешена владельцем только для `elkjs`). Новая зависимость и её лицензия записываются в журнал задачи.
- **Проверки каждой задачи:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test … --no-fail-fast`; для WebUI — `npm run check`, `npm run build`, `npm run size`, `npm run e2e`.
- **Единообразие с M0–M2:** русские комментарии, документы и тексты UI; английские идентификаторы; `thiserror` в библиотеках, `anyhow` в бинарниках; ошибки API — RFC 9457 через `ApiError`; логи — `tracing` (без текста заметок, записей, секретов, токенов и ключей); конфигурация — слой «файл → `ZAM_*` → флаги»; пути через `Path::join` по компонентам; один писатель на файл SQLite (`Db::write`).
- **Миграции** — номера по факту:
  - системные: `0003` (решения и доступ), `0004` (человеческий токен), `0005` (ключи Vault);
  - пользовательские (`migrations/user/`): `0007` (атрибуция), `0008` (записи журналов), `0009` (`certainty` узлов), `0010` (рёбра), `0011` (Vault).
  
  Перед миграцией — копия файла (`migrate::apply`), обработка существующих данных идемпотентна.
- **Инварианты (не нарушаются ни одной задачей):**
  - Human никогда не попадает в Qdrant и не виден по токену ни в каком виде;
  - участник общего решения видит в базе владельца **только пространство этого решения**: Rules, Human, Knowledge и Vault владельца, его рёбра в другие пространства и узлы вне решения ему недоступны (404);
  - значения Vault никогда не попадают в FTS, Qdrant, ответы поиска и логи; Vault не разделяется между пользователями;
  - узлы с `index: false` не векторизуются;
  - история только дописывается (`revisions`, `entry_revisions`, `vault_revisions` — append-only триггерами), удаление мягкое, замещение и отзыв ничего не удаляют;
  - агент (`author_kind = agent`) не создаёт `axiom`, не переводит в `axiom` и не меняет запись или заметку со статусом `axiom`;
  - SQLite — источник истины, Qdrant пересобирается из `embeddings` без инференса.
- **Коммиты** — после каждой задачи, ветка `m3-journals`, Conventional Commits на русском, trailer `Co-Authored-By` по реальной модели исполнителя.

## Review Focus

1. **Отзыв или отклонение доступа, пока агент работает с тем же токеном.** Уже следующий запрос (чтение узла, запись, поиск, граф, записи журналов, вложения) получает 403 `solution-access` или, в поиске, исключение решения с предупреждением. Кэша решений о доступе нет. Тесты — в задачах 3, 5 и 20.
2. **Участник «выходит» за пространство решения в базе владельца** — по id узла или записи из Knowledge владельца, через ребро или backlink в другое пространство, через `expand=graph`, `path_prefix`, вложение или `GET /spaces/{id}` по `space_id`. Каждый путь отвечает 404 или отфильтровывает чужое. Тесты — в задачах 3, 5 и 10.
3. **Агент обходит правила `certainty`:** PATCH записи `axiom`, supersede или retract `axiom`, перевод в `axiom` через frontmatter заметки, `theorem` без `evidence`, правка заметки `axiom` переносом или удалением. Всё отвечает 403/422, а человек (WebUI или человеческий токен) может это сделать. Тест — в задаче 9.
4. **Ложные срабатывания сканера секретов на идентификаторах** (git SHA, sha256, UUID, base32-id, длинные URL). Они не маскируются, настоящие ключи маскируются. Тест — в задаче 11.
5. **Мастер-ключ Vault потерян, подменён или расходится с `ZAM_MASTER_KEY`.** Сервер работает, Vault заблокирован с понятной причиной (503), новый ключ молча не создаётся. Тест — в задаче 12.

---

## Карта файлов

```
docs/specs/2026-10-17-zeroaimemory-tz.md        # v1.9 → правка под редакцию 2 (задача 1), итог (задача 20)
crates/zam-core/src/{solution,target}.rs        # SolutionGuid, TargetKind, TargetRef
crates/zam-store/migrations/system/{0003_solutions,0004_human_tokens,0005_vault_keys}.sql
crates/zam-store/migrations/user/{0007_attribution,0008_entries,0009_node_certainty,0010_edges,0011_vault}.sql
crates/zam-store/src/{access,solution_registry,certainty,graph,links,vault}.rs
crates/zam-store/src/entries/{mod,write,read,chunks,triggers}.rs
crates/zam-markdown/src/{secrets.rs,frontmatter.rs,chunk.rs} # сканер; certainty, evidence, relations; сила границ
crates/zam-index/src/{vector,qdrant,worker}.rs
crates/zam-search/src/{hybrid,adjust}.rs                     # бусты, канал папок
crates/zam-server/src/api/{scope,solutions,spaces,entries,releases,insights,triggers,graph,vault,admin}.rs
crates/zam-server/src/api/search/{mod,targets,merge}.rs      # бывший search.rs
crates/zam-server/src/{vault/{mod,keys,crypto}.rs,certainty.rs}
crates/zam-cli/src/cmd/{solution,log,release,insight,vault,link}.rs
web/src/components/{ChangelogView,ReleasesView,InsightsView,VaultView,Links}.svelte, routes/{Solutions,Settings}.svelte
web/vite.config.ts, web/src/lib/hydrate/{mermaid.ts,elk-stub.ts → удаляется}
eval/{insights.toml,queries.toml,grid-m3.toml,README.md}, xtask/src/eval/*, xtask/src/bench.rs
crates/zam-server/tests/m3_acceptance.rs
```

---

### Task 1: ТЗ v1.9 (fix round) — хранение в базе владельца, эпистемический статус, ELK; закрытые вопросы

**Files:**
- Modify: `docs/specs/2026-10-17-zeroaimemory-tz.md` — уже изменён коммитом `c908977` (v1.9 по дизайну первой редакции: `solutions/<guid>.db`, перенос, `tenant`, `schema 2`); правится под редакцию 2
- Modify: `docs/plans/2026-10-21-m3-journals-graph-log.md` (запись fix round)

**Interfaces:**
- Produces: текст ТЗ, на который ссылаются задачи 2–20. Имена и схемы — из Interfaces задач 2, 6, 9, 10 и 12 без изменений.

- [ ] **Step 1: Убрать дизайн первой редакции.**
  - Убрать: `solutions/<agent_solution_id>.db`, `blobs/solutions/…`, «База памяти» как термин, перенос решений в `solutions/`, `meta.db_kind`, `tenant` (`u:`/`s:`) и `metadata.schema = 2`, `index_skipped` по тенанту, `zam-server migrate` с переносом, `MemoryDbs`.
  - Снять пометки «[предложение, ждёт подтверждения владельца]» у модели хранения и §11.6: дизайн хранения — **решение владельца 2026-10-21**. Оставшиеся решения исполнителя (по умолчанию `rw`, переименование GUID при конфликте, человеческий токен) помечаются «[решение исполнителя]».
- [ ] **Step 2: Записать редакцию 2.**
  - §3: «Решение» — пространство в базе владельца, `agent_solution_id`; «Владелец решения», «Участник решения»; «Эпистемический статус» (`certainty`).
  - §6.1: каталог данных как в M2.
  - §6.2: `solutions` (GUID → `owner_id`, `space_id`, имя), `solution_access` (схема задачи 2), `tokens.human` (задача 9), `user_keys` (задача 12).
  - §6.3: `revisions.user_id`, у `entries` — `user_id` и `certainty`, у `entry_revisions` — `certainty` и `reason`, `nodes.certainty`; Vault — `vault_items`/`vault_revisions` **[решение исполнителя]**; рёбра — внутри одной базы.
  - §6.4: тенант — `user_id` владельца базы; поиск фильтрует по дизъюнкции областей `(user_id, space_ids)`; новые поля payload `entry_id`, `status`, `insight_kind`, `certainty` — без пересборки коллекции; фильтр статуса — исключающий.
  - §7.5: поле `certainty` (новый подраздел «Эпистемический статус»): определения axiom/theorem/hypothesis, матрица прав (задача 9), `data.evidence {checked, method, result}`, пример владельца со статьёй; для заметок — `certainty:` и `evidence:` во frontmatter.
  - §7.6: Vault участника для решения — в его базе, привязан к `agent_solution_id`.
  - §9.1: `filters.certainty`.
  - §9.2: бусты `axiom > theorem > hypothesis`; карточки показывают статус.
  - §11.4: человеческий токен (`human`) — выпускает только администратор, хранится не в CLAUDE.md, а в `~/.zeroaimemory/config.toml` **[решение исполнителя]**.
  - §11.6: хранение в базе владельца; реестр и разрешения в `system.db`; участник — только пространство решения; конфликт одного GUID у двух пользователей: первый зарегистрировавший — владелец, второй получает `pending`, а его прежнее локальное решение (M1–M2) переименовывается под новый GUID **[решение исполнителя]**.
  - §12, §13.2: эндпоинты и команды задач 3, 7–10, 12.
  - §15.2: Mermaid `layout: elk` — ленивая раскладка ELK (EPL-2.0, решение владельца 2026-10-21).
  - §8.2: H4–H6 входят в путь заголовков и `context_prefix` (секции — по-прежнему H1–H3), лист не кончается заголовком и вводной фразой на «:», ключи `[index] chunk_boundary_lambda|chunk_cohesion|chunk_prefix_l0` (задача 17);
  - §18.2: eval «надёжное выше гипотезы», chunk-recall@5 на длинных заметках (задача 16).
  - §19: строка M3 — плюс эпистемический статус.
  - §23: запись «v1.9, редакция 2».
- [ ] **Step 3: Самопроверка.**
  - `rg -n "solutions/<|schema 2|tenant|MemoryDbs|предложение, ждёт" docs/specs` — нет совпадений (кроме истории §23, где описано, что убрано);
  - `rg -n "certainty" docs/specs` — §3, §6.3, §6.4, §7.5, §9.1, §9.2.
- [ ] **Step 4: Журнал.** Fix round задачи 1 — что изменено по ответам владельца. Коммит `docs: ТЗ v1.9 — хранение в базе владельца, эпистемический статус, ELK`.

### Task 2: Реестр решений, доступ участников, атрибуция автора, регистрация существующих решений

**Files:**
- Create: `crates/zam-core/src/{solution.rs,target.rs}`; Modify: `crates/zam-core/src/lib.rs`
- Create: `crates/zam-store/migrations/system/0003_solutions.sql`, `crates/zam-store/migrations/user/0007_attribution.sql`, `crates/zam-store/src/{access.rs,solution_registry.rs}`, `crates/zam-store/tests/{access.rs,solution_registry.rs}`
- Modify: `crates/zam-store/src/{lib.rs,system.rs,user_db.rs (create_solution с заданным space_id, rename_solution_guid)}`, `src/nodes/{mod.rs,write.rs,revise.rs,tree_ops.rs}` (запись `user_id`)

**Interfaces:**
- Produces (`zam-core`):
  ```rust
  /// agent_solution_id: UUID с дефисами, хранится в нижнем регистре.
  pub struct SolutionGuid(String);
  impl FromStr for SolutionGuid { type Err = IdError; }   // только 36 символов с дефисами (как canonical_guid сервера)
  impl SolutionGuid { pub fn as_str(&self) -> &str; pub fn new_v7() -> SolutionGuid; }
  impl Display, Serialize, Deserialize, Hash, Eq, Ord, Clone for SolutionGuid;
  pub enum TargetKind { Node, Entry }                       // serde lowercase
  pub struct TargetRef { pub kind: TargetKind, pub id: Id }  // Display/FromStr: "n:<id>" | "e:<id>"
  ```
- Схема `0003_solutions.sql`:
  ```sql
  CREATE TABLE solutions (
      guid        TEXT PRIMARY KEY,                -- agent_solution_id
      owner_id    TEXT NOT NULL REFERENCES users(id),
      space_id    TEXT NOT NULL UNIQUE,            -- пространство solution в users/<owner_id>.db
      name        TEXT NOT NULL,
      created_at  TEXT NOT NULL,
      archived_at TEXT
  );
  CREATE TABLE solution_access (
      guid         TEXT NOT NULL REFERENCES solutions(guid),
      user_id      TEXT NOT NULL REFERENCES users(id),
      access       TEXT NOT NULL CHECK (access IN ('rw','ro')),
      state        TEXT NOT NULL CHECK (state IN ('pending','approved','rejected','revoked')),
      requested_at TEXT NOT NULL, token_id TEXT,
      decided_at   TEXT, decided_by TEXT,          -- 'admin' | id владельца
      reason       TEXT,
      PRIMARY KEY (guid, user_id)
  );
  CREATE INDEX solution_access_user ON solution_access (user_id, state);
  CREATE INDEX solution_access_pending ON solution_access (guid) WHERE state = 'pending';
  ```
- Схема `0007_attribution.sql`: `ALTER TABLE revisions ADD COLUMN user_id TEXT;` — `NULL` у старых строк означает «владелец базы».
- Produces (`zam-store`):
  ```rust
  pub struct SolutionInfo { pub guid: SolutionGuid, pub owner_id: Id, pub space_id: Id, pub name: String, pub created_at: Timestamp }
  pub enum AccessState { Pending, Approved, Rejected, Revoked }
  pub enum SolutionRole { Owner, Member(Access), Pending, Rejected, Revoked }
  pub enum AccessDecision { Approve(Access), Reject { reason: Option<String> } }
  pub struct AccessEntry { pub user_id: Id, pub user_name: String, pub access: Access, pub state: AccessState,
                           pub requested_at: Timestamp, pub decided_at: Option<Timestamp>, pub decided_by: Option<String> }
  impl SystemStore {   // access.rs
      pub async fn register_solution(&self, guid: &SolutionGuid, owner: Id, space_id: Id, name: &str) -> Result<(SolutionInfo, bool /*создано*/), StoreError>;
      pub async fn get_solution(&self, guid: &SolutionGuid) -> Result<Option<SolutionInfo>, StoreError>;
      pub async fn solution_by_space(&self, space_id: Id) -> Result<Option<SolutionInfo>, StoreError>;
      pub async fn list_solutions(&self) -> Result<Vec<SolutionInfo>, StoreError>;
      pub async fn solution_role(&self, guid: &SolutionGuid, user: Id) -> Result<Option<SolutionRole>, StoreError>; // None — ни владелец, ни запроса
      pub async fn request_access(&self, guid: &SolutionGuid, user: Id, token: Option<Id>) -> Result<AccessState, StoreError>;
      pub async fn decide_access(&self, guid: &SolutionGuid, user: Id, d: AccessDecision, by: &str) -> Result<(), StoreError>;
      pub async fn revoke_access(&self, guid: &SolutionGuid, user: Id, by: &str) -> Result<(), StoreError>;
      pub async fn list_access(&self, guid: &SolutionGuid) -> Result<Vec<AccessEntry>, StoreError>;
      pub async fn accessible_solutions(&self, user: Id) -> Result<Vec<(SolutionInfo, SolutionRole /*Owner|Member*/)>, StoreError>;
      pub async fn pending_requests_for_owner(&self, owner: Id) -> Result<u64, StoreError>;
  }
  pub struct Attribution { /* поля M1 */ pub user_id: Option<Id> }
  pub struct RegistryReport { pub registered: usize, pub already: usize, pub renamed: Vec<(SolutionGuid /*было*/, SolutionGuid /*стало*/)> }
  pub async fn register_user_solutions(sys: &SystemStore, db: &Db, user: Id) -> Result<RegistryReport, StoreError>; // solution_registry.rs
  pub async fn rename_solution_guid(db: &Db, space_id: Id, new: &SolutionGuid) -> Result<(), StoreError>;
  pub async fn create_solution_with_id(db: &Db, space_id: Id, guid: &SolutionGuid, name: &str) -> Result<(Space, bool), StoreError>;
  ```
- **Переходы** (иначе `Conflict`):
  - `pending → approved | rejected`;
  - `approved → revoked`, `approved → approved` (смена режима);
  - `rejected → approved`, `revoked → approved`.
  
  Повторный `request_access` после `rejected`/`revoked` состояние не меняет **[решение исполнителя]**. Владелец не может быть участником (`BadInput`).
- **Аудит** — в транзакции `system.db`: `solution.register`, `solution.access.request|approve|reject|revoke|mode`, `solution.guid_conflict`.
- **Регистрация существующих решений** (`register_user_solutions`, вызывается при старте сервера для каждого пользователя и из `zam-server migrate` — задача 3) для каждого пространства `solution` базы пользователя:
  - GUID не в реестре → `register_solution(владелец = пользователь)`;
  - уже за этим пользователем с этим `space_id` → ничего;
  - за другим владельцем → локальное пространство получает новый `SolutionGuid::new_v7()` (`rename_solution_guid`) и регистрируется за пользователем; для исходного GUID ставится `request_access` (pending), аудит `solution.guid_conflict`, WARN **[решение исполнителя; в «Уточнить у владельца»]**.
  
  Порядок обхода — по `users.created_at`: первый зарегистрировавший — владелец. Функция идемпотентна.

- [ ] **Step 1: Тесты `zam-core`:** `guid_canonical_forms`, `target_ref_display_parse`.
- [ ] **Step 2: Тесты `tests/access.rs`:**
  - `register_is_idempotent_and_owner_fixed`;
  - `request_access_idempotent_pending`;
  - `roles_owner_member_pending`;
  - `transition_matrix`;
  - `rejected_not_reset_by_new_request`;
  - `audit_rows_written`;
  - `accessible_solutions_only_owner_and_approved`;
  - `pending_count_for_owner`;
  - `migration_v2_to_v3_keeps_data`.
- [ ] **Step 3: Тесты `tests/solution_registry.rs`:**
  - `existing_solutions_registered_once` (повтор — `already`);
  - `guid_conflict_second_user_renamed_and_pending` (у второго пользователя пространство с новым GUID, его заметки на месте, запрос `pending` к исходному);
  - `revision_records_user_id`;
  - `migration_v6_to_v7_keeps_revisions`.
- [ ] **Step 4:** Реализация. `cargo test -p zam-core -p zam-store --no-fail-fast`, clippy, fmt. Коммит `feat(store): реестр решений и доступ участников, атрибуция user_id, регистрация решений M2`.

### Task 3: Сервер — доступ к решениям, `agent_solution_id`, API доступа, CLI, настройки пространств

**Files:**
- Modify: `crates/zam-server/src/api/{scope.rs,solutions.rs,spaces.rs,admin.rs,blobs.rs,mod.rs}`, `api/nodes/{read,write,history}.rs`, `src/{app.rs,attrib.rs,error.rs,main.rs}`
- Modify: `web/src/lib/api.ts` и места использования `solution_guid` (только переименование поля, чтобы e2e WebUI оставались зелёными)
- Create: `crates/zam-cli/src/cmd/solution.rs`; Modify: `crates/zam-cli/src/{main.rs,target.rs}`, `cmd/{mod.rs,status.rs}`
- Test: `crates/zam-server/tests/solutions_access.rs`, `crates/zam-cli/tests/solution_access.rs`

**Interfaces:**
- Consumes: задача 2.
- Produces (сервер):
  ```rust
  pub enum Role { Personal, Owner, Member(Access), Admin }
  /// Доступная пользователю запроса область одной базы: его личная база (личные пространства и свои решения)
  /// или база владельца общего решения (ровно одно пространство решения).
  pub struct MemScope { pub user: User /*кто действует*/, pub owner: Id /*чья база*/, pub db: Db, pub space: Space, pub role: Role }
  pub struct NodeScope { /* поля M1 */ pub owner: Id, pub role: Role }
  pub async fn space_scope(st: &AppState, p: &Principal, h: &HeaderMap, raw: &str, mode: Mode) -> Result<MemScope, ApiError>;
  pub async fn node_scope(st: &AppState, p: &Principal, h: &HeaderMap, raw_id: &str, mode: Mode) -> Result<NodeScope, ApiError>;
  pub struct Readable { pub owner: Id, pub db: Db, pub spaces: Vec<Space>, pub role: Role }
  pub async fn readable_areas(st: &AppState, p: &Principal, user: &User) -> Result<Vec<Readable>, ApiError>; // личная + по базе на каждое доступное чужое решение
  pub fn effective_write(p: &Principal, role: Role) -> Result<(), ApiError>; // токен rw И (Personal | Owner | Member(Rw)); Admin — всегда
  ApiError::SolutionAccess { guid: SolutionGuid, state: &'static str /*pending|rejected|revoked*/ } // 403, type "/problems/solution-access"
  ```
- **Разрешение области:**
  - **GUID решения:** реестр (каждый запрос читает `system.db`) → роль → `users.get(owner_id)` → пространство `space_id`. GUID не зарегистрирован → 404.
  - **Id пространства** (`SpaceRef::Id`): личная база; для чужой базы — только `space_id` доступного решения (`solution_by_space`).
  - **`node_scope`:** сначала личная база; затем базы владельцев доступных решений, где узел принимается, **только если `node.space_id` = `space_id` решения** (иначе 404 — Review Focus 2).
  - **Записи:** `entry_scope` (задача 7) использует то же правило.
- **Атрибуция:** `user_id` пользователя запроса и его токен — и при записи участника в базу владельца.
- **Вложения:**
  - `POST /blobs` к узлу решения пишет в каталог блобов владельца базы;
  - `GET /blobs/{sha256}/{filename}` ищет в личной базе, затем в базах владельцев доступных решений, в пределах пространства решения.
- **REST:**
  - `POST /solutions {agent_solution_id | id, name}`:
    - GUID свободен → сначала `register_solution` (с заранее созданным `space_id`), затем `create_solution_with_id` в базе вызывающего — 201;
    - владелец → 200 (пространство создаётся, если его нет после сбоя);
    - одобренный участник → 200;
    - чужое решение, запроса нет или он ждёт → 202 `{agent_solution_id, state: "pending"}`, своего пространства с этим GUID не создаётся;
    - отклонено или отозвано → 403 `solution-access`.
    
    Гонка двух первых регистраций разрешается реестром: проигравший получает 202.
  - `GET /solutions` — доступные решения с ролью (администратору — все).
  - `GET /solutions/{guid}` — пространство, `agent_solution_id`, `owner {id, name}`, `role`, `access`.
  - `GET /solutions/{guid}/access` (владелец любым своим токеном или администратор) → `{owner, members: [AccessEntry], pending: n}`.
  - `POST /solutions/{guid}/access/{user}/approve {access?: "rw"|"ro"}` (по умолчанию `rw` **[решение исполнителя]**), `…/reject {reason?}`, `…/revoke` — `rw`-токен владельца или администратор, недопустимый переход → 409.
  - `GET /spaces`:
    - личные пространства — свои решения с `role: owner`, `shared` (есть участники);
    - чужие решения — пространство из базы владельца с `role: member`, `access`, `owner`;
    - `access_requests_pending` (для владельца).
  - `GET/PATCH /admin/settings` — `rules.agent_write: bool`, аудит `settings.update`.
  - JSON-поле решения везде называется `agent_solution_id` (`Space` сериализует `solution_guid` под этим именем).
- **Старт:** `app::finish` и `zam-server migrate` вызывают `register_user_solutions` для каждого пользователя до `IndexRuntime::start`. Итог — одной строкой INFO (счётчики).
- **CLI:**
  - `solution.toml` читается с ключом `agent_solution_id`, старый `id` — через `serde(alias)`;
  - `zam solution access list|requests|approve <user> [--ro]|reject <user> [--reason R]|revoke <user> [--solution GUID]` (пользователь по имени или id);
  - `zam status` печатает роль в решении и «ожидают подтверждения доступа: N (zam solution access requests)».
  
  Дайджест старта сессии с этим числом — M4.

- [ ] **Step 1: Тесты `tests/solutions_access.rs`** (A — владелец, B, C):
  - `register_new_guid_registers_and_creates_space` (201, реестр, аудит);
  - `second_user_gets_pending_no_space_created` (202; дерево для B — 403 `state=pending`);
  - `owner_approves_member_reads_writes_with_attribution` (ревизия в базе A с `user_id = B`, `token_id` B);
  - `ro_member_and_ro_token_cannot_write`;
  - `revoke_takes_effect_on_next_request_same_token` (Review Focus 1);
  - `reject_then_repeat_request_stays_rejected`;
  - `only_owner_rw_or_admin_manage_access`;
  - `member_confined_to_solution_space` (Review Focus 2): id узлов Knowledge, Rules и Human владельца → 404; `GET /spaces/<id Knowledge владельца>/tree` → 404; блоб узла Knowledge владельца → 404;
  - `node_lookup_in_owner_db_without_header`;
  - `admin_sees_and_manages_all`;
  - `settings_rules_agent_write_toggle`;
  - `startup_registers_m2_solution` (каталог данных M2 → после старта решение в реестре, `GET /spaces` показывает `role: owner`).
- [ ] **Step 2: Тесты CLI:**
  - `solution_toml_old_id_key_read`;
  - `solution_access_flow` (B регистрирует → A видит запрос → `approve B --ro` → B читает, запись → 403 с текстом).
- [ ] **Step 3:** Реализация. `cargo test -p zam-server -p zam-cli --no-fail-fast`; WebUI: `check`, `build`, `e2e` (регрессия); clippy, fmt.
- [ ] **Step 4:** Коммит `feat(server): совместный доступ к решению — запрос, подтверждение, отзыв; agent_solution_id; настройки Rules`.

### Task 4: Индекс — области владельцев в фильтре, новые поля payload без пересборки

**Files:**
- Modify: `crates/zam-index/src/{vector.rs,qdrant.rs,worker.rs,edge.rs}`, `tests/{common/cases.rs,qdrant.rs}`
- Modify: `crates/zam-store/src/jobs.rs` (`IndexChunk` — поля записи, пока пустые)
- Test: `crates/zam-server/tests/index_worker.rs` (новые случаи)

**Interfaces:**
- Consumes: `TargetRef` (задача 2).
- Produces:
  ```rust
  pub struct PointPayload { /* поля M2: user_id (тенант — владелец базы), space_id, space_kind, solution_guid, path_ancestors,
      area, level, kind, tags, updated_at, rev, text_hash */
      pub node_id: Option<Id>, pub entry_id: Option<Id>, pub status: Option<String>, pub insight_kind: Option<String>, pub certainty: Option<String> }
  /// Одна область: база владельца и её пространства (непустой список).
  pub struct VectorScope { pub user_id: Id, pub space_ids: Vec<Id> }
  pub struct VectorFilter { pub scopes: Vec<VectorScope> /*should; пусто → пустая выдача*/, pub path_prefix: Option<String>, pub levels: Vec<String>,
      pub kinds: Vec<String>, pub exclude_statuses: Vec<String> /*must_not*/, pub insight_kinds: Vec<String>, pub certainty: Vec<String>, pub area: Option<String> }
  pub struct VectorHit { pub chunk_id: String, pub user_id: Id /*владелец базы*/, pub target: TargetRef, pub score: f32, pub level: String }
  // VectorIndex: delete_nodes(user, nodes) заменяется на
  fn delete_targets<'a>(&'a self, user: Id, targets: &'a [TargetRef]) -> IndexFuture<'a, ()>;   // по node_id или entry_id
  fn nearest_dense<'a>(&'a self, v: &'a [f32], f: &'a VectorFilter, limit: usize) -> IndexFuture<'a, Vec<VectorHit /*score = косинус*/>>;
  ```
- **Без пересборки** (`metadata.schema` остаётся 1):
  - `ensure` идемпотентно создаёт недостающие payload-индексы `entry_id`, `kind`, `status`, `insight_kind`, `certainty`, `area` (`create_field_index` на существующей коллекции);
  - точки M2 без `status`/`certainty` проходят исключающий фильтр статуса;
  - фильтр `certainty` (`must … in`) задаётся, только если запрошен.
  
  `path_prefix` применяется внутри каждой области.
- **Фильтр** — `should` из `must[user_id = u, space_id ∈ spaces]` по областям, общий для всех prefetch. Воркер, reindex и `/index/status` обходят базы пользователей, как в M2 (решения лежат в них).

- [ ] **Step 1: Тесты `zam-index`** (настоящий Qdrant, макрос `vector_index_cases!`):
  - `scopes_disjunction_isolates_users_and_spaces`: точки `A/Knowledge`, `A/Solution`, `B/Knowledge`; фильтр `[(B, [B/Knowledge]), (A, [A/Solution])]` → ни одной точки `A/Knowledge`;
  - `exclude_statuses_keeps_points_without_status`;
  - `kinds_insight_kinds_certainty_area_filters`;
  - `delete_targets_by_node_and_entry`;
  - `nearest_dense_returns_cosine`;
  - `ensure_adds_indexes_to_existing_collection` (коллекция M2 → индексы добавлены, точки на месте, без пересоздания).
- [ ] **Step 2: Тест сервера** `m2_collection_kept_and_upgraded_without_reindex` (счётчик `embed_passages` = 0, точки не пересозданы); регрессия `qdrant_lifecycle`, Human не в Qdrant.
- [ ] **Step 3:** Реализация. `cargo test -p zam-index -p zam-server --no-fail-fast`, `cargo clippy -p zam-index --features edge -- -D warnings`, fmt. Коммит `feat(index): фильтр по областям владельцев, поля записей в payload без пересборки`.

### Task 5: Поиск по личной базе и базам владельцев доступных решений

**Files:**
- Split: `crates/zam-server/src/api/search.rs` → `api/search/{mod.rs (обработчик), targets.rs (область), merge.rs (слияние FTS нескольких баз)}` — без смены поведения M2 в пределах одной базы
- Modify: `crates/zam-store/src/search.rs` (`FtsHit`), `crates/zam-search/src/*` (`node_id` → `target`)
- Test: `crates/zam-server/tests/search_multi.rs`; unit-тесты `merge.rs`

**Interfaces:**
- Consumes: `readable_areas` (задача 3), `VectorFilter.scopes`, `VectorHit.user_id` (задача 4).
- Produces:
  ```rust
  // zam-store
  pub struct FtsHit { /* поля M2, node_id → */ pub target: TargetRef, /* … */ pub raw_score: f64 /*bm25 канала, меньше — лучше*/ }
  // zam-search: ChunkMeta.node_id / Ranked.node_id → target: TargetRef (лимит max_per_node — по target)
  // server api/search/targets.rs
  pub struct SearchTarget { pub owner: Id, pub db: Db, pub spaces: Vec<Space>, pub role: Role }
  pub async fn resolve_targets(st: &AppState, p: &Principal, h: &HeaderMap, sc: &ScopeReq) -> Result<(Vec<SearchTarget>, Vec<String> /*warnings*/), ApiError>;
  // merge.rs
  pub struct MergedChannels { pub bm25: Vec<(usize /*цель*/, FtsHit)>, pub trigram: Vec<(usize, FtsHit)>, pub relaxed: bool }
  pub fn merge_channels(per_db: Vec<(usize, FtsChannels)>, top_k: usize) -> MergedChannels; // каждый канал — по raw_score, score = 1/место
  ```
- **Область:**
  - личная база — решения пользователя, Knowledge и Rules по флагам, Human только администратору;
  - чужое решение из `scope.solution` или `X-Zam-Solution` — отдельная цель (база владельца, одно пространство), если роль `Member`; иначе исключается с `warnings: ["solution access pending|rejected|revoked"]`;
  - `all_solutions` — все доступные;
  - `scope.spaces` — по тем же правилам.
- **Каналы:**
  - FTS — по каждой цели только по её пространствам, затем `merge_channels`;
  - вектор — один запрос с `scopes` всех целей без Human;
  - хиты направляются в базу владельца (`user_id`) для `live_chunks` с пространствами цели. Все последующие чтения группируются по цели, `pending_count` суммируется.
- **Элемент выдачи** получает `agent_solution_id`, если он из решения.

- [ ] **Step 1: Тесты.**
  - unit `merge_orders_by_raw_score_across_dbs`, `merge_keeps_top_k_per_channel`;
  - `member_search_finds_owner_solution_and_own_knowledge`;
  - `member_search_never_returns_owner_knowledge` (одинаковое слово в Knowledge владельца — не в выдаче ни в FTS, ни в векторном канале; Review Focus 2);
  - `revoked_solution_excluded_with_warning` (Review Focus 1);
  - `path_prefix_within_solution_only`;
  - `degraded_fts_only_multi_db`;
  - `expand_section_and_neighbors_in_owner_db`;
  - регрессия `tests/{search,hybrid}.rs`.
- [ ] **Step 2:** Реализация, `cargo test -p zam-store -p zam-search -p zam-server --no-fail-fast`, clippy, fmt. Коммит `feat(search): поиск по личной памяти и общим решениям владельцев одним запросом`.

### Task 6: Записи журналов — схема, хранилище, чанки, индексация, выдача в поиске

**Files:**
- Create: `crates/zam-store/migrations/user/0008_entries.sql`, `crates/zam-store/src/entries/{mod.rs,write.rs,read.rs,chunks.rs}`, `crates/zam-store/tests/entries.rs`
- Modify: `crates/zam-store/src/{jobs.rs,search.rs,lib.rs}`, `crates/zam-index/src/worker.rs`, `crates/zam-server/src/{attrib.rs (X-Zam-Area),api/search/mod.rs}`
- Test: `crates/zam-server/tests/entries_index.rs`

**Interfaces:**
- Consumes: `TargetRef`, `Attribution.user_id` (задача 2), `PointPayload` (задача 4), `FtsHit.target` (задача 5).
- Схема `0008_entries.sql` (столбцы `certainty`/`reason` закладываются сразу, правила — задача 9):
  ```sql
  CREATE TABLE entries (
      id TEXT PRIMARY KEY, space_id TEXT NOT NULL REFERENCES spaces (id),
      kind TEXT NOT NULL CHECK (kind IN ('changelog', 'release', 'insight')),
      title TEXT NOT NULL, title_norm TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
      data TEXT NOT NULL DEFAULT '{}', tags TEXT NOT NULL DEFAULT '[]', area TEXT,
      status TEXT CHECK (status IN ('active', 'revisit', 'superseded', 'retracted')),
      certainty TEXT CHECK (certainty IN ('axiom', 'theorem', 'hypothesis')),
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
      body TEXT NOT NULL, data TEXT NOT NULL, tags TEXT NOT NULL, area TEXT, status TEXT, certainty TEXT,
      reason TEXT /*причина смены статуса или certainty*/, ts TEXT NOT NULL,
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
  pub enum Certainty { Axiom, Theorem, Hypothesis }   // serde lowercase; правила — задача 9
  pub struct AuthorView { pub kind: AuthorKind, pub user_id: Option<Id>, pub token_id: Option<Id>, pub session_id: Option<String>, pub machine: Option<String> }
  pub struct NewEntry { pub space_id: Id, pub kind: EntryKind, pub title: String, pub body: String, pub data: serde_json::Value,
                        pub area: Option<String>, pub tags: Vec<String>, pub status: Option<EntryStatus>, pub certainty: Option<Certainty>,
                        pub idempotency_key: Option<String> }
  pub struct Entry { pub id: Id, pub space_id: Id, pub kind: EntryKind, pub title: String, pub body: String, pub data: serde_json::Value,
                     pub tags: Vec<String>, pub area: Option<String>, pub status: Option<EntryStatus>, pub certainty: Option<Certainty>,
                     pub supersedes_id: Option<Id>, pub superseded_by_id: Option<Id>, pub retract_reason: Option<String>, pub rev: i64,
                     pub author: AuthorView, pub index_state: IndexState, pub created_at: Timestamp, pub updated_at: Timestamp, pub deleted_at: Option<Timestamp> }
  pub struct EntryRevision { pub rev: i64, pub title: String, pub body: String, pub data: serde_json::Value, pub tags: Vec<String>, pub area: Option<String>,
                             pub status: Option<EntryStatus>, pub certainty: Option<Certainty>, pub reason: Option<String>, pub ts: Timestamp, pub author: AuthorView }
  pub struct EntryPatch { pub title: Option<String>, pub body: Option<String>, pub append_body: Option<String>, pub data: Option<serde_json::Value>,
                          pub tags: Option<Vec<String>>, pub area: Option<Option<String>>, pub status: Option<EntryStatus /*Active|Revisit*/>,
                          pub certainty: Option<Certainty>, pub reason: Option<String> }
  pub struct EntryFilter { pub space_id: Id, pub kind: Option<EntryKind>, pub statuses: Vec<EntryStatus>, pub certainty: Vec<Certainty>, pub area: Option<String>,
                           pub tag: Option<String>, pub since: Option<Timestamp>, pub until: Option<Timestamp>, pub cursor: Option<String>, pub limit: usize /*1..=200*/ }
  pub async fn create_entry(db: &Db, e: NewEntry, a: &Attribution) -> Result<(Entry, bool /*создана*/), StoreError>; // повтор Idempotency-Key → (та же, false)
  pub async fn get_entry(db: &Db, id: Id) -> Result<Entry, StoreError>;
  pub async fn list_entries(db: &Db, f: EntryFilter) -> Result<(Vec<Entry>, Option<String>), StoreError>;
  pub async fn update_entry(db: &Db, id: Id, if_rev: Option<i64>, p: EntryPatch, a: &Attribution) -> Result<Entry, StoreError>;
  pub async fn delete_entry(db: &Db, id: Id, a: &Attribution) -> Result<(), StoreError>;
  pub async fn restore_entry(db: &Db, id: Id, a: &Attribution) -> Result<Entry, StoreError>;
  pub async fn entry_revisions(db: &Db, id: Id) -> Result<Vec<EntryRevision>, StoreError>;
  pub async fn supersede_entry(db: &Db, old: Id, new: NewEntry, a: &Attribution) -> Result<(Entry /*новая*/, Entry /*старая*/), StoreError>; // одна транзакция
  pub async fn retract_entry(db: &Db, id: Id, reason: &str, a: &Attribution) -> Result<Entry, StoreError>; // пустая причина → BadInput
  pub fn normalize_title(s: &str) -> String;  // нижний регистр, пунктуация → пробел, пробелы схлопнуты
  ```
  Хранилище проверяет только целостность (переходы статусов insight, непустые причины). Права на `certainty` проверяет сервер (задача 9).
- **Чанки записи:** документ `# <title>\n\n<body>` → `chunk_note`, `context_prefix` — `[<пространство>/<kind>] <title> | <теги>`, в FTS — уровни `chunk` и `note`. Задание `upsert` с `target_kind = 'entry'` — в той же транзакции; удаление → `delete`; смена статуса или `certainty` → `upsert` (payload).
- **Payload:** `entry_id`, `kind`, `insight_kind = data.kind`, `status`, `certainty`, `area`.
- **Поиск:**
  - элемент записи: `entry_id`, `kind`, `status`, `insight_kind`, `certainty`, `title`;
  - карточка `md` по §9.3: `i:<id> insight/<kind> «<title>»  [score] <status> · <certainty> · ~N tok`, у changelog — `c:`, у release — `r:`;
  - `filters.kinds` (по умолчанию `note, insight, changelog, release`), `filters.status` (по умолчанию `active, revisit`), `include_history` снимает фильтр статуса;
  - фильтр — в Qdrant и в `live_chunks`.
- **`X-Zam-Area`** (до 128 печатных символов) — область записи по умолчанию для узлов и записей (`Attribution.area`).

- [ ] **Step 1: Тесты `tests/entries.rs`:**
  - `create_entry_chunks_fts_and_job`;
  - `idempotency_key_returns_same_entry`;
  - `patch_creates_revision_with_reason_and_enqueues`;
  - `append_body_appends_with_blank_line`;
  - `entry_revisions_append_only`;
  - `supersede_marks_old_links_new_one_tx`;
  - `retract_requires_reason`;
  - `superseded_cannot_return_to_active`;
  - `deleted_entry_not_in_fts_restore_returns`;
  - `list_entries_cursor_and_filters`;
  - `migration_v7_to_v8_keeps_data`.
- [ ] **Step 2: Тесты `tests/entries_index.rs`** (MiniLM + Qdrant):
  - `entry_indexed_with_payload`;
  - `search_returns_entry_items_with_kind_status`;
  - `default_excludes_superseded_and_retracted_include_history_returns`;
  - `status_change_updates_payload`.
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(store): записи журналов — хранение, ревизии, чанки, индексация и выдача в поиске`.

### Task 7: Changelog и Releases — REST, сравнение релизов, CLI

**Files:**
- Create: `crates/zam-server/src/api/{entries.rs,releases.rs}`, `crates/zam-cli/src/cmd/{log.rs,release.rs}`
- Modify: `crates/zam-server/src/api/mod.rs`, `crates/zam-cli/src/main.rs`
- Test: `crates/zam-server/tests/journals.rs`, `crates/zam-cli/tests/journals.rs`

**Interfaces:**
- Consumes: задача 6, `space_scope`/`effective_write` (задача 3).
- Produces: `pub async fn entry_scope(st, p, h, raw_id, mode) -> Result<EntryScope { user, owner, db, space, entry, role }, ApiError>` — то же правило, что `node_scope` (в чужой базе — только записи пространства решения).
- **REST:**
  - `GET /spaces/{space}/entries?kind=&status=&certainty=&area=&tag=&since=&until=&cursor=&limit=`;
  - `POST /spaces/{space}/entries` (`Idempotency-Key`): 201, повтор — 200;
  - `GET/PATCH/DELETE /entries/{id}` (необязательный `If-Match: <rev>`, расхождение → 409), `POST /entries/{id}/restore`, `GET /entries/{id}/revisions`;
  - `GET /releases/{id}/compare?with=prev|<id>` → `{from: Entry|null, to, changelog: {feat: [...], …}, notes_diff, commits: [{sha, subject}]}`.
- **Проверки:**
  - changelog: `data.kind ∈ {feat, fix, refactor, docs, research, ops, other}`, `data.refs {commits, files, notes, issues}` — массивы строк до 200;
  - release: `data.version` (непустая, до 64 символов, уникальна в пространстве → 409), `data.date` (YYYY-MM-DD, по умолчанию сегодня UTC), `data.git_tag?`, `data.commits?` (до 5000);
  - changelog и release — только в пространстве решения, иначе 422; тело — `note_body_limit`.
- **Сравнение:**
  - интервал — changelog с `created_at` в `(from.created_at, to.created_at]`;
  - у первого релиза `with=prev` берёт начало журнала (`from: null`) **[решение исполнителя]**;
  - дифф — `unified_diff`; коммиты — из `to.data.commits`.
- **CLI:**
  - `zam log changelog --kind K --title T [--body-file F | stdin] [--ref commit:SHA|file:P|note:ID|issue:X]… [--area A]`;
  - `zam release add <version> [--date D] [--tag T] [--notes-file F] [--from-git [FROM..TO]]` (`git log --format=%H%x09%s FROM..TO`; без диапазона — от предыдущего тега до `HEAD`);
  - `zam release compare <version|id> [--with prev|<version>]`.

- [ ] **Step 1: Тесты сервера:**
  - `changelog_crud_idempotency_and_revisions`;
  - `changelog_kind_validated_422`;
  - `release_compare_groups_by_kind_and_diffs_notes`;
  - `compare_with_explicit_id_and_first_release`;
  - `release_version_unique_409`;
  - `changelog_outside_solution_422`;
  - `ro_member_cannot_log_403`;
  - `member_entry_written_to_owner_db_attributed_to_member`;
  - `member_cannot_read_owner_knowledge_entry_404`.
- [ ] **Step 2: Тест CLI** `release_from_git_collects_commits` (git-репозиторий в `CARGO_TARGET_TMPDIR`, два тега, три коммита).
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(journals): Changelog и Releases — API, сравнение релизов, zam log/release`.

### Task 8: Insights — статусы, замещение, отзыв, дедупликация, триггеры, поправки поиска

**Files:**
- Create: `crates/zam-server/src/api/{insights.rs,triggers.rs}`, `crates/zam-store/src/entries/triggers.rs`, `crates/zam-cli/src/cmd/insight.rs`
- Modify: `crates/zam-search/src/adjust.rs`, `crates/zam-server/src/{config.rs,api/search/mod.rs,api/mod.rs}`, `crates/zam-store/src/search.rs` (`FtsQuery.area`), `deploy/config.example.toml`
- Test: `crates/zam-server/tests/insights.rs`, unit-тесты `adjust.rs` и `triggers.rs`, `crates/zam-cli/tests/insights.rs`

**Interfaces:**
- Consumes: задачи 4 (`nearest_dense`), 6, 7 (`entry_scope`, маршруты записей).
- Produces:
  ```rust
  pub enum TriggerKind { FileGlob, CommandRegex, ErrorRegex, Keyword }
  pub struct NewTrigger { pub kind: TriggerKind, pub pattern: String, pub tools: Option<Vec<String>> }
  pub struct TriggerRow { pub id: Id, pub entry_id: Id, pub kind: TriggerKind, pub pattern: String, pub tools: Option<Vec<String>>,
                          pub title: String, pub insight_kind: String, pub certainty: Option<Certainty>, pub space_id: Id }
  pub fn validate_trigger(t: &NewTrigger) -> Result<(), String>; // globset::Glob; regex::RegexBuilder size_limit 1 МиБ; keyword 1..=200; pattern ≤ 1000; tools ≤ 16
  pub async fn set_triggers(db: &Db, entry: Id, ts: Vec<NewTrigger>) -> Result<(), StoreError>; // ≤ 32 на запись
  pub async fn active_triggers(db: &Db, space_ids: &[Id]) -> Result<Vec<TriggerRow>, StoreError>; // только active|revisit, живые
  pub struct DedupCandidate { pub id: Id, pub title: String, pub status: EntryStatus, pub certainty: Option<Certainty>, pub reason: &'static str /*title|vector*/, pub score: Option<f32> }
  // zam-search
  pub struct Boosts { pub insight_kinds: HashMap<String, f64>, pub certainty: HashMap<String, f64> /*заполняет задача 9*/, pub area: Option<(String, f64)> }
  pub fn apply_boosts(r: &mut [Ranked], meta: &HashMap<String, ChunkMeta>, b: &Boosts);
  ```
- **Создание insight** (`POST /spaces/{space}/entries` с `kind: insight` или `zam log insight`; поля §7.5 плюс `force`, `supersedes_id`):
  1. `scope: global` → запись идёт в Knowledge пользователя запроса (§7.5).
  2. Дедупликация (без `force` и `supersedes_id`):
     - совпадение `title_norm` с `active`/`revisit` той же области;
     - при доступном векторном канале — `embed_query(title + "\n" + body)` (первые 512 токенов) → `nearest_dense(scopes = [область пространства], kinds = [insight], exclude_statuses = [superseded, retracted], area)`, косинус ≥ `insights.dedup_threshold` (0,90);
     - кандидаты есть → 409 `/problems/possible-duplicate` с `candidates` (до 5) и `dedup: "full" | "degraded"`;
     - без модели — только заголовок и `dedup: "degraded"`, в том числе в успешном ответе.
  3. `supersedes_id` → `supersede_entry`.
  4. «Дописать» — `PATCH /entries/{id} {append_body}`.
- **Операции:**
  - `POST /entries/{id}/supersede {новая запись}`;
  - `POST /entries/{id}/retract {reason}` (пустая → 422);
  - `PATCH {status: "revisit"|"active"}`; из `superseded`/`retracted` → 422 **[решение исполнителя]**;
  - `PATCH {triggers}`.
- **`GET /triggers?solution=<guid>`** — триггеры `active`/`revisit` доступного решения (в базе владельца) и Knowledge пользователя: `{etag, triggers: [{id, entry_id, type, pattern, tools, title, insight_kind, certainty, space}]}`, `ETag` — blake3 канонического JSON, `If-None-Match` → 304. Сопоставление в CLI — M4.
- **Поправки поиска (§9.2 п. 4):**
  - `search.boost_gotcha = 1.15`, `boost_failure = 1.15`, `boost_area = 1.10` (текущая область — `X-Zam-Area`);
  - `scope.area` — строгий фильтр;
  - значения **[решение исполнителя]**, проверка — сетка задачи 18.
- **`POST /entries/{id}/feedback`** и `usage` — M4 (§10.5).
- **CLI:**
  - `zam log insight --kind K --title T [--scope solution|global] [--certainty theorem|hypothesis] [--evidence-file F] [--trigger file_glob:**/*.sh]… [--tag]… [--force] [--supersedes ID] < body.md` (409 печатает кандидатов и подсказки);
  - `zam insight list|show|supersede|retract --reason|revisit|append`.

- [ ] **Step 1: Unit-тесты:**
  - `validate_trigger_rejects_bad_glob_regex_and_lengths`;
  - `regex_without_backrefs_is_only_dialect`;
  - `boost_gotcha_orders_above_equal_note`;
  - `area_boost_multiplies_only_matching`;
  - `normalize_title_cases`.
- [ ] **Step 2: Тесты сервера** (MiniLM + Qdrant):
  - `dedup_title_409_with_candidates`;
  - `dedup_vector_409`;
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
  - `member_triggers_only_solution_and_own_knowledge`;
  - `strict_area_filter`.
- [ ] **Step 3: Тест CLI** `insight_log_with_triggers_dedup_and_supersede`. Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(journals): Insights — статусы, замещение, отзыв, дедупликация, триггеры, бусты поиска`.

### Task 9: Эпистемический статус `certainty` — insights и заметки, человеческий токен, поиск

**Files:**
- Create: `crates/zam-store/migrations/system/0004_human_tokens.sql`, `crates/zam-store/migrations/user/0009_node_certainty.sql`, `crates/zam-server/src/certainty.rs`, `crates/zam-server/tests/certainty.rs`
- Modify:
  - `crates/zam-markdown/src/frontmatter.rs` (`certainty`, `evidence`);
  - `crates/zam-store/src/{system.rs (AuthToken.human, issue_token),nodes/chunks.rs (nodes.certainty),jobs.rs (payload)}`;
  - `crates/zam-server/src/{attrib.rs,auth/mod.rs,api/{admin.rs,insights.rs,entries.rs,nodes/write.rs,search/mod.rs,triggers.rs}}`, `crates/zam-search/src/adjust.rs`;
  - `crates/zam-cli/src/{locate.rs (human_token в ~/.zeroaimemory/config.toml),cmd/insight.rs}`.

**Interfaces:**
- Consumes: `Certainty`, `EntryPatch.certainty/reason` (задача 6), insights (задача 8), `Boosts.certainty` (задача 8).
- Схемы:
  - `0004_human_tokens.sql`: `ALTER TABLE tokens ADD COLUMN human INTEGER NOT NULL DEFAULT 0 CHECK (human IN (0,1));`
  - `0009_node_certainty.sql`: `ALTER TABLE nodes ADD COLUMN certainty TEXT CHECK (certainty IN ('axiom','theorem','hypothesis'));` плюс `meta.certainty_backfill = pending` — при открытии базы значение заполняется из frontmatter живых заметок, заметкам с `certainty` ставится `upsert`.
- Produces:
  ```rust
  pub struct Evidence { pub checked: String, pub method: String, pub result: String }   // все три непустые
  pub enum CertaintyChange { Create(Option<Certainty>), Transition { from: Option<Certainty>, to: Option<Certainty> }, ModifyOther /*правка, supersede, retract, delete, move*/ }
  pub fn check_certainty(author: AuthorKind, current: Option<Certainty>, change: &CertaintyChange,
                         evidence: Option<&Evidence>, reason: Option<&str>) -> Result<(), CertaintyError>;
  pub enum CertaintyError { AxiomRequiresHuman /*403*/, EvidenceRequired /*422*/, ReasonRequired /*422*/, InvalidTransition /*422*/ }
  pub fn default_certainty(author: AuthorKind, kind: EntryKind) -> Option<Certainty>; // insight: agent → Hypothesis, human → Axiom; иначе None
  // zam-markdown Frontmatter: pub certainty: Option<String>, pub evidence: Option<String>
  // zam-store: AuthToken { …, pub human: bool }; issue_token(…, human: bool)
  ```
- **Матрица прав** (`author_kind = agent` — токен без флага `human`; человек — сессия WebUI или токен с `human = 1`):

  | Действие | Агент | Человек |
  |---|---|---|
  | создать `hypothesis` | да (по умолчанию для insight агента) | да |
  | создать `theorem` | только с `data.evidence {checked, method, result}` | да |
  | создать `axiom` | 403 | да (по умолчанию для insight человека **[решение исполнителя]**) |
  | `hypothesis → theorem` | только с `evidence` и `reason` | да, с `reason` |
  | `theorem → hypothesis` | с `reason` (пересмотр) | да, с `reason` |
  | `* → axiom` | 403 | да, с `reason` |
  | любое изменение записи или заметки `axiom` (правка, смена certainty, supersede, retract, delete, move) | 403 | да |
  | снять certainty (`→ null`) | только у `hypothesis`, с `reason` | да |

  Каждая смена `certainty` — новая ревизия с `reason` в `entry_revisions`, у заметок — сообщение ревизии (обязательно).
  
  Пример владельца: метод из статьи не сработал → hypothesis «метод нерабочий». Перепроверка либо подтверждает (`→ theorem` с evidence), либо выясняется, что параметры были неверны (`supersede` новой записью или `retract` с причиной).
- **Заметки:**
  - `certainty:` во frontmatter; без поля — статуса нет;
  - `theorem` у агента требует непустого `evidence:` во frontmatter;
  - правило «агент не трогает `axiom`» проверяется по текущей ревизии до записи (`PUT`, `PATCH`, `move`, `delete`, `revert`).
- **Человеческий токен** **[решение исполнителя; в «Уточнить у владельца»]**:
  - выпускает только администратор: `POST /admin/users/{id}/tokens {human: true}`, в WebUI — флажок «для человека» с предупреждением не класть его в CLAUDE.md/AGENTS.md;
  - CLI берёт его из `~/.zeroaimemory/config.toml` (`human_token`) только для команд с флагом `--human` (например, `zam insight certify <id> axiom --reason R --human`);
  - аудит `token.issue` с `human: true`; в `/auth/me` и `GET /spaces` — `human: bool`.
- **Поиск:**
  - `certainty` в payload, в элементе выдачи и в карточке `md`;
  - `filters.certainty`;
  - бусты `search.boost_axiom = 1.20`, `boost_theorem = 1.10`, `boost_hypothesis = 1.00` (через `Boosts.certainty`; значения — сеткой задачи 18);
  - `GET /triggers` отдаёт `certainty`.
  
  Подсказки хуков показывают статус (M4).

- [ ] **Step 1: Unit-тесты `certainty.rs`** — `matrix_agent_vs_human` (таблица целиком, по случаю на строку и столбец); `evidence_requires_all_three_fields`; `default_certainty_by_author_and_kind`; `zam-markdown`: `frontmatter_certainty_and_evidence`.
- [ ] **Step 2: Тесты сервера** (Review Focus 3):
  - `agent_cannot_create_axiom_403`;
  - `agent_theorem_without_evidence_422`;
  - `agent_promotes_with_evidence_history_has_reason`;
  - `agent_demotes_theorem_with_reason`;
  - `agent_cannot_patch_supersede_retract_axiom_403`;
  - `agent_note_axiom_frontmatter_403_and_axiom_note_write_move_delete_403`;
  - `human_token_can_set_and_change_axiom`;
  - `webui_admin_can_set_axiom`;
  - `human_token_issued_only_by_admin`;
  - `search_shows_certainty_and_filters`;
  - `axiom_boost_orders_above_equal_hypothesis`;
  - `certainty_backfill_from_frontmatter`.
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(journals): эпистемический статус axiom/theorem/hypothesis — права, доказательства, история, поиск`.

### Task 10: Граф — `edges`, wiki-ссылки, связи frontmatter, API графа, `expand=graph`

**Files:**
- Create: `crates/zam-store/migrations/user/0010_edges.sql`, `crates/zam-store/src/{graph.rs,links.rs}`, `crates/zam-store/tests/graph.rs`, `crates/zam-server/src/api/graph.rs`, `crates/zam-cli/src/cmd/link.rs`
- Modify: `crates/zam-markdown/src/{frontmatter.rs,doc.rs}` (`relations`), `crates/zam-store/src/nodes/{chunks.rs,write.rs,revise.rs,tree_ops.rs}`, `entries/write.rs`, `crates/zam-server/src/api/{render.rs (общий резолвер),search/mod.rs (expand=graph),insights.rs (links, system supersedes)}`
- Test: `crates/zam-markdown/tests/parse.rs`, `crates/zam-server/tests/graph.rs`

**Interfaces:**
- Consumes: `TargetRef` (задача 2), записи (задача 6), `supersede_entry`.
- Схема `0010_edges.sql`:
  ```sql
  CREATE TABLE edges (
      id TEXT PRIMARY KEY, space_id TEXT NOT NULL,     -- пространство источника
      src_kind TEXT NOT NULL CHECK (src_kind IN ('node', 'entry')), src_id TEXT NOT NULL,
      dst_kind TEXT CHECK (dst_kind IN ('node', 'entry')), dst_id TEXT, dst_space_id TEXT, dst_raw TEXT, dst_anchor TEXT,
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
  pub struct EdgeView { pub id: Id, pub src: TargetRef, pub src_space: Id, pub dst: Option<TargetRef>, pub dst_space: Option<Id>,
                        pub dst_raw: Option<String>, pub dst_anchor: Option<String>, pub kind: EdgeType, pub origin: EdgeOrigin, pub created_at: Timestamp }
  pub struct GraphNode { pub r: TargetRef, pub space_id: Id, pub title: String, pub path: Option<String>,
                         pub status: Option<EntryStatus>, pub certainty: Option<Certainty>, pub deleted: bool }
  pub struct Graph { pub nodes: Vec<GraphNode>, pub edges: Vec<EdgeView> }
  pub struct NewEdge { pub src: TargetRef, pub dst: TargetRef, pub kind: EdgeType }
  pub trait GraphStore: Send + Sync {          // §8.5; `spaces` — область видимости: узлы и рёбра вне неё не возвращаются
      fn neighbors(&self, start: &[TargetRef], depth: u8 /*1..=3*/, types: &[EdgeType], spaces: &[Id]) -> impl Future<Output = Result<Graph, StoreError>> + Send;
      fn backlinks(&self, target: TargetRef, spaces: &[Id]) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;
      fn outgoing(&self, src: TargetRef, spaces: &[Id]) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;
      fn add_edge(&self, e: NewEdge, a: &Attribution) -> impl Future<Output = Result<EdgeView, StoreError>> + Send;
      fn remove_edge(&self, id: Id, a: &Attribution) -> impl Future<Output = Result<(), StoreError>> + Send;   // только explicit
  }
  impl GraphStore for Db;
  pub fn resolve_link(c: &Connection, space: Id, from_path: &str, link: &Link) -> rusqlite::Result<Option<Id>>; // links.rs — общее с рендером
  ```
- **Правила:**
  - Вики-ссылка `[[Имя#Якорь|текст]]`: сначала путь, затем `find_by_name` — первая по `path`, как сейчас в `api/render.rs::resolve_one`. Логика переносится в `zam-store::links`, рендер вызывает её же.
  - Markdown-ссылка на относительный путь — тоже `links_to`; внешние URL рёбер не дают. Неразрешённая ссылка → `dst_raw`.
  - `relations:` во frontmatter — список строк `"<тип>: <цель>"`, цель — `[[Имя]]` или путь **[решение исполнителя]**. Неизвестный тип → `warnings: ["unknown relation type: <тип>"]`.
  - Рёбра `wikilink`/`frontmatter` пересоздаются при каждой записи узла; `explicit`/`system` удаляются мягко.
  - Создание, переименование, перенос и восстановление узла разрешают ожидающие `dst_raw` пространства. `backlinks` — только живые источники.
  - Insight: `links: [{type, target: "node:<id>"|"entry:<id>"|"[[Имя]]"}]` → `explicit`; `supersede_entry` → `system supersedes` (новая → старая).
  - Цель в другой базе (личная память участника ↔ решение в базе владельца) → 422 «связи между базами — не в M3» **[решение исполнителя]**. В пределах одной базы допустимы связи между пространствами владельца; участнику они отфильтровываются (`spaces` = пространство решения — Review Focus 2).
  - Дозаполнение при открытии базы с `edges_backfill = pending` (фоновая задача по образцу R1).
- **API:**
  - `GET /graph?node=<id>|entry=<id>&depth=1..3&types=a,b`;
  - `POST /edges {src, type, dst}` (только `explicit`, `supersedes` → 422);
  - `DELETE /edges/{id}`;
  - `GET /nodes/{id}/backlinks`, `GET /entries/{id}/backlinks`, `GET /nodes/{id}/links`.
- **`expand=graph`:** соседи первых `top_k` элементов на 1 шаг в обе стороны в пределах области цели; до `ceil(top_k / 2)` новых элементов с баллом `родитель × search.graph_weight` (0,5); сначала `active`/`revisit` insights (по `certainty`: axiom, theorem, hypothesis), затем заметки; `superseded`/`retracted` — только с `include_history`; `sources: ["graph"]`, `via: {type, from}`.
- **CLI:** `zam link <src> <type> <dst>`, `zam link rm <edge-id>`, `zam links <id>`.

- [ ] **Step 1: Тесты `zam-markdown`:** `frontmatter_relations_flow_and_block`.
- [ ] **Step 2: Тесты `tests/graph.rs`:**
  - `wikilinks_create_resolved_and_unresolved`;
  - `unresolved_resolves_when_target_created`;
  - `rename_and_move_reresolve`;
  - `same_name_resolves_first_by_path_like_render`;
  - `rewrite_replaces_derived_keeps_explicit`;
  - `delete_source_hides_backlinks_restore_returns`;
  - `neighbors_depth2_cte_cycle_safe`;
  - `neighbors_respect_spaces_filter`;
  - `supersede_creates_system_edge`;
  - `backfill_existing_notes_on_open`.
- [ ] **Step 3: Тесты сервера:**
  - `graph_api_and_backlinks`;
  - `explicit_edge_crud_system_protected`;
  - `cross_db_edge_422`;
  - `member_graph_and_backlinks_hide_owner_other_spaces` (ребро из insight решения в Knowledge владельца не видно участнику ни в графе, ни в `expand=graph`);
  - `expand_graph_adds_neighbors_with_graph_source`;
  - `render_and_edges_agree_on_resolution`.
- [ ] **Step 4:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(graph): рёбра, wiki-ссылки и relations, API графа, expand=graph`.

### Task 11: Сканер секретов перед индексацией (§7.6)

**Files:**
- Create: `crates/zam-markdown/src/secrets.rs`, `crates/zam-markdown/tests/secrets.rs`
- Modify: `crates/zam-markdown/src/lib.rs`, `crates/zam-store/src/nodes/chunks.rs`, `entries/chunks.rs`, ответы записи в `crates/zam-server/src/api/{nodes/write.rs,entries.rs,insights.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub const SECRET_MARK: &str = "‹secret›";
  pub enum SecretKind { KnownPrefix(&'static str), PrivateKey, KeyValue, UrlPassword, Jwt, HighEntropy }
  pub struct SecretHit { pub kind: SecretKind, pub range: Range<usize>, pub line: usize /*с 1*/ }
  pub fn scan_secrets(text: &str) -> Vec<SecretHit>;
  pub fn mask_secrets(text: &str, hits: &[SecretHit]) -> String;
  ```
- **Правила** (`regex`, компиляция один раз):
  - Известные префиксы:
    - `AKIA|ASIA[0-9A-Z]{16}`;
    - `gh[pousr]_[A-Za-z0-9]{36}`, `github_pat_[A-Za-z0-9_]{60,}`;
    - `glpat-[A-Za-z0-9_-]{20,}`;
    - `xox[abprs]-[A-Za-z0-9-]{10,}`;
    - `sk-(ant-)?[A-Za-z0-9_-]{20,}`;
    - `AIza[0-9A-Za-z_-]{35}`;
    - токены ZeroAIMemory по формату `zam_core::token`.
  - JWT: три части от 10 символов.
  - Блок `-----BEGIN [A-Z ]*PRIVATE KEY-----` … `-----END … -----` целиком.
  - Ключ-значение: `(?i)(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret)\s*[:=]\s*["']?([^\s"'`]{6,})` — маскируется только значение. Не маскируются `<…>`, `${…}`, `{{…}}`, значения из одних `*` или `x`, `changeme`, `example`.
  - Пароль в URL: `scheme://user:<пароль>@`.
  - Высокая энтропия: `[A-Za-z0-9+/_=-]{32,}`, в котором есть строчные, заглавные и цифры, с энтропией Шеннона ≥ 4,5 бит/символ. Только hex, только цифры, UUID и base32-id (26 символов) этим правилом не помечаются никогда (Review Focus 4).
- **Где действует:**
  - текст чанков заметок и записей маскируется при нарезке, исходник не меняется;
  - ответ записи: `warnings: ["secrets_detected"]`, `secrets_detected: [{kind, line}]` без значений, с подсказкой `zam vault set`.
- **Бюджет:** ≤ 2 мс на 100 КБ (release).

- [ ] **Step 1: Тесты `tests/secrets.rs`:**
  - `detects_each_rule`;
  - `ignores_identifiers` (git SHA, sha256, UUID, `Id`, URL с query, `password=<your-password>`, `token: ${TOKEN}`);
  - `masks_only_value_in_key_value`;
  - `private_key_block_whole`;
  - property (proptest, 500 случаев): нет паники, байты вне диапазонов неизменны, границы UTF-8;
  - `#[ignore] bench_scan_100kb`.
- [ ] **Step 2: Тесты сервера:**
  - `secret_in_note_not_in_fts_nor_qdrant_text`;
  - `note_read_returns_original`;
  - `write_response_has_secrets_detected`;
  - `entry_body_secret_masked`.
- [ ] **Step 3:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(markdown): сканер секретов перед индексацией, предупреждение secrets_detected`.

### Task 12: Vault — шифрование, ключи, REST, аудит, CLI

**Files:**
- Create: `crates/zam-store/migrations/system/0005_vault_keys.sql`, `crates/zam-store/migrations/user/0011_vault.sql`, `crates/zam-store/src/vault.rs`, `crates/zam-server/src/vault/{mod.rs,keys.rs,crypto.rs}`, `crates/zam-server/src/api/vault.rs`, `crates/zam-cli/src/cmd/vault.rs`
- Modify: `crates/zam-server/src/{app.rs,config.rs ([vault] master_key_file),error.rs (ApiError::VaultLocked → 503 /problems/vault-locked),api/mod.rs}`, корневой `Cargo.toml` (`chacha20poly1305`, `zeroize`), `deploy/config.example.toml`
- Test: `crates/zam-server/tests/vault.rs`, unit-тесты `crypto.rs`/`keys.rs`, `crates/zam-cli/tests/vault.rs`

**Interfaces:**
- Consumes: `space_scope`, `Role` (задача 3), `SystemStore::audit`.
- Схемы:
  ```sql
  -- system 0005
  CREATE TABLE user_keys (user_id TEXT PRIMARY KEY REFERENCES users (id), wrapped BLOB NOT NULL, created_at TEXT NOT NULL);
  -- settings: 'vault.key_check' — зашифрованная мастер-ключом известная строка
  -- user 0011
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
      pub async fn load(cfg: &ServerConfig, sys: &SystemStore) -> anyhow::Result<VaultKeys>;   // Locked — не ошибка старта
      pub fn state(&self) -> &VaultState;
      pub async fn encrypt(&self, user: Id, item: Id, rev: i64, plain: &[u8]) -> Result<Vec<u8>, ApiError>;
      pub async fn decrypt(&self, user: Id, item: Id, rev: i64, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, ApiError>;
  }
  // zam-store vault.rs (без криптографии; id создаёт сервер и шифрует до вызова)
  pub enum VaultScope { Knowledge, Solution(SolutionGuid) }
  pub struct VaultMeta { pub id: Id, pub scope: VaultScope, pub title: String, pub data: serde_json::Value, pub rev: i64, pub author: AuthorView,
                         pub created_at: Timestamp, pub updated_at: Timestamp }
  pub async fn vault_list(db: &Db, scope: &VaultScope, q: Option<&str>) -> Result<Vec<VaultMeta>, StoreError>;
  pub async fn vault_get(db: &Db, id: Id) -> Result<(VaultMeta, Vec<u8>), StoreError>;
  pub async fn vault_put(db: &Db, id: Id, scope: &VaultScope, title: &str, data: serde_json::Value, secret_blob: Vec<u8>, a: &Attribution) -> Result<VaultMeta, StoreError>;
  pub async fn vault_update(db: &Db, id: Id, expected_rev: i64, title: Option<String>, data: Option<serde_json::Value>, secret_blob: Option<Vec<u8>>, a: &Attribution) -> Result<VaultMeta, StoreError>; // ревизия сменилась → Conflict
  pub async fn vault_delete(db: &Db, id: Id, a: &Attribution) -> Result<(), StoreError>;
  ```
- **Криптография.**
  - XChaCha20-Poly1305. Блоб — `0x01 ‖ nonce(24) ‖ ciphertext‖tag`, AAD — `b"zam-vault-v1" ‖ user_id ‖ item_id ‖ rev`.
  - Ключ пользователя — 32 случайных байта, обёрнут мастер-ключом с AAD `b"zam-ukey-v1" ‖ user_id`.
  - `vault.key_check` — шифротекст `b"zam-key-check"`.
- **Мастер-ключ:**
  - `ZAM_MASTER_KEY` (base64, 32 байта), иначе `[vault] master_key_file` (по умолчанию `<data_dir>/master.key`);
  - файл создаётся только при первом запуске, когда нет ни `user_keys`, ни `key_check`; на Linux права 0600;
  - не прошёл `key_check`, или файла нет при существующих ключах → `Locked`, WARN одной строкой, Vault отвечает 503; остальной сервер работает (Review Focus 5).
- **REST (§12):**
  - `GET /spaces/{space}/vault?q=` — только метаданные. `space` = `knowledge` или GUID доступного решения (своё или участника): записи **пользователя запроса** в его собственной базе с привязкой к решению; Vault владельца и других участников не виден никому, кроме администратора с `X-Zam-User`. `rules`/`human` → 404.
  - `POST /spaces/{space}/vault {title, data: {type, username?, url?, description?}, secret}` (только `rw`): 201, занятое имя → 409.
  - `GET /vault/{id}`; `GET /vault/{id}?reveal=1` + аудит `vault.reveal` (`{item, session, machine}`, `actor` — id токена).
  - `PATCH /vault/{id}`: новый `secret` → новая ревизия.
  - `DELETE /vault/{id}` (мягко, аудит).
  - `GET /vault/{id}/revisions`: раскрытие старой ревизии — только администратору.
  - `data.type ∈ {password, token, ssh_key, connection_string, url, repo, note}`.
- **CLI:**
  - `zam vault list [--q Q] [--global]`;
  - `zam vault get <имя> [--global] [--raw]` — в stdout только значение;
  - `zam vault set <имя> --type T [--username U] [--url URL] [--description D] [--global]` — значение только из stdin или скрытого ввода.

- [ ] **Step 1: Unit-тесты:**
  - `encrypt_decrypt_roundtrip`;
  - `aad_binding_swap_blob_fails`;
  - `wrapped_user_key_roundtrip`;
  - `master_key_created_once_with_0600_on_linux`;
  - `env_master_key_has_priority`.
- [ ] **Step 2: Тесты сервера:**
  - `agent_sets_and_reveals_secret_audited`;
  - `vault_list_never_returns_values`;
  - `vault_values_not_in_fts_qdrant_logs`;
  - `wrong_master_key_locks_vault_server_works`;
  - `missing_key_file_with_existing_keys_no_regen`;
  - `member_vault_for_shared_solution_in_member_db` (у B запись в `users/B.db` с `agent_solution_id` решения A; A её не видит; `GET /vault/{id_A}` по токену B → 404);
  - `ro_token_cannot_set_403`;
  - `title_unique_per_scope_409`;
  - `copy_without_master_key_reveals_nothing`.
- [ ] **Step 3: Тест CLI** `vault_set_from_stdin_get_raw`. Реализация, тесты (`--no-fail-fast`), clippy, fmt; аудит лицензий `chacha20poly1305` и `zeroize` — в журнал. Коммит `feat(vault): шифрование XChaCha20-Poly1305, мастер-ключ, API и аудит раскрытия, zam vault`.

### Task 13: WebUI — журналы, эпистемический статус, Vault, связи, совместный доступ, настройки

**Files:**
- Create: `web/src/components/{ChangelogView,ReleasesView,InsightsView,VaultView,Links}.svelte`, `web/src/routes/{Solutions,Settings}.svelte`, `web/e2e/{journals,access}.spec.ts`
- Modify: `web/src/lib/{api.ts,router.ts}`, `web/src/components/{Tree,DocView,Search}.svelte`, `web/src/routes/{Shell,Users}.svelte`, `web/e2e/seed.ts`

**Interfaces:**
- Consumes: REST задач 3, 6–10 и 12.
- **Поведение:**
  - **Дерево:** у решений метка роли (`владелец`, `общее`), у чужих решений, доступных пользователю, — `участник rw/ro` с владельцем. Сверху у решения — `Changelog`, `Releases`, `Insights`, `Vault`, затем дерево; у Knowledge — `Insights` и `Vault`.
  - **Changelog:** лента с фильтром по `kind` и области, курсор.
  - **Releases:** список и сравнение (группы changelog, `DiffView`, коммиты).
  - **Insights:**
    - таблица с фильтрами (вид, статус, `certainty`, область, теги) и значком статуса (axiom, theorem, hypothesis);
    - действия «Заменить», «Отозвать», «Пересмотреть», «Сменить статус надёжности» (причина обязательна; для `theorem` — форма evidence);
    - правка триггеров; цепочка замещения;
    - администратор в WebUI — человек, ему доступна `axiom`.
  - **Vault:** маска, «Показать» (`?reveal=1`, «раскрытие записывается в аудит»), баннер `vault-locked`.
  - **Вкладка «Связи»:** исходящие (с неразрешёнными) и обратные ссылки списком.
  - **Пользователи:** выпуск токена с флажком «для человека» и предупреждением.
  - **Администрирование → «Решения»:** реестр, владелец, участники, запросы (одобрить `rw`/`ro`, отклонить с причиной, отозвать).
  - **Администрирование → «Настройки»:** `rules.agent_write`.
  - **Поиск Ctrl+K:** карточки записей со статусом и `certainty`, предупреждения о доступе.
- **Бюджет:** начальный бандл ≤ 300 КБ gzip, новых npm-зависимостей нет.

- [ ] **Step 1: e2e** (`seed.ts`: решение, записи разной надёжности, секрет, второй пользователь с запросом):
  - `journals.spec.ts`: лента фильтруется; сравнение релизов; замена insight; повышение до `theorem` с evidence; секрет раскрывается; «Связи» с backlink;
  - `access.spec.ts`: одобрение `ro`, отзыв; человеческий токен; `rules.agent_write`.
  
  Снимки — в `.tmp/e2e/screens/`.
- [ ] **Step 2:** `npm run check`, `build`, `size`, `e2e`. Коммит `feat(web): журналы, надёжность записей, Vault, связи, доступ к решениям, настройки`.

### Task 14: WebUI — раскладка ELK для Mermaid (`layout: elk`)

**Files:**
- Modify: `web/vite.config.ts` (убрать алиас `elkjs/lib/elk.bundled.js` → заглушка; разрешить EPL-2.0 точечно), `web/src/lib/hydrate/mermaid.ts`, `web/e2e/notes.spec.ts` (или новый `mermaid.spec.ts`), `README.md` (раздел о лицензиях WebUI)
- Delete: `web/src/lib/hydrate/elk-stub.ts`

**Interfaces:**
- **Выбор пакета (аудит в журнал).** `elkjs` уже входит в зависимости `mermaid` 12 (`package-lock.json`, `^0.9.3`) и подгружается Mermaid динамически только для `layout: elk`. Отдельный `@mermaid-js/layout-elk` ставится, только если Mermaid 12 без него ELK не регистрирует — проверить по исходнику установленной версии и записать вывод.
- **Лицензия:**
  - в `vite.config.ts` разрешение EPL-2.0 даётся **только пакету `elkjs`** (проверка по имени и лицензии), а не всему списку `ALLOWED`;
  - `THIRD-PARTY-NOTICES.txt` содержит текст EPL-2.0 и ссылку на исходники `https://github.com/kieler/elkjs` с версией (EPL-2.0 §3 — доступность исходного кода);
  - `README.md` — абзац о EPL-2.0 в бандле WebUI (решение владельца 2026-10-21).
- **Поведение:**
  - раскладка по умолчанию остаётся `dagre`;
  - диаграмма с `layout: elk` (в frontmatter Mermaid или `%%{init}%%`) рисуется ELK;
  - код ELK — в отдельном ленивом чанке, загружается только при первой такой диаграмме.
- **Бюджет:** начальный бандл не растёт (сравнение `npm run size` до и после — в отчёте задачи); размер ленивого чанка ELK — в журнал.

- [ ] **Step 1: e2e** `mermaid_elk_layout_renders` (диаграмма `layout: elk` даёт SVG без пометки об ошибке; без `elk` чанк ELK не запрашивается — проверка по сетевым запросам Playwright).
- [ ] **Step 2:** `npm run check`, `build`, `size`, `e2e`; проверить, что сборка падает на другом пакете с EPL-2.0 (временный тест-пакет не нужен — unit-проверка функции разрешения в `vite.config.ts`, если она вынесена). Коммит `feat(web): раскладка ELK для Mermaid (elkjs, EPL-2.0, ленивый чанк)`.

### Task 15: Поиск по точкам папок (§8.3, §9.2)

**Files:**
- Modify: `crates/zam-index/src/qdrant.rs`, `crates/zam-search/src/hybrid.rs`, `crates/zam-store/src/search.rs` (`live_folders`), `crates/zam-server/src/{api/search/mod.rs,config.rs}` (`[search] w_folder`, `max_folders`)
- Test: unit `zam-search`, `crates/zam-server/tests/folders_search.rs`

**Interfaces:**
- Consumes: `VectorHit` (задача 4), `fuse`.
- Produces:
  ```rust
  pub struct FolderHit { pub folder: Id, pub path_prefix: String, pub rank: usize }
  pub struct SearchInputs { /* поля M2 */ pub folders: Vec<FolderHit> }
  // fuse: чанку узла под path_prefix папки прибавляется w_folder / (rrf_k + rank) — один раз, по лучшей папке-предку
  pub struct FolderRow { pub id: Id, pub space_id: Id, pub path: String, pub l0: Option<String>, pub child_count: i64 }
  pub async fn live_folders(db: &Db, ids: Vec<Id>, space_ids: &[Id], path_prefix: Option<&str>) -> Result<Vec<FolderRow>, StoreError>;
  ```
- **Поведение:**
  - prefetch dense top-10 — по `note | section | folder`; точки папок проверяются по SQLite (живы, не Human, в области цели);
  - папка поднимает листы своих потомков каналом `folder`;
  - карточка `f:<id> <space>/<path>/  [score] · <N> детей` — только при `filters.kinds ∋ folder`, не больше `search.max_folders` (2);
  - `w_folder` выбирается сеткой задачи 18 из `{0, 0.3, 0.5, 1.0}`; без значимого выигрыша — 0.

- [ ] **Step 1: Тесты:**
  - unit `folder_channel_boosts_descendants_once`, `no_folder_hits_equals_m2_order`;
  - сервер: `broad_query_lifts_notes_under_matching_folder`, `folder_cards_only_with_kind_folder_and_capped`, `deleted_or_human_folder_ignored`, `member_sees_only_solution_folders`.
- [ ] **Step 2:** Реализация, тесты (`--no-fail-fast`), clippy, fmt. Коммит `feat(search): точки папок — канал иерархического вспоминания и карточки папок`.

### Task 16: Eval-набор M3 и метрики — устаревание, надёжность, решения, FTS-only, длинные заметки

**Files:**
- Create: `eval/insights.toml`, `eval/corpus/{ru,en}/long/*.md` (длинные заметки)
- Modify: `eval/queries.toml`, `eval/README.md`, `xtask/src/eval/{corpus.rs,run.rs,metrics.rs,report.rs}`, корпус (frontmatter `index: false` у выбранных заметок)

**Interfaces:**
- Consumes: REST записей, insights, `certainty` (задачи 7, 8, 9), `scope.solution` (задача 5).
- Produces (общий набор для задач 17 и 18):
  ```rust
  pub struct Query { /* поля M2 */ pub forbid: Vec<String>, pub weaker: Vec<String>, pub evidence: Option<String> /*дословная подстрока заметки*/ }
  pub struct QueryMetrics { /* поля M2 */ pub cancelled5: Option<bool>, pub reliable_first: Option<bool>, pub chunk_recall5: Option<f64>, pub chunk_ndcg10: Option<f64> }
  ```
- **Набор:**
  - `eval/insights.toml` — `[[insight]] key, lang, kind, scope, area?, certainty?, evidence?, title, body, triggers?, supersedes = "<key>"?, retract_reason?`:
    - не меньше 48 инсайтов;
    - ≥ 12 цепочек замещения и ≥ 4 отзыва;
    - ≥ 12 пар противоречий «hypothesis против theorem или axiom» на одну тему (например, «метод X не работает» — hypothesis, «метод X работает при параметрах P» — theorem с evidence).
    
    Записи `axiom` создаёт загрузчик человеческим токеном, выпущенным администратором прогона.
  - **Длинные заметки** (исследование `docs/research/2026-10-21-chunking-semantic.md` §5, шаг 1): 20–30 заметок по 2–5 тыс. слов (ru и en) с H4–H6, длинными бесструктурными абзацами, вводными фразами («Пример:», «Выполните:») перед кодом и списками, сменой темы внутри секции.
  - В `queries.toml`:
    - ≥ 30 запросов `kind = "superseded"` (`expect` новый, `forbid` старый или отозванный);
    - ≥ 12 запросов `kind = "certainty"` (`expect` надёжный, `weaker` — противоречащая гипотеза);
    - 40–60 запросов `kind = "chunk"` к фактам из середины длинных заметок с полем `evidence` (дословная подстрока-доказательство).
  - Заметки группы `e` пишутся в решение с фиксированным GUID набора, поиск идёт с `scope.solution`.
  - ≥ 10 заметок с `index: false` и запросами `exact` к ним (FTS-only находки — открытый пункт M2).
- **Метрики:**
  - `cancelled@5` — доля запросов `superseded` с `forbid` в top-5 (цель 0); контроль с `include_history: true` — `cancelled@5 > 0`, иначе `eval --check` падает;
  - `reliable_first` — доля запросов `certainty`, где `expect` выше `weaker` (отчёт с бустами и без);
  - `chunk-recall@5` — доля запросов `chunk`, у которых среди топ-5 листов (`mode = chunks`, без учёта перекрытия) есть лист, целиком содержащий `evidence`; `chunk-nDCG@10`;
  - recall@5, MRR@10, nDCG@10 по видам и по подмножеству «длинные заметки» — в отчёте и JSON с бутстреп-интервалами.
- **Базовый прогон** (код до задачи 17, hybrid fp16 CUDA) → `eval/results/<дата>-m3-base-*`.
- **Чувствительность набора:** сетка перекрытия 0/1/2 предложения должна давать ненулевые разности `chunk-recall@5`. Если не даёт, набор длинных заметок усиливается до закрытия задачи.
- **Сравнение с M2:** парная разность nDCG@10 по неизменённым id запросов против `eval/results/2026-10-20-*`. Интервал целиком ниже нуля — дефект, разбирается в задаче.

- [ ] **Step 1:** Набор; тест `eval_set_is_valid` (расширить):
  - ключи `forbid`, `weaker`, `supersedes` существуют;
  - цепочки без циклов;
  - у `theorem` есть `evidence`;
  - `evidence` запроса `chunk` дословно есть в ожидаемой заметке;
  - отпечаток обновлён.
- [ ] **Step 2:** Загрузчик записей (с человеческим токеном для `axiom`) и метрики; unit-тесты `cancelled_at_5_counts_forbidden_in_top5`, `reliable_first_compares_ranks`, `chunk_recall_requires_whole_evidence_in_leaf`.
- [ ] **Step 3:** Базовый прогон и проверка чувствительности; итоги — `eval/results/` и журнал. Коммит `feat(eval): набор M3 — устаревание, надёжность, решения, FTS-only, длинные заметки, chunk-recall`.

### Task 17: Нарезка с учётом силы границ

**Files:**
- Modify:
  - `crates/zam-markdown/src/chunk.rs` (`top_blocks`, `pack`, `heading_path`), `crates/zam-markdown/tests/chunk.rs`;
  - `tests/golden_chunks/` (новые фикстуры; перегенерация `ZAM_BLESS=1` — с ревью диффа);
  - `crates/zam-store/src/nodes/chunks.rs` (`context_prefix`);
  - `crates/zam-server/src/config.rs` и `deploy/config.example.toml` (`[index] chunk_boundary_lambda`, `chunk_cohesion`, `chunk_prefix_l0`).
- Create: `eval/grid-chunking.toml`

**Interfaces:**
- Consumes: набор и метрики задачи 16; исследование `docs/research/2026-10-21-chunking-semantic.md` §5 (шаги 2–3, критерий).
- Produces:
  ```rust
  pub struct ChunkParams { /* поля M2 */ pub boundary_lambda: f64 /*0 — поведение M2*/, pub cohesion: bool /*false*/ }
  pub fn boundary_strength(prev: &Unit, next: &Unit) -> i32;   // сила границы перед `next`
  // IndexSection: chunk_boundary_lambda: f64 (0.0), chunk_cohesion: bool (false), chunk_prefix_l0: bool (false)
  ```
- **Исправления** (принимаются по тестам, независимо от eval):
  - **Висячий заголовок H4–H6** (дефект по чтению кода, исследование §2): лист никогда не кончается заголовком H4–H6, заголовок уходит в следующий лист вместе со своим содержимым.
  - **Вводная фраза:** абзац на «:» не отрывается от следующего блока (код, список, таблица).
  - **Путь заголовков:** H4–H6 входят в `heading_path` листа (`H1 > H2 > H3 > H4`) и тем самым в `context_prefix`. Секции по-прежнему режутся по H1–H3.
  
  Меняются `embed_key` затронутых чанков — нужен `reindex --full` (в CHANGELOG).
- **Сила границы:**
  - перед H4–H6 и `---` — 4;
  - между абзацами и перед или после кода, таблицы, списка — 2;
  - между предложениями — 1;
  - после заголовка H4–H6 или после абзаца на «:» — −5 (разрез запрещён).
- **Выбор разреза** — динамическое программирование по кандидатам группы (по аналогии с Knuth–Plass): минимум Σ((len − goal)/goal)² − λ·strength при жёстких `hard_max` и `target_min` (кроме хвоста), O(n·k). При λ = 0 — жадная упаковка M2 (кроме двух исправлений выше).
- **`chunk_cohesion`** (за флагом, по умолчанию выключен): в длинном абзаце или серии абзацев без структуры к силе границы добавляется лексическая связность соседних окон — пересечение основ слов (`rust-stemmers`, MIT/BSD-3-Clause, сейчас только в xtask, становится зависимостью `zam-markdown`). Без инференса.
- **`chunk_prefix_l0`** (эксперимент): `context_prefix + " — " + L0` заметки (≤ 50 токенов); расход токенов индексации — в отчёт.
- **Не брать:** embedding-нарезку и late chunking (обоснование — исследование §4).
- **Сетка `eval/grid-chunking.toml`** (fp16 CUDA, проверка лучшей — int8 CPU):
  - база;
  - λ ∈ {0,5; 1; 2};
  - H4-путь вкл./выкл.;
  - cohesion вкл.;
  - prefix-L0 вкл.;
  - 150–300 поверх лучшей.
- **Критерий принятия** (парный бутстреп 95 % против базы):
  - нижняя граница Δ `chunk-recall@5` > 0 на подмножестве длинных заметок;
  - на всём наборе нижние границы Δ nDCG@10 и Δ recall@5 ≥ −0,01;
  - число чанков и токенов индексации ±5 % от базы (prefix-L0 — отдельной строкой).
  
  Не выполнено — `chunk_boundary_lambda = 0`, `cohesion` и `prefix_l0` выключены, в журнал — вывод «нарезка по структуре достаточна». Исправления H4–H6 и «:» остаются в любом случае. Шум свежего индекса — ±0,003 (журнал M2).

- [ ] **Step 1: Тесты `tests/chunk.rs`** (TDD):
  - `h4_heading_never_ends_leaf` (сценарий исследования: P1 = 310, H4, P2 = 300);
  - `h5_h6_never_end_leaf`;
  - `colon_intro_stays_with_block`;
  - `heading_path_includes_h4`;
  - `dp_matches_greedy_when_lambda_zero` (golden-фикстуры M2 без H4 и «:» не меняются);
  - `dp_prefers_strong_boundary_when_lambda_positive`;
  - proptest (500 случаев) при λ ∈ {0, 1, 2}: `src[range] == text` и ≤ `hard_max`;
  - фаззинг `chunk_note` без паник.
- [ ] **Step 2:** Реализация; `cargo test -p zam-markdown -p zam-store -p zam-server --no-fail-fast`, clippy, fmt.
- [ ] **Step 3:** Сетка на наборе задачи 16, выбор по критерию; умолчания `[index]` и вывод — в журнал, результаты — `eval/results/<дата>-grid-chunking.*`.
- [ ] **Step 4:** Коммит `feat(markdown): нарезка с учётом силы границ, H4–H6 в пути заголовков, без висячих заголовков`.

### Task 18: Финальные прогоны eval M3, сетка весов поиска и гейт int8

**Files:**
- Create: `eval/grid-m3.toml`
- Modify: `xtask/src/eval/gate.rs`, `eval/README.md`, умолчания `[search]` в `crates/zam-server/src/config.rs` и `deploy/config.example.toml`

**Interfaces:**
- Consumes: набор задачи 16, нарезка задачи 17 (финальные прогоны — только после неё), `w_folder` (задача 15), бусты (задачи 8 и 9), `paired_diff` (`xtask/src/eval/metrics.rs`).
- **Сетка `eval/grid-m3.toml`** (≤ 10 конфигураций, fp16 CUDA):
  - `w_folder ∈ {0, 0.3, 0.5, 1.0}`;
  - бусты вида: 1,0 против 1,15/1,10;
  - бусты `certainty`: 1,0 против 1,1/1,05 против 1,2/1,1.
  
  Выбор — парный бутстреп по nDCG@10 и `reliable_first` (цель ≥ 0,9). Итог — в умолчания `[search]` и в журнал.
- **Финальные прогоны:** fts, hybrid int8 CPU, hybrid fp16 CUDA, hybrid+rerank GPU → `eval/results/<дата>-*`.
- **Вердикт гейта** (решение владельца 2026-10-21): «пройден», если нижняя граница 95 % интервала парного бутстрепа (10 000 выборок, фиксированный seed) разности int8 − fp32 ≥ −0,01 и для nDCG@10, и для recall@5. Косинус и Спирмен — в разделе «справочно», на вердикт не влияют.
- **Ожидание:** в M2 нижняя граница recall@5 была −0,015, поэтому возможно «не пройден». Порог исполнитель не меняет; итог и интервалы идут в журнал как вопрос владельцу.

- [ ] **Step 1: Unit-тесты гейта** `verdict_uses_lower_bound_only`, `cosine_is_informational`.
- [ ] **Step 2:** Сетка весов, выбор, умолчания `[search]`.
- [ ] **Step 3:** Финальные прогоны и `cargo xtask eval --quality-gate` на наборе задачи 16 → `eval/results/<дата>-quality-gate.{json,md}`.
- [ ] **Step 4:** Коммит `feat(eval): финальные прогоны M3, веса поиска, гейт int8 по нижней границе`.

### Task 19: Исследование — первый старт Qdrant после массовой индексации

**Files:**
- Modify: `xtask/src/bench.rs` (`--scenario first-start`), `crates/zam-index/src/supervisor/{config.rs,mod.rs}`, `crates/zam-index/src/qdrant.rs` (`wal_config`), `crates/zam-server/src/config.rs` (`[qdrant]`)
- Test: `crates/zam-index/tests/supervisor.rs`

**Interfaces:**
- **Варианты:**
  - A — перед остановкой воркер перестаёт писать, супервизор ждёт `flush_interval_sec` + 1 с после последнего upsert (не дольше `qdrant.shutdown_flush_wait_ms`, 6000);
  - B — `wal_capacity_mb` коллекции 32 → 8 и 4 (`update_collection`);
  - C — `optimizers.flush_interval_sec` 5 → 1;
  - A+B.
- **Замер** (GPU и CPU, 10 тыс. заметок, HDD): массовая индексация → жёсткая остановка во время записи → время до готовности Qdrant при следующем старте. Также штатная остановка и скорость индексации. По 3 повтора: медиана и максимум, число операций WAL из журнала Qdrant.
- **Критерий:** первый старт ≤ 5 с; остановка дольше не более чем на 5 с; индексация медленнее не больше чем на 5 %. Из прошедших — лучший по первому старту; иначе — ближайший к цели с записью остатка.
- **Внедрение:** `qdrant.shutdown_flush_wait_ms`, `qdrant.wal_capacity_mb`, `qdrant.flush_interval_sec`, умолчания по итогу. Таблица — в журнал M3 и `eval/results/<дата>-bench-first-start.md`.

- [ ] **Step 1: Тесты** `config_renders_flush_interval_and_wal`, `shutdown_waits_for_flush_bounded`.
- [ ] **Step 2:** Замеры, выбор, внедрение, повторный замер. Коммит `perf(index): первый старт Qdrant после массовой индексации — <выбранный вариант>`.

### Task 20: e2e приёмки M3, документация, версия `0.0.4-m3`

**Files:**
- Create: `crates/zam-server/tests/m3_acceptance.rs` (процесс `zam-server`, `tests/common/process.rs`, MiniLM, Qdrant из `target/deps`)
- Modify:
  - `.github/workflows/ci.yml` (job на обеих ОС, если `check` дольше 15 мин);
  - `xtask/src/smoke.rs` (шаги M3);
  - `CHANGELOG.md`, `README.md`, `docs/journal.md`;
  - `deploy/README-install.md` (мастер-ключ и его резервирование, совместный доступ, человеческий токен), `deploy/config.example.toml`;
  - `docs/specs/2026-10-17-zeroaimemory-tz.md` (§17 по задаче 19, §18.2 по задачам 16–18, §19 — статус, §23);
  - `Cargo.toml` и `web/package.json` (`0.0.4-m3`);
  - `docs/plans/2026-10-21-m3-journals-graph-log.md`.

- [ ] **Step 1: «Изоляция»** (§18.1):
  - Human и Vault — нет в Qdrant, в выдаче по токену и в `logs/`; значения Vault нет в FTS;
  - B без доступа к решению A — 403 `solution-access` на дерево, узел, записи, граф, вложение; поиск исключает решение с предупреждением;
  - после одобрения `ro` — чтение пространства решения, запись 403, Knowledge, Rules, Human и Vault A — 404;
  - после отзыва тот же токен — 403 на следующем запросе.
- [ ] **Step 2: «Замещение без потери»:**
  - insight X (hypothesis, с триггером) → `supersede` новым Y (theorem с evidence);
  - поиск по умолчанию находит Y, не находит X; `include_history` находит обоих;
  - X со статусом `superseded`, его ревизии целы; ребро `supersedes` Y → X; триггеры X не выдаются;
  - агент не может отозвать `axiom`, а человеческий токен может.
- [ ] **Step 3: «Секрет через агента»:**
  - агент пишет секрет с `X-Zam-Session`/`X-Zam-Machine`, раскрывает его;
  - аудит `vault.reveal` с сессией и машиной без значения; в байтах базы и в журнале значения нет;
  - перезапуск с тем же ключом раскрывает, с чужим — 503 `vault-locked`.
- [ ] **Step 4: Smoke M3:**
  - регистрация решения;
  - insight → находится в top-3 с `kind=insight` и `certainty=hypothesis`;
  - `vault set`/`get` — то же значение;
  - диаграмма с `layout: elk` не нужна в smoke (проверяется e2e WebUI).
- [ ] **Step 5: Полный прогон:**
  - fmt, clippy `-D warnings`;
  - `cargo test --workspace --no-fail-fast` дважды;
  - тесты `#[ignore]` с моделями;
  - WebUI `check`, `build`, `size`, `e2e`;
  - `cargo xtask package` cpu и gpu + `smoke` → `SMOKE OK`;
  - CI зелёный на обеих ОС.
- [ ] **Step 6: Документы:**
  - CHANGELOG `## [0.0.4-m3]`;
  - `docs/journal.md` — итог M3 и критерии §19 со ссылками на тесты и eval;
  - журнал решений M3 (решения, rulings, отложенное, «Уточнить у владельца»).
  
  Коммиты: `test: e2e приёмки M3 — изоляция, замещение без потери, секрет через агента` и `docs: M3 — CHANGELOG, журнал, ТЗ, версия 0.0.4-m3`.
