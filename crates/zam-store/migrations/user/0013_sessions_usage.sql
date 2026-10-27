-- user DB, задача 5 M4 (§6.3, §9.3, §10.5): сессии агентов, выданное сессии, показы и оценки.
-- `sessions` и `session_seen` лежат в базе пользователя запроса (сессия — его, даже если цель в
-- чужом решении; `session_seen.owner_id` — владелец базы цели); `usage` — в базе, где лежит
-- цель. `memory_writes` — записи в память с `X-Zam-Session` этой сессии (решение контроллера
-- R1, хук `checkpoint` §10.3). Срок хранения сессий — 30 дней от `last_seen_at` (воркер
-- обслуживания), `usage` — бессрочно.
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                -- внешний id сессии агента (X-Zam-Session / session_id хука), ≤ 128
  token_id TEXT, solution_guid TEXT, area TEXT, machine TEXT, agent TEXT,
  started_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, ended_at TEXT,
  memory_writes INTEGER NOT NULL DEFAULT 0);
CREATE INDEX sessions_last_seen ON sessions(last_seen_at);
CREATE TABLE session_seen (
  session_id TEXT NOT NULL REFERENCES sessions(id),
  owner_id TEXT NOT NULL,             -- пользователь-владелец базы цели (решение может быть чужим)
  target_kind TEXT NOT NULL CHECK (target_kind IN ('node','entry','blob')),
  target_id TEXT NOT NULL, rev INTEGER NOT NULL, seen_at TEXT NOT NULL,
  PRIMARY KEY (session_id, owner_id, target_kind, target_id));
CREATE TABLE usage (
  id INTEGER PRIMARY KEY, ts TEXT NOT NULL,
  user_id TEXT NOT NULL, session_id TEXT,
  target_kind TEXT NOT NULL, target_id TEXT NOT NULL,
  event TEXT NOT NULL CHECK (event IN ('recall_shown','useful','useless')),
  source TEXT                         -- recall: session_start|prompt|pre_tool|post_tool; feedback: agent|webui
);
CREATE INDEX usage_target ON usage(target_kind, target_id, event);
