-- user DB, задача 2 M6 (спецификация M6 §7.2, §8а, §12а, §13): вся схема этапа одной миграцией
-- (один `bak-v24`, один шаг отката). Заморожена после M6: новые изменения схемы — новой
-- миграцией. Части, дописанные после первого применения (`imported_from`, `node_idempotency`),
-- база версии 25 получает досборкой при открытии (`zam_store::schema_repair`). Миграция с признаком `rebuild` (zam_store::migrate):
-- загрузчик выключает `foreign_keys` вне транзакции, после SQL проверяет `foreign_key_check`.
--
-- Схема снята с базы на 0024 (`sqlite_master`): у `session_seen` (0013) нет своих индексов,
-- кроме автоиндекса первичного ключа, нет триггеров и представлений; триггеров и представлений,
-- ссылающихся на неё в теле, тоже нет. FK — только свой `session_id → sessions(id)`, дочерних
-- FK на `session_seen` нет. Поэтому пересборка — новая таблица, перенос строк, `DROP`, `RENAME`
-- без снятия и пересоздания чужих объектов (в отличие от 0024: триггеры 0022 ссылаются на
-- `entries`). Остальное — `ALTER TABLE ADD COLUMN` и новые таблицы.

-- ---------- session_seen: эпоха в первичном ключе (§7.2) ----------
-- Отметки прежних эпох не считаются выданными (`seen_revs` читает только текущую эпоху сессии
-- `sessions.seen_epoch`) и хранятся до срока сессии; показ идей читается без эпохи
-- (`seen_entry_ids`). Прежние строки — эпоха 0.
CREATE TABLE session_seen_new (
  session_id TEXT NOT NULL REFERENCES sessions(id),
  owner_id TEXT NOT NULL,             -- пользователь-владелец базы цели (решение может быть чужим)
  target_kind TEXT NOT NULL CHECK (target_kind IN ('node','entry','blob')),
  target_id TEXT NOT NULL, part TEXT NOT NULL DEFAULT '', rev INTEGER NOT NULL,
  seen_at TEXT NOT NULL,
  epoch INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, owner_id, target_kind, target_id, part, epoch));
INSERT INTO session_seen_new (session_id, owner_id, target_kind, target_id, part, rev, seen_at,
                              epoch)
SELECT session_id, owner_id, target_kind, target_id, part, rev, seen_at, 0 FROM session_seen;
DROP TABLE session_seen;
ALTER TABLE session_seen_new RENAME TO session_seen;

-- ---------- sessions: эпоха и holdout (§7.2, §12) ----------
-- `seen_epoch` растёт на `session_start` с `source` compact/resume/clear, `recovery`,
-- `PreCompact` и явном `new_epoch`; `holdout = 1` — сессия контрольной группы агентного A/B
-- (`/recall` отдаёт пустой текст, учёт ведётся).
ALTER TABLE sessions ADD COLUMN seen_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN holdout INTEGER NOT NULL DEFAULT 0;

-- ---------- session_tokens: повторная выдача (§7.3) ----------
-- `/read` с пометкой «уже выдавалось в этой сессии» прибавляет 1 к строке события `read`.
ALTER TABLE session_tokens ADD COLUMN reread_calls INTEGER NOT NULL DEFAULT 0;

-- ---------- учёт выданного без сессии (§7.1) ----------
-- MCP без явной сессии и прочие выдачи без сессии: по суткам UTC (`yyyy-MM-dd`) и событию;
-- запись — UPSERT с прибавлением. Срока хранения нет (строк — сутки × события).
CREATE TABLE tokens_unattributed (
  day TEXT NOT NULL,
  event TEXT NOT NULL,
  calls INTEGER NOT NULL,
  tokens INTEGER NOT NULL,
  PRIMARY KEY (day, event)) WITHOUT ROWID;

-- ---------- снимок сессии (§8а) ----------
-- Один снимок на сессию (повторная запись заменяет прежний); `data` — JSON нейтрального снимка
-- `zam_core::snapshot::SnapshotData` после маскировки. Каскада нет: таблица — в явном списке
-- `gc_sessions`, срок `snapshot.keep_days` соблюдает воркер обслуживания (`gc_snapshots`).
-- `owner_id` не нужен: сессия принадлежит пользователю базы. Повод — `trigger_kind`, а не
-- `trigger`: TRIGGER — ключевое слово SQL (как `git_commit` в 0023).
CREATE TABLE session_snapshots (
  session_id TEXT PRIMARY KEY REFERENCES sessions(id),
  taken_at TEXT NOT NULL,
  trigger_kind TEXT NOT NULL,         -- zam_core::snapshot::SnapTrigger (CHECK нет: новые поводы)
  agent TEXT,
  status TEXT NOT NULL CHECK (status IN ('complete','partial')),
  skipped INTEGER NOT NULL,
  bytes INTEGER NOT NULL,
  data TEXT NOT NULL);

-- ---------- code_refs: вызовы по получателю (§12а) ----------
-- Сколько из `count` — вызовы по получателю (`x.f()`); данные старого клиента — 0 (все прямые).
-- Первичный ключ `(file_id, name, kind)` не меняется.
ALTER TABLE code_refs ADD COLUMN recv_count INTEGER NOT NULL DEFAULT 0;

-- ---------- происхождение импорта (§12б.5, задача 18b2) ----------
-- `{archive_fingerprint, source_id, source_rev, content_hash}` записи или заметки, пришедшей из
-- архива `POST /spaces/{space}/import`: повтор того же архива сравнивается по `source_id` +
-- `content_hash`. Отдельная колонка, а не поле `data`: `data` задач, вопросов, идей и терминов
-- нормализуется типизированным разбором при каждой записи сервера и потеряла бы поле.
ALTER TABLE nodes ADD COLUMN imported_from TEXT;
ALTER TABLE entries ADD COLUMN imported_from TEXT;

-- ---------- ключи идемпотентности заметок (§16 п. 19, CR-20, задача 19) ----------
-- `Idempotency-Key` у `POST /nodes`, `PUT` и `PATCH /nodes/{id}`: ключ в границах автора
-- (`<пользователь|t<токен>|->:<ключ>`, как `entries.idempotency_key`), операция, узел и отпечаток
-- запроса (blake3 hex, `zam_store::nodes::idempotency`). Строка пишется в транзакции самой
-- правки; живёт, пока жив узел (узлы физически не удаляются) — отдельного срока нет, как у
-- ключа записи журнала, и чистки нет (ревью задачи 19, M-6). Внешний ключ `node_id → nodes(id)`:
-- будущая пересборка `nodes` (новая таблица, `DROP`, `RENAME`) должна учесть эту дочернюю
-- таблицу. Текст `CREATE TABLE` ниже дословно повторяет `zam_store::schema_repair::
-- NODE_IDEMPOTENCY_DDL` (досборка баз версии 25, тест `node_idempotency_ddl_matches_0025`).
--
-- 0025 заморожена после M6: новые изменения схемы — новой миграцией.
CREATE TABLE node_idempotency (
  key TEXT PRIMARY KEY,
  op TEXT NOT NULL CHECK (op IN ('create','replace','patch')),
  node_id TEXT NOT NULL REFERENCES nodes(id),
  fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL) WITHOUT ROWID;