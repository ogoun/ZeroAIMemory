-- user DB, задача 10 M4.5 (спецификация M4.5 §9.4–9.5): сессии субагентов и учёт выданных
-- токенов. `parent_id` — id сессии родителя (хук `subagent-start` создаёт сессию
-- `<session>:<agent_id>`), `agent_type` — тип субагента (`Explore`, `Plan`, свои), ≤ 128.
-- Внешнего ключа на `sessions(id)` нет: сессия родителя может быть удалена сроком хранения
-- раньше или создана позже субагента.
ALTER TABLE sessions ADD COLUMN parent_id TEXT;
ALTER TABLE sessions ADD COLUMN agent_type TEXT;
CREATE INDEX sessions_parent ON sessions(parent_id) WHERE parent_id IS NOT NULL;
-- выданное сессии по событию `/recall` и хуков с локальным выводом (заполняет задача 12):
-- `calls` — число выдач, `tokens` — оценка размера текста в токенах. Удаляется вместе с сессией.
CREATE TABLE session_tokens (
  session_id TEXT NOT NULL REFERENCES sessions(id),
  event TEXT NOT NULL,
  calls INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, event));
