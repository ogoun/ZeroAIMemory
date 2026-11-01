-- user DB, задача 4 M5 (спецификация M5 §3): вложения как цель индексации.
--
-- `attachment_docs` — документ вложения: пара (узел, блоб, имя) из `attachments` со своим id
-- (цель заданий `index_jobs.target_kind = 'attachment'` и владелец чанков `chunks.attachment_id`).
-- Чанки и права — на каждое вложение; извлечение — одно на блоб (`blob_extract`). `deleted_at` —
-- метка удаления узла-владельца (та же, что у `nodes.deleted_at`: восстановление возвращает пакет)
-- или открепления. `attachments.text_state` (0002) не используется.
CREATE TABLE attachment_docs (
    id            TEXT PRIMARY KEY,
    node_id       TEXT NOT NULL REFERENCES nodes (id),
    blob_sha256   TEXT NOT NULL REFERENCES blobs (sha256),
    filename      TEXT NOT NULL,
    mime          TEXT,
    extract_state TEXT NOT NULL DEFAULT 'pending'
                  CHECK (extract_state IN ('pending', 'done', 'failed', 'unsupported')),
    -- причина `failed`: `parse_crash | parse_timeout | parse_oom | parse_error: …` (без текста)
    extract_error TEXT,
    ocr_needed    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    deleted_at    TEXT,
    UNIQUE (node_id, blob_sha256, filename)
);
CREATE INDEX attachment_docs_blob ON attachment_docs (blob_sha256);

-- Кэш извлечения по содержимому (одинаковый файл в разных заметках не разбирается повторно).
-- `units` — JSON `[{locator, heading_path, text}]`; новая версия извлечения — новый ключ.
CREATE TABLE blob_extract (
    blob_sha256       TEXT NOT NULL,
    extractor_version INTEGER NOT NULL,
    kind              TEXT NOT NULL,
    units             BLOB NOT NULL,
    warnings          TEXT NOT NULL DEFAULT '[]',
    ocr_needed        INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL,
    PRIMARY KEY (blob_sha256, extractor_version)
);

-- Чанк вложения: `node_id` — узел-владелец (область и права), `attachment_id` — документ.
-- Чанки заметки — `attachment_id IS NULL`: правка, перенарезка и удаление заметки их не трогают.
ALTER TABLE chunks ADD COLUMN attachment_id TEXT REFERENCES attachment_docs (id);
CREATE INDEX chunks_attachment ON chunks (attachment_id) WHERE attachment_id IS NOT NULL;

-- Дозаполнение из `attachments` (база M4.5). Id — 26 случайных символов Crockford base32 (первый
-- 0–7: 128 бит), как у `Id`; `% 8`/`% 32` от `random()` без `abs` переполнения. Вид документа
-- без чтения файла не определить: `pending` — по расширению поддерживаемых видов (`sniff` M5),
-- остальные — `unsupported`; воркер уточняет вид по содержимому.
INSERT INTO attachment_docs (id, node_id, blob_sha256, filename, mime, extract_state,
                             created_at, deleted_at)
SELECT substr('01234567', abs(random() % 8) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1)
       || substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', abs(random() % 32) + 1, 1),
       a.node_id, a.blob_sha256, a.filename, a.mime,
       CASE WHEN lower(rtrim(a.filename)) GLOB '*.pdf'
              OR lower(rtrim(a.filename)) GLOB '*.docx'
              OR lower(rtrim(a.filename)) GLOB '*.pptx'
              OR lower(rtrim(a.filename)) GLOB '*.xlsx'
              OR lower(rtrim(a.filename)) GLOB '*.xls'
              OR lower(rtrim(a.filename)) GLOB '*.ods'
              OR lower(rtrim(a.filename)) GLOB '*.csv'
              OR lower(rtrim(a.filename)) GLOB '*.tsv'
              OR lower(rtrim(a.filename)) GLOB '*.txt'
              OR lower(rtrim(a.filename)) GLOB '*.text'
              OR lower(rtrim(a.filename)) GLOB '*.md'
              OR lower(rtrim(a.filename)) GLOB '*.markdown'
              OR lower(rtrim(a.filename)) GLOB '*.log'
              OR lower(rtrim(a.filename)) GLOB '*.json'
              OR lower(rtrim(a.filename)) GLOB '*.jsonl'
              OR lower(rtrim(a.filename)) GLOB '*.yaml'
              OR lower(rtrim(a.filename)) GLOB '*.yml'
              OR lower(rtrim(a.filename)) GLOB '*.toml'
              OR lower(rtrim(a.filename)) GLOB '*.ini'
              OR lower(rtrim(a.filename)) GLOB '*.cfg'
              OR lower(rtrim(a.filename)) GLOB '*.conf'
              OR lower(rtrim(a.filename)) GLOB '*.xml'
              OR lower(rtrim(a.filename)) GLOB '*.html'
              OR lower(rtrim(a.filename)) GLOB '*.htm'
              OR lower(rtrim(a.filename)) GLOB '*.rst'
              OR lower(rtrim(a.filename)) GLOB '*.adoc'
            THEN 'pending' ELSE 'unsupported' END,
       a.created_at, n.deleted_at
FROM attachments a JOIN nodes n ON n.id = a.node_id;

-- Задания извлечения дозаполненным вложениям живых узлов (приоритет reindex, как перенарезка).
INSERT INTO index_jobs (target_kind, target_id, rev, op, state, attempts, priority, not_before,
                        enqueued_at, updated_at)
SELECT 'attachment', d.id, 0, 'upsert', 'queued', 0, 0,
       strftime('%Y-%m-%dT%H:%M:%S', 'now') || '.000000Z',
       strftime('%Y-%m-%dT%H:%M:%S', 'now') || '.000000Z',
       strftime('%Y-%m-%dT%H:%M:%S', 'now') || '.000000Z'
FROM attachment_docs d
WHERE d.extract_state = 'pending' AND d.deleted_at IS NULL;
