-- user DB, задача 7 M4.5 (спецификация M4.5 §6.1–6.3, §7): ссылки знаний на файлы решения.
-- Владелец ссылки — заметка (`node`, frontmatter `refs:`) или запись журнала (`entry`, поле
-- запроса `refs` и строки `data.refs.files` у changelog). `path` — путь относительно корня
-- решения, лексически нормализован (`zam_store::file_refs::normalize_ref_path`).
-- `solution_guid` — решение ссылки (пространство решения владельца или `X-Zam-Solution`);
-- NULL — ссылка только справочная, устарелость не отслеживается. `content_hash` — blake3
-- содержимого файла (64 hex) при записи, NULL — хэша нет (MCP-клиент): тогда ссылка
-- устаревает по любому сообщению об изменении после `recorded_at`.
CREATE TABLE file_refs (
  id INTEGER PRIMARY KEY,
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('node','entry')),
  owner_id TEXT NOT NULL,
  solution_guid TEXT,
  path TEXT NOT NULL,
  content_hash TEXT,
  git_commit TEXT,
  recorded_at TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'ok' CHECK (state IN ('ok','stale')),
  stale_at TEXT,
  stale_reason TEXT);
-- уникальность и выборка по владельцу (префикс `owner_kind, owner_id`)
CREATE UNIQUE INDEX file_refs_owner ON file_refs(owner_kind, owner_id, path);
-- сообщения об изменении и индекс для хуков: ссылки решения на путь
CREATE INDEX file_refs_path ON file_refs(solution_guid, path);
-- проверенные команды (§7): `verified_at:` (YYYY-MM-DD) и `verified_at_commit:` текущей ревизии
ALTER TABLE nodes ADD COLUMN verified_at TEXT;
ALTER TABLE nodes ADD COLUMN verified_at_commit TEXT;
-- дозаполнение из frontmatter живых заметок и `data.refs.files` changelog — в Rust при открытии
-- базы (разбор Markdown SQL не посчитает)
INSERT INTO meta (key, value) VALUES ('refs_backfill', 'pending');
