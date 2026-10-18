-- user DB, §6.3 ТЗ (этап M0: только пространства). Время — TEXT, формат zam_store::time.
CREATE TABLE spaces (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('rules', 'human', 'knowledge', 'solution')),
    solution_guid TEXT UNIQUE,
    name          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    archived_at   TEXT
);
