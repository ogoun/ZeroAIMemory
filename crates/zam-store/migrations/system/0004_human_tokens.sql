-- system.db, §7.5 и §11 ТЗ (этап M3): человеческий токен. Запись по токену с human = 1
-- атрибутируется как author_kind = human (ставит и меняет axiom, §7.5); выпускает его только
-- администратор. Существующие токены — агентские (0).
ALTER TABLE tokens ADD COLUMN human INTEGER NOT NULL DEFAULT 0 CHECK (human IN (0,1));
