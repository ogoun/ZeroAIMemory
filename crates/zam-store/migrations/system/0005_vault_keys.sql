-- system.db, §6.2 и §7.6 ТЗ (этап M3): ключи данных Vault. На каждого пользователя — свой
-- 32-байтный ключ, обёрнутый мастер-ключом сервера (XChaCha20-Poly1305, AAD
-- `zam-ukey-v1 ‖ user_id`). Проверочный шифротекст мастер-ключа — в `settings` под ключом
-- `vault.key_check`. Мастер-ключ в базе не хранится.
CREATE TABLE user_keys (user_id TEXT PRIMARY KEY REFERENCES users (id), wrapped BLOB NOT NULL, created_at TEXT NOT NULL);
