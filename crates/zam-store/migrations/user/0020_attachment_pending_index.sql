-- user DB, задача 10 M5: частичный индекс документов вложений, ждущих извлечения. `pending_count`
-- считается в каждом ответе `/search`; без индекса часть вложений — обход всех документов
-- области с подзапросом `index_jobs` на каждый: 4,4 мс на 10 тыс. и 97 мс на 100 тыс. документов
-- (замер `attachment_queries_scale`, журнал этапа), с индексом — только документы в очереди.
CREATE INDEX attachment_docs_pending ON attachment_docs (id)
    WHERE extract_state = 'pending' AND deleted_at IS NULL;
