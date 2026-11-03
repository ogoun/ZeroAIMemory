-- user DB, задача 3 M5.6 (спецификация M5.6 §6, §8): карта кода решения и ссылки знаний на
-- символы. Без пересборки таблиц (`rebuild: false`): `ALTER TABLE ADD COLUMN`, пересоздание
-- индекса и новые таблицы. Карта — производные данные (восстанавливается полной синхронизацией
-- `zam code sync`), в поиск памяти, чанки и очередь индексации не попадает.

-- ---------- file_refs: ссылка на символ ----------
-- `symbol` — квалифицированное имя (`Store::open`, `foo#2`), NULL — ссылка на файл целиком;
-- `symbol_hash` — `body_hash` символа (16 hex) при записи, NULL — ориентиром станет первое
-- сообщение с символами. `stale_reason` += `symbol_changed`, `symbol_removed` (CHECK нет).
ALTER TABLE file_refs ADD COLUMN symbol TEXT;
ALTER TABLE file_refs ADD COLUMN symbol_hash TEXT;
-- один владелец — одна ссылка на `(путь, символ)`: файл и несколько его символов — разные строки;
-- выражение `COALESCE` принимает и цель `ON CONFLICT` upsert (SQLite ≥ 3.24)
DROP INDEX file_refs_owner;
CREATE UNIQUE INDEX file_refs_owner ON file_refs(owner_kind, owner_id, path, COALESCE(symbol, ''));

-- ---------- снимок карты решения ----------
-- Одна карта на решение, «последний снимок побеждает» пофайлово (§5). `git_commit`, `branch`,
-- `machine` — преобладающие среди файлов (столбец `commit` назван как в `file_refs`: `COMMIT` —
-- ключевое слово SQL). `gen` растёт при каждом изменении карты и входит в `etag`; ранги,
-- посчитанные по устаревшим входам, по нему не сохраняются (`zam_store::code`).
CREATE TABLE code_snapshots (
    solution_guid TEXT PRIMARY KEY,
    git_commit    TEXT,
    branch        TEXT,
    machine       TEXT,
    updated_at    TEXT NOT NULL,
    files         INTEGER NOT NULL DEFAULT 0,
    symbols       INTEGER NOT NULL DEFAULT 0,
    gen           INTEGER NOT NULL DEFAULT 0,
    etag          TEXT NOT NULL DEFAULT '',
    rank_state    TEXT NOT NULL DEFAULT 'pending' CHECK (rank_state IN ('ok', 'pending')),
    ranked_at     TEXT
);

-- ---------- файлы ----------
-- Виды языка и причины пропуска проверяет код (`zam_code::{Lang, SkipReason}`), а не CHECK:
-- новые языки (§14) не потребуют пересборки таблицы. Удаление снимка удаляет файлы, файла —
-- его символы и ссылки (каскад).
CREATE TABLE code_files (
    id            INTEGER PRIMARY KEY,
    solution_guid TEXT NOT NULL REFERENCES code_snapshots (solution_guid) ON DELETE CASCADE,
    path          TEXT NOT NULL,
    lang          TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    git_oid       TEXT,
    size          INTEGER NOT NULL,
    parse_state   TEXT NOT NULL CHECK (parse_state IN ('ok', 'partial', 'skipped')),
    skip_reason   TEXT,
    symbols       INTEGER NOT NULL DEFAULT 0,
    rank          REAL NOT NULL DEFAULT 0,
    git_commit    TEXT,
    branch        TEXT,
    machine       TEXT,
    user_id       TEXT,
    updated_at    TEXT NOT NULL,
    UNIQUE (solution_guid, path)
);

-- ---------- символы ----------
-- `share` — доля ранга файла (`zam_code::Ranked::symbol_share`), `rank` — ранг файла × доля;
-- с `focus` ранг символа — персонализированный ранг файла × сохранённая доля (§7).
CREATE TABLE code_symbols (
    id        INTEGER PRIMARY KEY,
    file_id   INTEGER NOT NULL REFERENCES code_files (id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    qual      TEXT NOT NULL,
    kind      TEXT NOT NULL,
    line      INTEGER NOT NULL,
    end_line  INTEGER NOT NULL,
    sig       TEXT,
    body_hash TEXT NOT NULL,
    exported  INTEGER,
    share     REAL NOT NULL DEFAULT 0,
    rank      REAL NOT NULL DEFAULT 0
);
CREATE INDEX code_symbols_file ON code_symbols (file_id);
CREATE INDEX code_symbols_name ON code_symbols (name);
CREATE INDEX code_symbols_qual ON code_symbols (qual);

-- FTS символов: самостоятельная таблица, как `vault_fts` (0018); `rowid` = `code_symbols.id`
-- (удаление строки по `rowid` — точечное, по столбцу UNINDEXED был бы полный перебор).
-- `terms` — слова имени и частей `qual` (`zam_code::fts_terms`: `getUserById` → `get user by id`).
-- Строки пишет `zam_store::code` вместе с символом, удаляет триггер — в том числе при каскаде.
CREATE VIRTUAL TABLE code_symbols_fts USING fts5 (
    name, qual, terms,
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER code_symbols_fts_delete AFTER DELETE ON code_symbols BEGIN
    DELETE FROM code_symbols_fts WHERE rowid = old.id;
END;

-- ---------- агрегированные ссылки файла ----------
CREATE TABLE code_refs (
    file_id    INTEGER NOT NULL REFERENCES code_files (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    count      INTEGER NOT NULL,
    first_line INTEGER NOT NULL,
    PRIMARY KEY (file_id, name, kind)
) WITHOUT ROWID;
-- потребители имени (`/code/impact`, карточка символа)
CREATE INDEX code_refs_name ON code_refs (name);

-- Таблицы рёбер графа файлов (`code_file_edges` §6 спецификации) нет — решение контроллера по
-- ревью задачи 2: граф по именам плотный (вкладов «ссылки × определители», до 10^8 на 50 000
-- файлов), он строится из `code_symbols`/`code_refs` при ранжировании, рёбра между показанными
-- файлами считаются по запросу; хранятся только ранги (`code_files.rank`, `code_symbols.rank`).
