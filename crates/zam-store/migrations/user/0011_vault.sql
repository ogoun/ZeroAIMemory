-- user DB, §6.3 и §7.6 ТЗ (этап M3): Vault пользователя. Значение (`secret`) — только
-- шифротекст (блоб `0x01 ‖ nonce(24) ‖ ciphertext‖tag`, шифрует сервер); у Vault нет чанков,
-- FTS и заданий индексации. `scope = 'knowledge'` — личный Vault, `scope = 'solution'` —
-- привязка к `agent_solution_id` (своего или общего решения; Vault участника лежит в его базе).
CREATE TABLE vault_items (
    id TEXT PRIMARY KEY, scope TEXT NOT NULL CHECK (scope IN ('knowledge', 'solution')), agent_solution_id TEXT,
    title TEXT NOT NULL, data TEXT NOT NULL DEFAULT '{}', secret BLOB NOT NULL, current_rev INTEGER NOT NULL DEFAULT 1,
    author_kind TEXT NOT NULL, user_id TEXT, token_id TEXT, session_id TEXT, machine TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT,
    CHECK ((scope = 'solution') = (agent_solution_id IS NOT NULL))
);
CREATE UNIQUE INDEX vault_live_title ON vault_items (scope, coalesce(agent_solution_id, ''), title) WHERE deleted_at IS NULL;

-- Ревизии записи Vault — полный снимок (значение — шифротекст своей ревизии); только дописываются.
CREATE TABLE vault_revisions (item_id TEXT NOT NULL REFERENCES vault_items (id), rev INTEGER NOT NULL, title TEXT NOT NULL,
    data TEXT NOT NULL, secret BLOB NOT NULL, ts TEXT NOT NULL, author_kind TEXT NOT NULL, user_id TEXT, token_id TEXT,
    session_id TEXT, machine TEXT, PRIMARY KEY (item_id, rev));

CREATE TRIGGER vault_revisions_no_update BEFORE UPDATE ON vault_revisions BEGIN
    SELECT RAISE(ABORT, 'vault_revisions are append-only');
END;

CREATE TRIGGER vault_revisions_no_delete BEFORE DELETE ON vault_revisions BEGIN
    SELECT RAISE(ABORT, 'vault_revisions are append-only');
END;
