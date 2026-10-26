-- user DB, задача 19b M3: частичный индекс живых детей папки. Выборки живого дерева
-- (`list_children` и свёртка L0 папок воркером, `has_children`, `child_count` карточек
-- папок, обход поддерева при удалении) ищут детей по `(space_id, parent_id)` с
-- `deleted_at IS NULL`; по `nodes_parent` они проходили и удалённых детей, и их работа
-- росла с корзиной. Удалённые строки в этот индекс не попадают. `nodes_parent` остаётся
-- для обходов, которым нужны и удалённые (восстановление, перенос).
CREATE INDEX nodes_live_children ON nodes (space_id, parent_id) WHERE deleted_at IS NULL;
