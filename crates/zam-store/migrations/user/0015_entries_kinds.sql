-- user DB, задача 1 M4.5 (спецификация M4.5 §1.2, §1.3, §5.2): виды записей `term`, `task`,
-- `question` и тип связи `contradicts`. Миграция с признаком `rebuild` (zam_store::migrate):
-- загрузчик выключает `foreign_keys` вне транзакции, после SQL проверяет `foreign_key_check`.
-- Схема снята с базы на 0014 (`sqlite_master`): у `entries` — индексы `entries_feed`,
-- `entries_title`, `entries_pending` и автоиндексы PRIMARY KEY и UNIQUE (`idempotency_key`),
-- триггеров и представлений на `entries` нет; дочерние FK — `entry_revisions.entry_id`,
-- `triggers.entry_id` и самоссылки `supersedes_id`/`superseded_by_id` (`chunks.entry_id` —
-- без FK). У `edges` — индексы `edges_src`, `edges_dst`, `edges_unresolved`, `edges_explicit`,
-- `edges_derived_raw` и автоиндекс PRIMARY KEY; FK на `edges` нет. Ссылки на `entries` в
-- других таблицах — по имени: после `RENAME` новая таблица их получает.

-- ---------- entries ----------
CREATE TABLE entries_new (
    id               TEXT PRIMARY KEY,
    space_id         TEXT NOT NULL REFERENCES spaces (id),
    kind             TEXT NOT NULL
                     CHECK (kind IN ('changelog', 'release', 'insight', 'term', 'task', 'question')),
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
    -- статуса нет только у changelog и release, у остальных видов он обязателен
    CHECK ((status IS NULL) = (kind IN ('changelog', 'release'))),
    -- статус из множества своего вида (NULL отсечён проверкой выше: `IN` с NULL дал бы NULL,
    -- а CHECK с NULL проходит)
    CHECK (status IS NULL
        OR (kind = 'insight'  AND status IN ('active', 'revisit', 'superseded', 'retracted'))
        OR (kind = 'term'     AND status IN ('active', 'superseded', 'retracted'))
        OR (kind = 'task'     AND status IN ('open', 'in_progress', 'blocked', 'done', 'dropped'))
        OR (kind = 'question' AND status IN ('open', 'answered', 'obsolete')))
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

-- ---------- edges ----------
CREATE TABLE edges_new (
    id TEXT PRIMARY KEY, space_id TEXT NOT NULL,     -- пространство источника
    src_kind TEXT NOT NULL CHECK (src_kind IN ('node', 'entry')), src_id TEXT NOT NULL,
    dst_kind TEXT CHECK (dst_kind IN ('node', 'entry')), dst_id TEXT, dst_space_id TEXT, dst_raw TEXT, dst_anchor TEXT,
    type TEXT NOT NULL CHECK (type IN ('links_to','relates_to','depends_on','part_of','caused_by','fixes','supersedes','derived_from','mentions','contradicts')),
    origin TEXT NOT NULL CHECK (origin IN ('wikilink', 'frontmatter', 'explicit', 'system')),
    user_id TEXT, token_id TEXT, created_at TEXT NOT NULL, deleted_at TEXT,
    CHECK (dst_id IS NOT NULL OR dst_raw IS NOT NULL)
);
INSERT INTO edges_new (
    id, space_id, src_kind, src_id, dst_kind, dst_id, dst_space_id, dst_raw, dst_anchor, type,
    origin, user_id, token_id, created_at, deleted_at)
SELECT
    id, space_id, src_kind, src_id, dst_kind, dst_id, dst_space_id, dst_raw, dst_anchor, type,
    origin, user_id, token_id, created_at, deleted_at
FROM edges;
DROP TABLE edges;
ALTER TABLE edges_new RENAME TO edges;
CREATE INDEX edges_src ON edges (src_kind, src_id) WHERE deleted_at IS NULL;
CREATE INDEX edges_dst ON edges (dst_kind, dst_id) WHERE deleted_at IS NULL;
CREATE INDEX edges_unresolved ON edges (space_id, dst_raw) WHERE dst_id IS NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX edges_explicit ON edges (src_kind, src_id, dst_kind, dst_id, type)
    WHERE origin IN ('explicit', 'system') AND deleted_at IS NULL;
CREATE INDEX edges_derived_raw ON edges (space_id, dst_raw)
    WHERE deleted_at IS NULL AND origin IN ('wikilink', 'frontmatter');
