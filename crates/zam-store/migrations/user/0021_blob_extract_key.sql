-- user DB, финальное ревью M5 (M-1, M-2): ключ годности кэша извлечения `blob_extract`.
--
-- `src_kind` — вид, которым файл разбирали (`zam_docs::sniff_file`: CSV и текст различаются
-- расширением имени, один блоб под `a.csv` и `a.txt` — два вида); строка годится только тому же
-- виду. У строк извлечения он равен `kind`, у закэшированных неудач (`kind = failed |
-- unsupported`) прежних версий неизвестен (NULL) — такие разбираются заново один раз.
-- `limits_fp` — отпечаток лимитов разбора `[docs]`, при которых строка получена: результат с
-- `truncated: <лимит>` при других лимитах извлекается заново (пустой — неизвестен).
ALTER TABLE blob_extract ADD COLUMN src_kind TEXT;
ALTER TABLE blob_extract ADD COLUMN limits_fp TEXT NOT NULL DEFAULT '';
UPDATE blob_extract SET src_kind = kind WHERE kind NOT IN ('failed', 'unsupported');
