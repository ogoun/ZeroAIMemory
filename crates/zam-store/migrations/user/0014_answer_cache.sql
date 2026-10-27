-- user DB, задача 7 M4 (§6.3, §9.3, §9.4): кэш выжимок агентов и их источники.
-- Выжимка лежит в базе пространства кэша: по решению — в базе владельца решения (общая на
-- решение), по личным пространствам (Knowledge, Rules) — в базе пользователя. Источники
-- (`cache_deps`) — узлы и записи журналов той же базы с ревизией и хэшем текста цели (или
-- секции `section`, если она указана): по ним выжимка проверяется при чтении, по обратному
-- индексу `cache_deps_target` инвалидируется в транзакции писателя источника. `section` —
-- путь заголовков или '' (цель целиком): NULL в первичном ключе SQLite не уникален.
-- `query_vec` — dense-вектор запроса (f16 LE, как `embeddings.dense`), NULL без модели.
CREATE TABLE answer_cache (
  id TEXT PRIMARY KEY, space_id TEXT NOT NULL REFERENCES spaces(id), area TEXT,
  author_user_id TEXT NOT NULL, session_id TEXT, machine TEXT,
  scope_hash TEXT NOT NULL, query TEXT NOT NULL, query_norm TEXT NOT NULL, query_vec BLOB,
  summary TEXT NOT NULL, tokens INTEGER NOT NULL,
  state TEXT NOT NULL DEFAULT 'valid' CHECK (state IN ('valid','stale')),
  stale_reason TEXT,                  -- JSON: [{target, section?, reason: changed|deleted|superseded|retracted|new_sources}]
  created_at TEXT NOT NULL, last_used_at TEXT NOT NULL, hits INTEGER NOT NULL DEFAULT 0,
  promoted_to TEXT, deleted_at TEXT);
CREATE INDEX answer_cache_key ON answer_cache(scope_hash, query_norm) WHERE deleted_at IS NULL;
CREATE INDEX answer_cache_space ON answer_cache(space_id, last_used_at) WHERE deleted_at IS NULL;
CREATE TABLE cache_deps (
  cache_id TEXT NOT NULL REFERENCES answer_cache(id),
  target_kind TEXT NOT NULL CHECK (target_kind IN ('node','entry')),
  target_id TEXT NOT NULL, rev INTEGER NOT NULL, text_hash TEXT NOT NULL,
  section TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (cache_id, target_kind, target_id, section));
CREATE INDEX cache_deps_target ON cache_deps(target_kind, target_id);
