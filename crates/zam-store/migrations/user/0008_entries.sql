-- user DB, §6.3, §7.2–7.5 ТЗ (этап M3): записи журналов решения (Changelog, Releases,
-- Insights), их ревизии и триггеры вспоминания. Время — TEXT, формат zam_store::time.
-- Столбцы `certainty` и `reason` закладываются сразу; правила `certainty` — задача 9.

CREATE TABLE entries (
    id               TEXT PRIMARY KEY,
    space_id         TEXT NOT NULL REFERENCES spaces (id),
    kind             TEXT NOT NULL CHECK (kind IN ('changelog', 'release', 'insight')),
    title            TEXT NOT NULL,
    title_norm       TEXT NOT NULL,       -- zam_store::normalize_title (дедупликация по заголовку)
    body             TEXT NOT NULL DEFAULT '',
    data             TEXT NOT NULL DEFAULT '{}',  -- JSON-объект по виду записи
    tags             TEXT NOT NULL DEFAULT '[]',  -- JSON-массив строк
    area             TEXT,
    status           TEXT CHECK (status IN ('active', 'revisit', 'superseded', 'retracted')),
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
    -- статус есть только у insight, и у insight он обязателен
    CHECK ((kind = 'insight') = (status IS NOT NULL))
);
CREATE INDEX entries_feed ON entries (space_id, kind, created_at) WHERE deleted_at IS NULL;
CREATE INDEX entries_title ON entries (space_id, kind, title_norm) WHERE deleted_at IS NULL;
-- `pending_count` поиска: только записи в очереди векторной индексации.
CREATE INDEX entries_pending ON entries (space_id)
    WHERE index_state = 'pending' AND deleted_at IS NULL;

-- Ревизии записи — полный снимок полей на момент правки; только дописываются.
CREATE TABLE entry_revisions (
    entry_id    TEXT NOT NULL REFERENCES entries (id),
    rev         INTEGER NOT NULL,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    data        TEXT NOT NULL,
    tags        TEXT NOT NULL,
    area        TEXT,
    status      TEXT,
    certainty   TEXT,
    reason      TEXT,                     -- причина смены статуса или certainty
    ts          TEXT NOT NULL,
    author_kind TEXT NOT NULL,
    user_id     TEXT,
    token_id    TEXT,
    session_id  TEXT,
    machine     TEXT,
    PRIMARY KEY (entry_id, rev)
);

CREATE TRIGGER entry_revisions_no_update BEFORE UPDATE ON entry_revisions BEGIN
    SELECT RAISE(ABORT, 'entry_revisions are append-only');
END;

CREATE TRIGGER entry_revisions_no_delete BEFORE DELETE ON entry_revisions BEGIN
    SELECT RAISE(ABORT, 'entry_revisions are append-only');
END;

-- Триггеры проактивного вспоминания (§10.3) записи insight.
CREATE TABLE triggers (
    id         TEXT PRIMARY KEY,
    entry_id   TEXT NOT NULL REFERENCES entries (id),
    type       TEXT NOT NULL CHECK (type IN ('file_glob', 'command_regex', 'error_regex', 'keyword')),
    pattern    TEXT NOT NULL,
    tools      TEXT,                      -- JSON-массив инструментов или NULL
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);
CREATE INDEX triggers_entry ON triggers (entry_id);

-- Чанки записи (node_id IS NULL): пересборка и удаление по записи.
CREATE INDEX chunks_entry ON chunks (entry_id) WHERE entry_id IS NOT NULL;
