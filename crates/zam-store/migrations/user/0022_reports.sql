-- user DB, задача 1 M5.5 (спецификация M5.5 §4, §5, §11): вид записи `report` (отчёт),
-- файлы отчёта `entry_attachments` и неизменность отчёта на уровне схемы. Миграция с признаком
-- `rebuild` (zam_store::migrate): загрузчик выключает `foreign_keys` вне транзакции, после SQL
-- проверяет `foreign_key_check`.
-- Схема снята с базы на 0021 (`sqlite_master`): `entries` не менялась с 0015 — индексы
-- `entries_feed`, `entries_title`, `entries_pending` и автоиндексы PRIMARY KEY и UNIQUE
-- (`idempotency_key`), триггеров и представлений на `entries` нет; дочерние FK —
-- `entry_revisions.entry_id`, `triggers.entry_id` и самоссылки `supersedes_id`/`superseded_by_id`
-- (`chunks.entry_id`, `file_refs`, `edges`, `usage`, `cache_deps` — без FK). Ссылки на `entries`
-- в других таблицах — по имени: после `RENAME` новая таблица их получает.

-- ---------- entries ----------
CREATE TABLE entries_new (
    id               TEXT PRIMARY KEY,
    space_id         TEXT NOT NULL REFERENCES spaces (id),
    kind             TEXT NOT NULL
                     CHECK (kind IN ('changelog', 'release', 'insight', 'term', 'task', 'question',
                                     'report')),
    title            TEXT NOT NULL,
    title_norm       TEXT NOT NULL,       -- zam_store::normalize_title (дедупликация по заголовку)
    body             TEXT NOT NULL DEFAULT '',
    data             TEXT NOT NULL DEFAULT '{}',  -- JSON-объект по виду записи
    tags             TEXT NOT NULL DEFAULT '[]',  -- JSON-массив строк
    area             TEXT,
    status           TEXT,                -- допустимое множество — по виду (CHECK ниже)
    certainty        TEXT CHECK (certainty IN ('axiom', 'theorem', 'hypothesis')),
    supersedes_id    TEXT REFERENCES entries (id),
    superseded_by_id TEXT REFERENCES entries (id),
    retract_reason   TEXT,
    idempotency_key  TEXT UNIQUE,
    current_rev      INTEGER NOT NULL DEFAULT 1,
    author_kind      TEXT NOT NULL CHECK (author_kind IN ('human', 'agent', 'system')),
    user_id          TEXT,
    token_id         TEXT,
    session_id       TEXT,
    machine          TEXT,
    index_state      TEXT NOT NULL DEFAULT 'pending'
                     CHECK (index_state IN ('pending', 'indexed', 'failed', 'excluded')),
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    deleted_at       TEXT,
    -- статуса нет только у changelog, release и report, у остальных видов он обязателен
    CHECK ((status IS NULL) = (kind IN ('changelog', 'release', 'report'))),
    -- статус из множества своего вида (NULL отсечён проверкой выше: `IN` с NULL дал бы NULL,
    -- а CHECK с NULL проходит)
    CHECK (status IS NULL
        OR (kind = 'insight'  AND status IN ('active', 'revisit', 'superseded', 'retracted'))
        OR (kind = 'term'     AND status IN ('active', 'superseded', 'retracted'))
        OR (kind = 'task'     AND status IN ('open', 'in_progress', 'blocked', 'done', 'dropped'))
        OR (kind = 'question' AND status IN ('open', 'answered', 'obsolete'))),
    -- отчёт не имеет эпистемического статуса, не замещается и не отзывается
    CHECK (kind <> 'report' OR (certainty IS NULL AND supersedes_id IS NULL
                                AND superseded_by_id IS NULL AND retract_reason IS NULL)),
    -- отчёт не векторизуется: только FTS (спецификация M5.5 §7)
    CHECK (kind <> 'report' OR index_state = 'excluded')
);
INSERT INTO entries_new (
    id, space_id, kind, title, title_norm, body, data, tags, area, status, certainty,
    supersedes_id, superseded_by_id, retract_reason, idempotency_key, current_rev, author_kind,
    user_id, token_id, session_id, machine, index_state, created_at, updated_at, deleted_at)
SELECT
    id, space_id, kind, title, title_norm, body, data, tags, area, status, certainty,
    supersedes_id, superseded_by_id, retract_reason, idempotency_key, current_rev, author_kind,
    user_id, token_id, session_id, machine, index_state, created_at, updated_at, deleted_at
FROM entries;
DROP TABLE entries;
ALTER TABLE entries_new RENAME TO entries;
CREATE INDEX entries_feed ON entries (space_id, kind, created_at) WHERE deleted_at IS NULL;
CREATE INDEX entries_title ON entries (space_id, kind, title_norm) WHERE deleted_at IS NULL;
-- `pending_count` поиска: только записи в очереди векторной индексации.
CREATE INDEX entries_pending ON entries (space_id)
    WHERE index_state = 'pending' AND deleted_at IS NULL;

-- ---------- entry_attachments ----------
-- Файлы отчёта (картинки и прочие): пара (запись, sha256, имя), как `attachments` у заметок.
-- Каждая пара — единица `blobs.refcount`. Строки только дописываются: открепление и очистка
-- блобов — вместе со сборщиком мусора блобов (M7), тогда же снимаются триггеры ниже.
CREATE TABLE entry_attachments (
    entry_id    TEXT NOT NULL REFERENCES entries (id),
    blob_sha256 TEXT NOT NULL REFERENCES blobs (sha256),
    filename    TEXT NOT NULL,
    mime        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (entry_id, blob_sha256, filename)
);
-- сборщик мусора блобов (M7): кто ещё ссылается на блоб
CREATE INDEX entry_attachments_blob ON entry_attachments (blob_sha256);

-- ---------- неизменность отчёта ----------
-- Содержимое, атрибуция и ревизия отчёта не меняются; разрешены только корзина
-- (`deleted_at`), `updated_at` и состояние индекса (`index_state`, CHECK держит `excluded`).
-- Другая запись правкой отчётом не становится.
CREATE TRIGGER entries_report_immutable BEFORE UPDATE ON entries
WHEN (NEW.kind = 'report' AND OLD.kind <> 'report') OR (OLD.kind = 'report' AND (
       NEW.id IS NOT OLD.id OR NEW.space_id IS NOT OLD.space_id OR NEW.kind IS NOT OLD.kind
    OR NEW.title IS NOT OLD.title OR NEW.title_norm IS NOT OLD.title_norm
    OR NEW.body IS NOT OLD.body OR NEW.data IS NOT OLD.data OR NEW.tags IS NOT OLD.tags
    OR NEW.area IS NOT OLD.area OR NEW.status IS NOT OLD.status
    OR NEW.certainty IS NOT OLD.certainty OR NEW.supersedes_id IS NOT OLD.supersedes_id
    OR NEW.superseded_by_id IS NOT OLD.superseded_by_id
    OR NEW.retract_reason IS NOT OLD.retract_reason
    OR NEW.idempotency_key IS NOT OLD.idempotency_key OR NEW.current_rev IS NOT OLD.current_rev
    OR NEW.author_kind IS NOT OLD.author_kind OR NEW.user_id IS NOT OLD.user_id
    OR NEW.token_id IS NOT OLD.token_id OR NEW.session_id IS NOT OLD.session_id
    OR NEW.machine IS NOT OLD.machine OR NEW.created_at IS NOT OLD.created_at))
BEGIN
    SELECT RAISE(ABORT, 'report-immutable');
END;

-- у отчёта только первая ревизия
CREATE TRIGGER entry_revisions_report_once BEFORE INSERT ON entry_revisions
WHEN NEW.rev > 1 AND EXISTS (SELECT 1 FROM entries WHERE id = NEW.entry_id AND kind = 'report')
BEGIN
    SELECT RAISE(ABORT, 'report-immutable');
END;

-- файлы пишутся только в транзакции создания отчёта: `created_at` пары равен `created_at`
-- отчёта (`create_report` пишет одно и то же время); к другим записям и позже — отказ
CREATE TRIGGER entry_attachments_report_only BEFORE INSERT ON entry_attachments
WHEN NOT EXISTS (SELECT 1 FROM entries
                 WHERE id = NEW.entry_id AND kind = 'report' AND created_at = NEW.created_at)
BEGIN
    SELECT RAISE(ABORT, 'report-immutable: files are written only with the report');
END;

CREATE TRIGGER entry_attachments_no_update BEFORE UPDATE ON entry_attachments BEGIN
    SELECT RAISE(ABORT, 'report-immutable: entry_attachments are append-only');
END;

CREATE TRIGGER entry_attachments_no_delete BEFORE DELETE ON entry_attachments BEGIN
    SELECT RAISE(ABORT, 'report-immutable: entry_attachments are append-only');
END;
