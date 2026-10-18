-- user DB, §6.3 ТЗ: ревизии только дописываются — и на уровне схемы.
-- Любая правка или удаление строки revisions прерывает оператор.

CREATE TRIGGER revisions_no_update BEFORE UPDATE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are append-only');
END;

CREATE TRIGGER revisions_no_delete BEFORE DELETE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are append-only');
END;
