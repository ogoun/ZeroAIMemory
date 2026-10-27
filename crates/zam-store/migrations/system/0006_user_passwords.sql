-- system.db, §11.4 и §15.1 ТЗ (этап M4): вход пользователей в WebUI. Пароль задаёт
-- администратор, при первом входе пользователь обязан его сменить. Пароль администратора
-- по-прежнему только в конфигурации; NULL — вход в WebUI закрыт.
ALTER TABLE users ADD COLUMN password_hash TEXT;               -- argon2id; NULL — вход в WebUI закрыт
ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0 CHECK (must_change_password IN (0,1));
ALTER TABLE web_sessions ADD COLUMN user_id TEXT REFERENCES users(id); -- NULL — сессия администратора
