-- user DB, §6.3 и §8.5 ТЗ (этап M3): граф связей. Рёбра заметок и записей журналов внутри одной
-- базы; `space_id` — пространство источника, `dst_space_id` — цели (NULL у неразрешённой ссылки).
-- Выведенные рёбра (`wikilink`, `frontmatter`) пересоздаются при каждой записи узла, `dst_raw` у
-- них — ключ ссылки (`[[Имя]]` или путь в пространстве, zam_store::links); `explicit` и
-- `system` удаляются мягко (`deleted_at`).
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
-- Поддержка разрешения ссылок [решение исполнителя]: перепроверка выведенных рёбер по ключу при
-- создании, переносе, удалении и восстановлении узла и поиск заметки по имени (`[[Имя]]`) —
-- без просмотра всех рёбер и всех заметок пространства. Имя — первым столбцом: иначе индекс
-- перехватывает выборки живых заметок пространства (например, `pending_count`).
CREATE INDEX edges_derived_raw ON edges (space_id, dst_raw)
    WHERE deleted_at IS NULL AND origin IN ('wikilink', 'frontmatter');
CREATE INDEX nodes_live_name ON nodes (name, space_id, path)
    WHERE deleted_at IS NULL AND kind = 'note';
-- Рёбра существующих заметок (разбор Markdown SQL не посчитает) и системные `supersedes`
-- замещённых записей — в Rust при открытии базы, фоновой задачей (zam_store::graph).
INSERT INTO meta (key, value) VALUES ('edges_backfill', 'pending');
