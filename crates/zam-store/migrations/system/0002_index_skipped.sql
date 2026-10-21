-- system.db, §8.1 ТЗ (этап M2): базы пользователей, исключённые из пересборки векторного
-- индекса (файл повреждён или не открывается). Хранится здесь, а не в базе пользователя:
-- её как раз нельзя открыть. Строка снимается, когда база снова открылась и ей поставлен
-- reindex; до этого исключение переживает перезапуск сервера.
CREATE TABLE index_skipped (
    user_id    TEXT PRIMARY KEY REFERENCES users(id),
    error      TEXT NOT NULL,          -- без текста заметок
    attempts   INTEGER NOT NULL,
    skipped_at TEXT NOT NULL
);
