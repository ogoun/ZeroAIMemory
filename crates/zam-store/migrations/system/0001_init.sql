-- system.db, §6.2 ТЗ. Время — TEXT RFC 3339 UTC.
CREATE TABLE users (
    id          TEXT PRIMARY KEY,            -- UUIDv7 (base32)
    name        TEXT NOT NULL UNIQUE,
    is_admin    INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
    created_at  TEXT NOT NULL,
    disabled_at TEXT
);

CREATE TABLE tokens (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id),
    name         TEXT NOT NULL,
    prefix       TEXT NOT NULL,              -- первые 8 символов секрета
    secret_hash  TEXT NOT NULL,              -- sha256 (hex)
    access       TEXT NOT NULL CHECK (access IN ('rw', 'ro')),
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    last_ip      TEXT,
    revoked_at   TEXT
);
CREATE UNIQUE INDEX idx_tokens_secret_hash ON tokens(secret_hash);
CREATE INDEX idx_tokens_user_id ON tokens(user_id);

-- id хранится как sha256 идентификатора из cookie
CREATE TABLE web_sessions (
    id         TEXT PRIMARY KEY,
    csrf_token TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    ip         TEXT
);

CREATE TABLE audit (
    id      INTEGER PRIMARY KEY,
    ts      TEXT NOT NULL,
    actor   TEXT NOT NULL,                   -- 'admin' | id токена
    user_id TEXT,
    action  TEXT NOT NULL,
    target  TEXT,
    ip      TEXT,
    details TEXT NOT NULL DEFAULT '{}'       -- JSON, без секретов
);
CREATE INDEX idx_audit_ts ON audit(ts);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL                      -- JSON
);
