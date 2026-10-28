-- system.db, §10.3 ТЗ (этап M4, R5): настройки пользователя на сервере. Пока это только
-- переключатели типов подсказок хуков `hooks.<тип>` (`session_start`, `prompt`, `pre_tool`,
-- `post_tool`, `checkpoint`; нет строки — значение по умолчанию `true`). Настройки — часть
-- учётной записи (как пароль и токены), а не памяти: в базе пользователя их нет.
CREATE TABLE user_settings (
    user_id TEXT NOT NULL REFERENCES users (id),
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,                      -- JSON
    PRIMARY KEY (user_id, key)
) WITHOUT ROWID;
