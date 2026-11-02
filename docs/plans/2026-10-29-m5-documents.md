# M5. Документы — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal.** Вложения DOCX/XLSX/ODS/CSV/PPTX/PDF к заметкам извлекаются в структуру со ссылкой на место
(`locator`: страница, лист и диапазон, слайд, абзац), индексируются в FTS и Qdrant как уровень
`attachment`, ищутся, читаются по `locator` через `/read` и MCP, просматриваются в WebUI (pdf.js,
docx-preview, своя таблица, текст слайдов). Недоверенные файлы разбираются с лимитами, PDF — в
рабочем процессе того же бинарника.

**Architecture.**
- **Крейт `zam-docs`** — чистые функции `sniff`/`extract` над байтами с лимитами и дедлайном; PDF —
  модуль `pdf` с `pdfium-render`, вызываемый только из рабочего процесса.
- **Рабочий процесс** `zam-server doc-worker` (скрытая подкоманда, без tokio): кадры по stdin/stdout,
  пул в сервере (`zam-server/src/docs/pool.rs`), Job Object с лимитом памяти / `RLIMIT_AS` +
  `PDEATHSIG`, запуск из выделенного потока (как супервизор Qdrant).
- **Вложение — отдельная цель индексации.** Таблица `attachment_docs(id, node_id, blob_sha256,
  filename, …)`; `TargetKind::Attachment` в заданиях, воркере и точках Qdrant; чанки вложения несут
  `attachment_id` (и `node_id` владельца для прав и FTS-области). Все пути чанков заметки
  (`replace_chunks`, удаление и перенос узла, `delete_targets` в Qdrant) исключают
  `attachment_id IS NOT NULL`. Так правка заметки не трогает вложения, а перенос/удаление узла явно
  каскадирует на его вложения.
- **Извлечение — отдельная стадия до нарезки:** результат кэшируется по `blob_sha256` в
  `blob_extract`; воркер индексации получает готовые единицы.

**Tech Stack.** Новые зависимости (аудит — `docs/research/2026-10-29-m5-document-extraction.md`; версия —
не новее выпущенной за 2 недели до начала задачи, точное закрепление `=x.y.z`, запись в журнал):
`quick-xml` ≥ 0.41, `calamine` 0.36.x, `pdfium-render` 0.9.x (`dynamic`, без `bundled`), `image` 0.25
(`png`, `jpeg`, `gif`, `webp` — без C), `fast_image_resize`; WebUI — `pdfjs-dist` 6.x, `docx-preview` 0.4.
`zip` 9 уже в workspace. Кадры обмена — `serde_json` (уже есть).

**Spec:** `docs/specs/2026-10-29-m5-documents.md` (далее — «спецификация»), основа — ТЗ v1.17
(§5, §6.2, §8.1 п. 8, §9.1, §9.3, §12, §15.3, §17, §18). Карта кода —
`.tmp/m5/codemap-key.md` (ключевые места и подводные камни). Журнал этапа —
`docs/plans/2026-10-29-m5-documents-log.md`.

## Global Constraints

- **Временные файлы**, логи, тестовые данные — только `.tmp/` или `target/` (в тестах
  `env!("CARGO_TARGET_TMPDIR")`); запрещены `/tmp` (в Git Bash — `AppData\Local\Temp`), `%TEMP%`,
  scratchpad, `std::env::temp_dir()`. Субагентам — явно.
- **Окружение:** `. scripts/dev-env.ps1` перед cargo/npm; при нехватке памяти на линковке — `-j 2`;
  место на H: < 100 ГБ — удалить `target/debug/incremental` и `target/tmp`. Полный прогон ~2 ч —
  только в задачах 1, 5 и 12.
- **Тесты с процессами** — без `ZAM_*` и `AGENT_ENV_VARS`; тесты CLI — через
  `crates/zam-cli/tests/common::zam_cmd`, сторож `repo_clean` зелёный.
- **Литералы Rust с `\`** — через Edit, не shell-heredoc.
- **Платформы:** Windows x64 и Linux x64 (сервер — glibc); GPU — только CUDA; без Docker; без внешних
  офисных пакетов (LibreOffice запрещён, ТЗ §15.3).
- **Проверки задачи:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, затронутые наборы; WebUI — `npm run check|build|size|e2e`, `npm run test:build` (политика
  лицензий).
- **Единообразие:** русские тексты и комментарии (doc-комментарии ≤ 100 символов), английские
  идентификаторы, `thiserror`/`anyhow`, `ApiError` (RFC 9457), `tracing` без содержимого документов.
- **Миграции:** пользовательская `0019_attachments_index.sql` (задача 4); системных нет.
- **MCP:** 10 инструментов, описания ≤ 300 символов.
- **Инварианты:** Human-вложения — только FTS, не Qdrant, не выдача по токену; участник общего решения
  видит только вложения пространства решения; значения Vault и секреты из документов не в индексе
  (сканер секретов); история только дописывается; разбор недоверенного файла не роняет сервер.
- **Коммиты** — после каждой задачи, ветка `m5-documents` от main после merge M4.5, Conventional
  Commits по-русски, trailer по реальной модели, только своими путями.

## Review Focus

1. **Враждебный файл** (zip-бомба, XML с DTD/XXE, глубокая вложенность, PDF с бесконечным циклом
   страниц, битый файл, неверное расширение). Ожидание: частичный результат с `warnings` или
   `failed` с причиной, лимиты памяти и времени соблюдены, сервер работает. Тесты — задачи 1–3.
2. **Правка, перенос, удаление заметки с вложениями.** Ожидание: правка текста заметки не
   переиндексирует вложения и не теряет их точки; перенос пересчитывает `context_prefix` вложений по
   их формуле; удаление скрывает их из выдачи, восстановление возвращает. Тест — задача 4.
3. **Один файл в нескольких заметках и у разных пользователей.** Ожидание: извлечение один раз на
   `blob_sha256` (кэш), чанки и права — на каждое вложение отдельно; общий файл не протекает между
   пользователями. Тест — задача 4/5.
4. **Падение или зависание рабочего процесса посреди разбора.** Ожидание: вложение `failed`
   (`parse_crash|parse_timeout|parse_oom`), процесс перезапускается, очередь продолжается, на
   Windows и Linux процесс не переживает сервер. Тест — задача 3.
5. **Просмотр в WebUI недоверенного DOCX/PDF.** Ожидание: скрипты документа не исполняются, нет
   доступа к cookie и API из документа (sandbox-iframe), CSP главной страницы не ослаблена. Тест —
   задача 9.

---

## Карта файлов

```
crates/zam-docs/{Cargo.toml,src/{lib,sniff,limits,ooxml,docx,pptx,sheet,csv,pdf}.rs,tests/*}
crates/zam-docs/fuzz/ (nightly, вне CI) или proptest в tests/
crates/zam-server/src/{main.rs,docs/{mod,pool,protocol,worker}.rs,config.rs}
crates/zam-store/migrations/user/0019_attachments_index.sql
crates/zam-store/src/{attachments.rs (новый),blobs.rs,jobs.rs,nodes/{chunks,tree_ops}.rs,search.rs,user_db.rs}
crates/zam-core/src/target.rs
crates/zam-index/src/{worker.rs,qdrant.rs,edge.rs,vector.rs}
crates/zam-search/src/hybrid.rs
crates/zam-server/src/api/{blobs,read,search/mod,previews (новый)}.rs, mcp/tools.rs, web.rs
crates/zam-cli/src/cmd/{read,attach,search}.rs
web/src/components/viewers/{PdfViewer,DocxViewer,SheetViewer,SlidesViewer}.svelte, web/viewer-pdf.html
xtask/src/{docs_corpus.rs,eval/docs.rs,smoke.rs}, eval/{docs/,docs-queries.toml}
crates/zam-server/tests/m5_acceptance.rs
```

---

### Task 1: Крейт `zam-docs` — каркас, `sniff`, лимиты, DOCX и PPTX

**Files:** Create `crates/zam-docs/**`; Modify корневой `Cargo.toml` (workspace deps `quick-xml`).

**Interfaces — Produces:**
```rust
pub enum DocKind { Pdf, Docx, Pptx, Xlsx, Xls, Ods, Csv, Text }
pub struct Limits { pub max_bytes: u64, pub max_unpacked: u64, pub max_ratio: u32, pub max_entries: u32,
                    pub max_xml_depth: u32, pub max_cells: u64, pub max_chars: u64, pub max_pages: u32 }
impl Default for Limits { /* 100 МиБ, 500 МиБ, 100, 10_000, 256, 2_000_000, 20_000_000, 5000 */ }
pub enum Locator { Page(u32), Slide(u32), Sheet { name: String, range: String }, Para(u32) }
impl Locator { pub fn render(&self) -> String; pub fn parse(s: &str) -> Result<Self, DocError>; } // "page:12" | "slide:3" | "sheet:Итоги!A1:F40" | "para:40"
pub struct Unit { pub locator: Locator, pub heading_path: Vec<String>, pub text: String }
pub struct Extracted { pub kind: DocKind, pub units: Vec<Unit>, pub warnings: Vec<String>, pub ocr_needed: bool }
pub enum DocError { TooLarge, Unsupported, Malformed(String), Timeout, Internal(String) }
pub const EXTRACTOR_VERSION: u32 = 1;
pub fn sniff(bytes: &[u8], name: &str) -> Option<DocKind>;   // сигнатура (PK + [Content_Types].xml, %PDF-, OLE для XLS) → расширение
pub fn extract(kind: DocKind, bytes: &[u8], limits: &Limits, deadline: Instant) -> Result<Extracted, DocError>; // PDF → Unsupported (только в рабочем процессе)
```
- Общий модуль `ooxml`: чтение записей zip только `take(лимит)` в память, подсчёт фактических байт,
  отказ по `max_entries`, `max_unpacked`, степени сжатия; XML-читатель с `DocType` → ошибка,
  счётчиком глубины, без резолвера сущностей.
- DOCX: `word/document.xml` + `styles.xml` (стили заголовков по `w:styleId`/`w:name` Heading1..6,
  Title и `outlineLvl`), таблицы → `| a | b |`, сноски/колонтитулы — отдельные единицы в конце,
  `Locator::Para(n)` — порядковый номер абзаца тела.
- PPTX: `ppt/slides/slideN.xml` по порядку из `presentation.xml`, текст фигур; `notesSlideN.xml` —
  отдельная единица слайда с `heading_path` «Заметки докладчика».
- Превышение лимитов кроме `max_bytes` — частичный результат и `warnings: ["truncated: <лимит>"]`.

- [ ] **Step 1: Тесты** (`crates/zam-docs/tests/ooxml.rs`, фикстуры генерируются в тесте через `zip` — без бинарных файлов в репозитории): `docx_headings_tables_footnotes`, `docx_para_locator`, `pptx_slides_and_notes_order`, `sniff_by_signature_not_extension`, `zip_bomb_ratio_truncates`, `zip_too_many_entries`, `xml_doctype_rejected`, `xml_deep_nesting_truncates`, `deadline_partial_result`, `locator_render_parse_roundtrip`.
- [ ] **Step 2: proptest** `extract_never_panics` (случайные байты и случайные zip с XML) — без nightly.
- [ ] **Step 3:** Реализация; создать журнал этапа (шапка, ссылки, «Решения», «Уточнить у владельца» — §11 спецификации); полный прогон workspace; коммит `feat(docs): крейт zam-docs — DOCX и PPTX с лимитами`.

### Task 2: Таблицы — XLSX/XLS/ODS (`calamine`) и CSV

**Files:** `crates/zam-docs/src/{sheet,csv}.rs`, tests.

**Interfaces:** `extract` для `Xlsx|Xls|Ods|Csv`; `pub fn sheet_page(kind, bytes, sheet: &str, from: u32, rows: u32, limits) -> Result<SheetPage, DocError>` (для превью, задача 8): `SheetPage { sheets: Vec<String>, sheet, header: Vec<String>, rows: Vec<Vec<String>>, total_rows: u32, cols: u32 }`.
- Единица — блок строк под шапкой (первая непустая строка листа), до ~`chunk_target_max` токенов
  (оценка символами/3,5 — в крейте нет токенизатора; уточнение нарезки — в задаче 5),
  `Locator::Sheet { name, range: "A<from>:<col><to>" }`; `max_cells` — потолок по листам;
  общие строки XLSX больше `max_chars` — обрезка с `warnings`.
- CSV: RFC 4180, кодировка UTF-8 (BOM снимается) или Windows-1251 по эвристике (`encoding_rs`, уже
  транзитивно — добавить явно), разделитель из `, ; \t` по первым строкам.
- Если `calamine` тянет `zip` 8 — допустимо (решение контроллера, спецификация §11 п. 4), записать.

- [ ] **Step 1: Тесты:** `xlsx_sheets_blocks_with_header`, `xlsx_10k_rows_sheet_page_fast` (≤ 200 мс в release-тесте на генерированном файле; в debug — без порога), `ods_basic`, `csv_semicolon_cp1251`, `csv_quotes_newlines`, `max_cells_truncates`, `shared_string_bomb_truncates`.
- [ ] **Step 2:** Реализация, журнал. Коммит `feat(docs): таблицы XLSX/XLS/ODS и CSV`.

### Task 3: PDF в рабочем процессе `zam-server doc-worker`

**Files:** `crates/zam-docs/src/pdf.rs`; `crates/zam-server/src/{main.rs,docs/{mod,pool,protocol,worker}.rs,config.rs}`; `crates/zam-index` — вынести общие функции запуска дочернего процесса (`scrubbed_env`, PDEATHSIG, группа процессов) в `pub` модуль, без изменения поведения Qdrant; `deploy/config.example.toml`.

**Interfaces:**
```rust
// zam-docs::pdf (feature "pdfium")
pub fn extract_pdf(lib: &Pdfium, bytes: &[u8], limits: &Limits, deadline: Instant) -> Result<Extracted, DocError>;
pub fn render_thumb(lib: &Pdfium, bytes: &[u8], page: u32, max_px: u32) -> Result<Vec<u8> /*PNG*/, DocError>;
// zam-server::docs
pub struct DocPool; impl DocPool {
  pub fn new(cfg: &DocsConfig) -> Self;
  pub async fn extract(&self, kind: DocKind, bytes: Bytes) -> Result<Extracted, ParseFailure>;
  pub async fn thumb(&self, bytes: Bytes, page: u32, max_px: u32) -> Result<Vec<u8>, ParseFailure>;
}
pub enum ParseFailure { Crash, Timeout, Oom, Doc(DocError) }   // строки причин: parse_crash|parse_timeout|parse_oom
```
- `[docs]` конфигурации: `pdfium_dylib` (по умолчанию `<exe_dir>/pdfium.dll|libpdfium.so`, как ORT),
  `workers` 2, `worker_jobs` 200, `worker_memory_mb` 2048, `timeout_ms` 60000, лимиты `Limits`.
- PDF: текст постранично (`PdfPageText`), порядок колонок — XY-cut по прямоугольникам символов,
  колонтитулы — строки, повторяющиеся на ≥ 60 % страниц в верхних/нижних 8 % высоты; страница без
  текста с изображениями → `ocr_needed`; JS и вложения PDF не исполняются и не извлекаются.
- Рабочий процесс: скрытая подкоманда `doc-worker` в `main.rs` до создания tokio (как служба);
  своя инициализация `tracing` в stderr; кадр = u32 BE длина + JSON (`Request {Extract{kind, bytes:
  base64}|Thumb{…}|Ping}`, `Response`); stderr забирает родитель в журнал (`forward`).
- Пул: выделенный долгоживущий поток-спавнер (PDEATHSIG привязан к потоку-родителю); Windows — свой
  Job Object c `JOB_OBJECT_LIMIT_PROCESS_MEMORY` + `KILL_ON_JOB_CLOSE` (образец
  `xtask/src/procjob.rs:41-60`, feature `Win32_System_JobObjects`); Linux — `setrlimit(RLIMIT_AS)` в
  `pre_exec`; окружение без секретов (`SECRET_ENV`); тайм-аут — убийство процесса; после
  `worker_jobs` или падения — новый процесс; ошибка загрузки pdfium — `Doc(Unsupported)` с понятной
  причиной в `/index/status`.

- [ ] **Step 1: Тесты** (PDF-фикстуры генерируются в тесте простым писателем PDF в `zam-docs` dev — минимальный валидный PDF с текстом; двухколоночный, без текста): `pdf_pages_text`, `pdf_two_columns_order`, `pdf_headers_footers_removed`, `pdf_scan_ocr_needed`, `worker_roundtrip_extract`, `worker_crash_is_parse_crash_and_restarts` (запрос `Crash` под `cfg(test)`/фичей test-hooks), `worker_timeout_killed`, `worker_memory_limit_oom` (выделение сверх лимита в тестовом запросе), `worker_dies_with_server` (Windows и Linux), `missing_pdfium_unsupported`.
- [ ] **Step 2:** Тесты PDF требуют pdfium: `cargo xtask fetch-deps` кладёт его в `target/deps/<платформа>/pdfium/`; без библиотеки — пропуск с `eprintln!`, как тесты моделей.
- [ ] **Step 3:** Реализация, журнал (протокол кадров — решение исполнителя). Коммит `feat(docs): PDF в рабочем процессе doc-worker с лимитами памяти и времени`.

### Task 4: Хранилище — вложения как цель индексации (миграция 0019)

**Files:** `crates/zam-store/migrations/user/0019_attachments_index.sql`, `crates/zam-store/src/{attachments.rs,blobs.rs,jobs.rs,nodes/{chunks,tree_ops}.rs,search.rs,user_db.rs}`, `crates/zam-core/src/target.rs`, `crates/zam-server/src/api/blobs.rs`.

**Interfaces:**
```rust
// миграция 0019: attachment_docs(id TEXT PK, node_id, blob_sha256, filename, mime, extract_state CHECK
//   (pending|done|failed|unsupported), extract_error, ocr_needed, created_at, deleted_at), UNIQUE(node_id, blob_sha256, filename);
//   blob_extract(blob_sha256, extractor_version, units BLOB /*JSON*/, warnings, ocr_needed, created_at, PK(blob_sha256, extractor_version));
//   ALTER TABLE chunks ADD COLUMN attachment_id TEXT; индекс chunks(attachment_id); дозаполнение attachment_docs из attachments.
// zam-core
TargetKind::Attachment   // префикс "a"; во всех match — явная ветка (компилятор укажет места)
// zam-store::attachments
pub async fn list_attachments(db: &Db, node: Id) -> Result<Vec<AttachmentDoc>, StoreError>;
pub async fn detach(db: &Db, node: Id, sha: &str, filename: &str, attrib: &Attribution) -> Result<(), StoreError>;
pub async fn extract_cached(db: &Db, sha: &str) -> Result<Option<Extracted>, StoreError>;
pub async fn put_extract(db: &Db, sha: &str, e: &Extracted) -> Result<(), StoreError>;
pub async fn set_extract_state(db: &Db, att: Id, state: ExtractState, err: Option<&str>) -> Result<(), StoreError>;
pub async fn replace_attachment_chunks(db: &Db, att: Id, units: &[Unit]) -> Result<(), StoreError>; // ставит задание Attachment
```
- `attach` ставит задание `Attachment` для поддерживаемого вида (`sniff`), остальные — без задания.
- **Изоляция чанков** (Review Focus 2): `replace_chunks`/сравнение нарезки заметки, удаление узла
  (`tree_ops.rs:47`), пересчёт префиксов при переносе (`tree_ops.rs:358-365`) — только
  `attachment_id IS NULL`; перенос/удаление/восстановление узла каскадирует на `attachment_docs` и
  их задания (`Attachment`: пересчёт префикса — `upsert`, удаление — `delete`).
- `chunk_id` вложения — UUIDv5(`attachment_id ‖ ordinal ‖ blake3(text)`), без коллизий с заметкой.
- REST: `GET /nodes/{id}/attachments` (список с `extract_state`, `ocr_needed`), `DELETE
  /nodes/{id}/attachments/{sha}/{filename}` (открепление: `refcount−1`, чанки и точки — `delete`;
  блоб при `refcount = 0` остаётся — GC в M7).

- [ ] **Step 1: Тесты:** `migration_0019_backfills_attachment_docs`, `attach_enqueues_attachment_job`, `note_edit_keeps_attachment_chunks` (Review Focus 2), `node_move_recomputes_attachment_prefix`, `node_delete_hides_attachments_restore_returns`, `detach_removes_chunks`, `same_blob_two_notes_two_docs`, `attachment_chunk_ids_unique`.
- [ ] **Step 2:** Реализация (все `match TargetKind` — явные ветки), журнал. Коммит `feat(store): вложения как цель индексации, изоляция чанков, миграция 0019`.

### Task 5: Конвейер индексации вложений

**Files:** `crates/zam-index/src/{worker.rs,qdrant.rs,edge.rs,vector.rs}`, `crates/zam-store/src/{jobs.rs,user_db.rs,attachments.rs}`, `crates/zam-server/src/{app.rs,docs/mod.rs}`.

**Interfaces:**
- Стадия извлечения: воркер берёт задание `Attachment` → кэш `blob_extract` по
  (`sha`, `EXTRACTOR_VERSION`) → иначе `zam-docs::extract` (в `spawn_blocking` с дедлайном) или
  `DocPool::extract` (PDF) → `put_extract` → нарезка `Unit` → `replace_attachment_chunks` →
  эмбеддинги → точки. Сбой — `extract_state = failed` + причина, `index_state` вложения в
  `/index/status` (`failed_items` с `reason`).
- Нарезка: единица длиннее `chunk_hard_max` — по предложениям (общая функция нарезки заметок,
  `zam-markdown`), `locator` и `heading_path` наследуются; `context_prefix` —
  `[<space>/<путь заметки>] <имя файла> > <heading_path>`; сканер секретов (как в `note_chunks`).
- Точки Qdrant: `kind: "attachment"`, payload `node_id` (владелец, для области), `attachment_id`,
  `blob_sha256`, `locator`; `delete_targets` по `attachment_id`; удаление точек узла не задевает
  точки его вложений (`must_not attachment_id exists`).
- Human — только FTS (чанки пишутся, точки — нет), как у заметок Human.
- Отпечаток нарезки вложений `chunker_attachments` (включает `EXTRACTOR_VERSION`) → ветка в
  `RechunkPlan`/`rechunk_pending`; инвалидация кэша выжимок при появлении текста вложения
  (`cache::invalidate_target`).
- `parse_level`/`ChunkLevel` и `Level::parse` (`zam-search/src/hybrid.rs:69`) знают `attachment`.

- [ ] **Step 1: Тесты:** `attachment_indexed_fts_and_vector`, `pdf_attachment_via_worker` (с pdfium), `extract_cache_shared_by_sha` (Review Focus 3), `human_attachment_fts_only`, `secrets_masked_in_attachment_chunks`, `node_upsert_keeps_attachment_points`, `extractor_version_bump_rechunks`, `failed_extract_reported_in_index_status`.
- [ ] **Step 2:** Реализация; полный прогон workspace. Коммит `feat(index): индексация вложений — извлечение, нарезка, точки`.

### Task 6: Поиск по вложениям

**Files:** `crates/zam-server/src/api/search/mod.rs`, `crates/zam-store/src/search.rs`, `crates/zam-search/src/{hybrid,adjust}.rs`, `crates/zam-server/src/mcp/tools.rs`, `crates/zam-cli/src/cmd/search.rs`.

**Interfaces:**
- `DEFAULT_KINDS` += `attachment`; `scope_sql`: ветка заметок — `c.attachment_id IS NULL`, ветка
  вложений — `kinds ∋ attachment`, живой узел-владелец, живое `attachment_docs`.
- `FtsHit`/`ChunkMeta` — `attachment_id`, `blob_sha256`, `locator`, `filename`.
- Карточка: JSON `{kind: "attachment", id: "a:<chunk>", node, file, locator, locator_text, …}`;
  Markdown `a:<chunk> <space>/<путь заметки> › <файл> · стр. 12` (`лист «Итоги» A1:F40`,
  `слайд 3`, `абз. 40`).
- `max_per_node`: вложения считаются отдельно от заметки-владельца (своя квота
  `search.max_per_attachment` 2).
- Кэш выжимок: `kinds` в `scope_hash` — смена умолчаний записывается в CHANGELOG (выжимки с
  прежними умолчаниями пересоздаются, как в M4.5).

- [ ] **Step 1: Тесты:** `search_finds_docx_pdf_xlsx_pptx_with_locator`, `attachment_not_listed_as_note`, `attachment_quota_separate`, `kinds_note_only_excludes_attachments`, `participant_sees_only_solution_attachments`, `md_card_locator_text`.
- [ ] **Step 2:** Реализация, журнал. Коммит `feat(search): поиск по вложениям с locator`.

### Task 7: `/read` по `locator`, MCP и CLI

**Files:** `crates/zam-server/src/api/read.rs`, `crates/zam-server/src/error.rs`, `crates/zam-server/src/mcp/tools.rs`, `crates/zam-cli/src/cmd/read.rs`.

**Interfaces:**
- `split_ref`: `a:<chunk>` (чанк вложения → его документ и `locator` чанка по умолчанию),
  `b:<sha256>` (вложение по блобу в доступных пространствах; несколько — первое по дате и
  `warnings`); `Located::Attachment`.
- `locator` у вложения: текст единиц с этим `locator` (диапазон листа — пересечение блоков); без
  `locator` — оглавление (страницы/листы/слайды с первой строкой) и подсказка; у `n:`/записей
  `locator` — 422 `/problems/locator-invalid`; неверный формат — 422 `locator-invalid`.
  `locator-unsupported` удаляется.
- `session_seen` — `target_kind = 'blob'`, `part` = отрисованный `locator` (`WHOLE_TARGET` — весь
  документ).
- MCP `memory_read`: `locator` в схеме элемента; описание ≤ 300 символов. CLI: `zam read <ref>
  --locator <loc>`.

- [ ] **Step 1: Тесты:** `read_attachment_by_locator_page_sheet_slide_para`, `read_attachment_toc_without_locator`, `read_note_with_locator_422`, `read_bad_locator_422`, `read_b_sha_ref`, `read_seen_parts_per_locator`, `mcp_read_locator`, `cli_read_locator`.
- [ ] **Step 2:** Реализация, журнал. Коммит `feat(read): чтение вложений по locator`.

### Task 8: Превью — листы, миниатюры, текст

**Files:** `crates/zam-server/src/api/previews.rs` (новый), `crates/zam-store/src/datadir.rs` (`previews_dir()`), `crates/zam-server/src/api/mod.rs`, `crates/zam-docs` (миниатюры изображений — `image` + `fast_image_resize`).

**Interfaces:** маршрут **`GET /api/v1/previews/{sha256}?kind=sheet|thumb|text&…`** (не
`/blobs/{sha}/preview` — конфликт с `/{filename}`, карта кода): `sheet` — `SheetPage` (кэш
`previews/<sha>/sheet-<лист>-<from>.json`), `thumb&page=&px=` — PNG (PDF через `DocPool::thumb`,
изображения — ресайз; `px` ≤ 1024), `text` — PPTX текст слайдов, TXT/JSON/YAML/код — HTML syntect.
Права — как скачивание блоба (`locate_blob`); `Cache-Control: private, max-age=…`, `ETag` = sha +
параметры.

- [ ] **Step 1: Тесты:** `preview_sheet_paging`, `preview_sheet_10k_first_screen_under_1s` (release-порог, ТЗ §17), `preview_thumb_pdf_and_image`, `preview_text_pptx_and_code`, `preview_access_denied_for_foreign`, `preview_cache_reused`.
- [ ] **Step 2:** Реализация, журнал. Коммит `feat(previews): превью листов, миниатюры, текст`.

### Task 9: WebUI — вложения и просмотрщики

**Files:** `web/src/components/viewers/{PdfViewer,DocxViewer,SheetViewer,SlidesViewer}.svelte`, `web/src/components/DocView.svelte` (список вложений заметки, открытие просмотрщика), `web/src/lib/{api,router}.ts`, `web/viewer-pdf.html` + маршрут сервера `/viewer/pdf` с собственной CSP (`crates/zam-server/src/web.rs`), `web/build/license-policy.ts`, `web/.size-limit.json`, `web/e2e/documents.spec.ts`.

**Interfaces:**
- Список вложений заметки (`GET /nodes/{id}/attachments`): имя, размер, состояние извлечения
  (`ocr_needed` — «нужно распознавание»), открыть, открепить (с подтверждением).
- PDF: отдельный документ `/viewer/pdf?sha=&page=` в iframe; CSP только этого ответа — главная
  политика + `'wasm-unsafe-eval'` и `worker-src 'self' blob:` (решение контроллера, спецификация
  §11 п. 3); pdf.js `isEvalSupported: false`, без скриптов PDF; переход к странице из выдачи
  (`locator`); поиск по тексту pdf.js.
- DOCX: `docx-preview` → DOMPurify → `srcdoc` sandbox-iframe без `allow-scripts`/`allow-same-origin`
  (Review Focus 5); `jszip` — под MIT в `license-policy.ts`.
- XLSX/ODS/CSV: `SheetViewer` по `/previews?kind=sheet` — виртуальная прокрутка, закреплённая шапка,
  листы, переход к диапазону `locator`.
- PPTX: текст слайдов с навигацией.
- Просмотрщики — ленивые чанки (`{#await import}`); size-limit: `entry` ≤ текущий + 10 КБ, отдельные
  записи для чанков просмотрщиков.

- [ ] **Step 1: e2e:** `pdf_opens_at_locator_page`, `docx_renders_in_sandbox_no_script`, `xlsx_sheet_switch_and_range`, `pptx_slide_text`, `attachments_list_detach`, `main_csp_unchanged`.
- [ ] **Step 2:** `npm run check|build|size|e2e|test:build`; журнал, CHANGELOG. Коммит `feat(web): вложения и просмотрщики PDF/DOCX/XLSX/PPTX`.

### Task 10: Тестовый набор `eval/docs/` и eval документов, A/B PDF

**Files:** `xtask/src/{docs_corpus.rs,eval/docs.rs}`, `eval/docs-queries.toml`, `eval/README.md`, `eval/results/2026-10-xx-m5-*`.

**Interfaces:** `cargo xtask docs-corpus` — генерирует `eval/docs/` (синтетика §6 спецификации:
DOCX, XLSX 10 тыс. строк × 3 листа, PPTX 20 слайдов, PDF одноколоночный/двухколоночный/кириллица/
CJK/500 страниц/скан/враждебные) детерминированно из сида; бинарники корпуса в git не кладутся
(`eval/docs/` под `.gitignore`, в git — генератор и запросы). `cargo xtask eval --docs` — recall@5 по
вложениям и точность `locator` (≥ 30 запросов), бутстрэп CI, как M4/M4.5; режимы FTS, int8 CPU,
fp16 CUDA (GPU разрешён владельцем). A/B: `--pdf-backend pdfium|pdf_oxide` (pdf_oxide — только в
xtask как dev-зависимость для сравнения, в продукт — только при явном выигрыше, решение в журнал).
Скорость: PDF 500 страниц — время извлечения, превью XLSX — первый экран.

- [ ] **Step 1:** Генератор и раннер; тест `docs_corpus_deterministic`, `docs_eval_smoke`.
- [ ] **Step 2:** Прогоны, сводка `eval/results/2026-10-xx-m5-summary.md`, журнал. Коммит `test(eval): набор документов, eval вложений, A/B PDF`.

### Task 11: Smoke комплектов и поставка

**Files:** `xtask/src/smoke.rs`, `xtask/src/package.rs` (проверка, что pdfium в комплекте грузится), `deploy/README-install.md`, `README.md`.

- Smoke: вложение PDF и DOCX к заметке → индексация → поиск по фрагменту с `locator` → `/read` по
  `locator` → превью листа; проверка загрузки pdfium из каталога комплекта (`doc-worker` `Ping`).
- [ ] **Step 1:** Шаги smoke и тест раскладки комплекта; локальный прогон smoke пакета Windows; CI. Коммит `test(smoke): документы в комплектах`.

### Task 12: e2e приёмки M5, перенос в ТЗ, версия `0.0.7-m5`

**Files:** `crates/zam-server/tests/m5_acceptance.rs`, ТЗ (v1.18: содержание спецификации в §5, §6, §8,
§9, §12, §15, §16, §17, §19, §23), спецификация (статус «перенесено»), CHANGELOG `[0.0.7-m5]`, README,
`docs/journal.md`, журнал этапа, версии `Cargo.toml`/`web/package.json`.

- Тесты приёмки — спецификация §7: `documents_indexed_searchable_readable`, `hostile_files_contained`, `worker_killed_attachment_failed_server_alive`, `xlsx_preview_under_1s`, `human_attachment_not_in_qdrant`.
- [ ] **Step 1:** e2e, полный прогон `ZAM_REQUIRE_ML=1 cargo test --workspace --no-fail-fast`, smoke, CI зелёный (CI, Edge, CUDA).
- [ ] **Step 2:** Перенос в ТЗ, документы, «Итог M5», «Уточнить у владельца». Коммит `chore(release): 0.0.7-m5`.
