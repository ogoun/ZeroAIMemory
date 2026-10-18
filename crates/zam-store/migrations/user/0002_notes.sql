-- user DB, §6.3 ТЗ (этап M1: заметки). Время — TEXT, формат zam_store::time.

-- Дерево узлов. Путь уникален только среди живых узлов: удалённый узел
-- (deleted_at) не блокирует путь (частичный индекс вместо UNIQUE(space_id, path)).
CREATE TABLE nodes (
    id           TEXT PRIMARY KEY,
    space_id     TEXT NOT NULL REFERENCES spaces (id),
    parent_id    TEXT REFERENCES nodes (id),
    kind         TEXT NOT NULL CHECK (kind IN ('folder', 'note')),
    name         TEXT NOT NULL,
    path         TEXT NOT NULL,
    sort_key     TEXT,
    area         TEXT,
    current_rev  INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT,             -- sha256 hex текущей ревизии
    frontmatter  TEXT,             -- JSON: tags, description, l0, index
    l0           TEXT,
    l1           TEXT,
    l0_source    TEXT CHECK (l0_source IN ('auto', 'manual', 'agent')),
    index_state  TEXT NOT NULL DEFAULT 'pending'
                 CHECK (index_state IN ('pending', 'indexed', 'failed', 'excluded')),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    deleted_at   TEXT
);
CREATE UNIQUE INDEX nodes_live_path ON nodes (space_id, path) WHERE deleted_at IS NULL;
CREATE INDEX nodes_parent ON nodes (space_id, parent_id);
-- Частичный: только для корзины; выборки живых узлов идут по nodes_parent/nodes_live_path.
CREATE INDEX nodes_deleted ON nodes (deleted_at) WHERE deleted_at IS NOT NULL;

-- Ревизии только дописываются.
CREATE TABLE revisions (
    node_id      TEXT NOT NULL REFERENCES nodes (id),
    rev          INTEGER NOT NULL,
    content      TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    size         INTEGER NOT NULL,
    author_kind  TEXT NOT NULL CHECK (author_kind IN ('human', 'agent', 'system')),
    token_id     TEXT,
    session_id   TEXT,
    machine      TEXT,
    message      TEXT,
    ts           TEXT NOT NULL,
    PRIMARY KEY (node_id, rev)
);

-- Чанки. `seq` — явный INTEGER PRIMARY KEY (псевдоним rowid): VACUUM не
-- перенумерует его, поэтому внешний контент FTS5 остаётся согласованным.
CREATE TABLE chunks (
    seq            INTEGER PRIMARY KEY,
    id             TEXT NOT NULL UNIQUE,  -- blake3(node ‖ ordinal ‖ text_hash), 16 байт hex
    node_id        TEXT REFERENCES nodes (id),
    entry_id       TEXT,
    rev            INTEGER NOT NULL,
    level          TEXT NOT NULL
                   CHECK (level IN ('chunk', 'section', 'note', 'folder', 'attachment')),
    heading_path   TEXT NOT NULL DEFAULT '',
    ordinal        INTEGER NOT NULL,
    text           TEXT NOT NULL,
    context_prefix TEXT NOT NULL,
    token_count    INTEGER NOT NULL,
    text_hash      TEXT NOT NULL,         -- blake3 hex текста
    blob_sha256    TEXT,
    locator        TEXT
);
CREATE INDEX chunks_node ON chunks (node_id);

-- context_prefix (пространство, путь, имя, заголовки, теги) участвует в FTS (§8.2 п.6).
CREATE VIRTUAL TABLE chunks_fts USING fts5 (
    text, heading_path, context_prefix,
    content = 'chunks', content_rowid = 'seq',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE chunks_fts_tri USING fts5 (
    text,
    content = 'chunks', content_rowid = 'seq',
    tokenize = 'trigram'
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts (rowid, text, heading_path, context_prefix)
        VALUES (new.seq, new.text, new.heading_path, new.context_prefix);
    INSERT INTO chunks_fts_tri (rowid, text) VALUES (new.seq, new.text);
END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts (chunks_fts, rowid, text, heading_path, context_prefix)
        VALUES ('delete', old.seq, old.text, old.heading_path, old.context_prefix);
    INSERT INTO chunks_fts_tri (chunks_fts_tri, rowid, text) VALUES ('delete', old.seq, old.text);
END;
-- Чанки пересоздаются удалением и вставкой; UPDATE (например, context_prefix
-- после переноса узла) держит FTS согласованным.
CREATE TRIGGER chunks_au AFTER UPDATE OF text, heading_path, context_prefix ON chunks BEGIN
    INSERT INTO chunks_fts (chunks_fts, rowid, text, heading_path, context_prefix)
        VALUES ('delete', old.seq, old.text, old.heading_path, old.context_prefix);
    INSERT INTO chunks_fts_tri (chunks_fts_tri, rowid, text) VALUES ('delete', old.seq, old.text);
    INSERT INTO chunks_fts (rowid, text, heading_path, context_prefix)
        VALUES (new.seq, new.text, new.heading_path, new.context_prefix);
    INSERT INTO chunks_fts_tri (rowid, text) VALUES (new.seq, new.text);
END;

CREATE TABLE blobs (
    sha256     TEXT PRIMARY KEY,
    size       INTEGER NOT NULL,
    mime       TEXT,
    refcount   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE attachments (
    node_id     TEXT NOT NULL REFERENCES nodes (id),
    blob_sha256 TEXT NOT NULL REFERENCES blobs (sha256),
    filename    TEXT NOT NULL,
    mime        TEXT,
    size        INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    text_state  TEXT NOT NULL DEFAULT 'none'
                CHECK (text_state IN ('none', 'pending', 'done', 'failed')),
    PRIMARY KEY (node_id, blob_sha256, filename)
);
