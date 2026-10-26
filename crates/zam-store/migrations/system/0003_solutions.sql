-- system.db, §6.2 и §11.6 ТЗ (этап M3): реестр решений и доступ участников.
-- Память решения хранится в базе владельца (users/<owner_id>.db, пространство space_id);
-- здесь — только реестр (GUID → владелец, пространство) и разрешения.
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
