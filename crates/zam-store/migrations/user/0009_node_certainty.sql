-- user DB, §7.5 ТЗ (этап M3): эпистемический статус заметки — `certainty:` из frontmatter
-- текущей ревизии (без поля — NULL). Права на статус проверяет хранилище (zam_store::certainty).
ALTER TABLE nodes ADD COLUMN certainty TEXT CHECK (certainty IN ('axiom','theorem','hypothesis'));
-- Дозаполнение из frontmatter живых заметок — в Rust при открытии базы (разбор Markdown SQL не
-- посчитает); заметкам со статусом ставится upsert (статус попадает в payload Qdrant).
INSERT INTO meta (key, value) VALUES ('certainty_backfill', 'pending');
