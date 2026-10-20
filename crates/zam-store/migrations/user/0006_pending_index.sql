-- user DB, §17 ТЗ (этап M2, задача 15): частичный индекс заметок, ждущих векторной
-- индексации. `pending_count` считается в каждом ответе `/search`; без индекса это обход
-- всех узлов области — 6 мс на 10 тыс. заметок (замер `cargo xtask bench`), с индексом —
-- доли миллисекунды: в индекс попадают только заметки в очереди.
CREATE INDEX nodes_pending ON nodes (space_id, path)
    WHERE kind = 'note' AND index_state = 'pending' AND deleted_at IS NULL;
