-- user DB, §6.3, §8.1, §8.2 ТЗ (этап M2): кэш эмбеддингов, outbox индексации,
-- уровни чанков в FTS. Время — TEXT, формат zam_store::time.

-- Кэш эмбеддингов по тексту чанка: Qdrant пересобирается отсюда без инференса.
-- dense — f16 LE подряд; sparse — повторяющиеся пары (u32 LE id, f16 LE вес).
CREATE TABLE embeddings (
    text_hash  TEXT NOT NULL,          -- blake3 hex текста чанка (chunks.text_hash)
    model_id   TEXT NOT NULL,
    dense      BLOB NOT NULL,
    sparse     BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (text_hash, model_id)
) WITHOUT ROWID;

-- Outbox: задания ставятся в той же транзакции, что и запись узла (§8.1).
CREATE TABLE index_jobs (
    id          INTEGER PRIMARY KEY,
    target_kind TEXT NOT NULL DEFAULT 'node',  -- M2: только 'node'
    target_id   TEXT NOT NULL,
    rev         INTEGER NOT NULL,
    op          TEXT NOT NULL CHECK (op IN ('upsert', 'delete')),
    state       TEXT NOT NULL DEFAULT 'queued'
                CHECK (state IN ('queued', 'running', 'done', 'failed')),
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    priority    INTEGER NOT NULL DEFAULT 0,
    not_before  TEXT NOT NULL,
    enqueued_at TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX index_jobs_queue ON index_jobs (state, priority, not_before);
-- Одно активное задание на узел: новая запись заменяет активное, а не добавляет второе.
CREATE UNIQUE INDEX index_jobs_active ON index_jobs (target_kind, target_id)
    WHERE state IN ('queued', 'running');

-- Служебные флаги базы.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
-- Ruling R1: чанки M1 (по секциям, level = 'section') и без векторов — при первом
-- открытии v4 все живые заметки перечанковываются чанкером §8.2 и получают `upsert`.
INSERT INTO meta (key, value) VALUES ('rechunk_v4', 'pending');

-- В FTS идут только уровни `chunk` и `note`; `section` (L1 секции) — только для векторов.
-- Чанки M1 (все `section`, все в FTS) становятся `chunk`, чтобы удаление при
-- перечанковке убрало их из FTS. UPDATE OF level старые триггеры не задевает.
UPDATE chunks SET level = 'chunk' WHERE level = 'section';

DROP TRIGGER chunks_ai;
DROP TRIGGER chunks_ad;
DROP TRIGGER chunks_au;

-- Внешний контент FTS5 совпадает с подмножеством строк chunks, поэтому
-- `INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')` применять нельзя:
-- он проиндексировал бы и `section`. Условия — внутри тел триггеров (INSERT … SELECT
-- … WHERE), чтобы удаление старой и вставка новой версии шли строго по порядку.
CREATE TRIGGER chunks_ai AFTER INSERT ON chunks WHEN new.level <> 'section' BEGIN
    INSERT INTO chunks_fts (rowid, text, heading_path, context_prefix)
        VALUES (new.seq, new.text, new.heading_path, new.context_prefix);
    INSERT INTO chunks_fts_tri (rowid, text) VALUES (new.seq, new.text);
END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks WHEN old.level <> 'section' BEGIN
    INSERT INTO chunks_fts (chunks_fts, rowid, text, heading_path, context_prefix)
        VALUES ('delete', old.seq, old.text, old.heading_path, old.context_prefix);
    INSERT INTO chunks_fts_tri (chunks_fts_tri, rowid, text) VALUES ('delete', old.seq, old.text);
END;
CREATE TRIGGER chunks_au AFTER UPDATE OF text, heading_path, context_prefix, level ON chunks BEGIN
    INSERT INTO chunks_fts (chunks_fts, rowid, text, heading_path, context_prefix)
        SELECT 'delete', old.seq, old.text, old.heading_path, old.context_prefix
        WHERE old.level <> 'section';
    INSERT INTO chunks_fts_tri (chunks_fts_tri, rowid, text)
        SELECT 'delete', old.seq, old.text WHERE old.level <> 'section';
    INSERT INTO chunks_fts (rowid, text, heading_path, context_prefix)
        SELECT new.seq, new.text, new.heading_path, new.context_prefix
        WHERE new.level <> 'section';
    INSERT INTO chunks_fts_tri (rowid, text)
        SELECT new.seq, new.text WHERE new.level <> 'section';
END;
