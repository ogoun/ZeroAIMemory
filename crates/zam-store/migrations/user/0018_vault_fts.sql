-- user DB, задача 11 M4.5 (спецификация M4.5 §8): поиск по ОТКРЫТЫМ полям Vault. В индекс
-- попадают только `title` и открытый текст (`description`, `url`, `username`, `purpose`,
-- `address`, `limits`, `notes` из `data`); значение (`secret`) и шифротекст — никогда (§7.6 ТЗ).
-- Таблица самостоятельная (без `content=`): строки пишет код `vault_*` в транзакции записи,
-- мягкое удаление убирает строку. Токенайзер — как у `chunks_fts`.
CREATE VIRTUAL TABLE vault_fts USING fts5 (
    item_id UNINDEXED, title, open_text,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Дозаполнение живых записей.
INSERT INTO vault_fts (item_id, title, open_text)
SELECT id, title,
       coalesce(json_extract(data, '$.description'), '') || char(10)
    || coalesce(json_extract(data, '$.url'), '') || char(10)
    || coalesce(json_extract(data, '$.username'), '') || char(10)
    || coalesce(json_extract(data, '$.purpose'), '') || char(10)
    || coalesce(json_extract(data, '$.address'), '') || char(10)
    || coalesce(json_extract(data, '$.limits'), '') || char(10)
    || coalesce(json_extract(data, '$.notes'), '')
FROM vault_items WHERE deleted_at IS NULL;
