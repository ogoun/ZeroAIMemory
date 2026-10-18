# Исследование 03. Стек: bge-m3, Qdrant, реранк, чанкинг, язык, SQLite

Дата: 2026-10-17. «(из памяти)» — без проверки по источнику в сессии.

## bge-m3
- XLM-R large, ~568M параметров, dense 1024 (CLS-пулинг), 8192 токена; за один проход: dense, sparse (лексические веса), multi-vector (ColBERT).
| Сервер | Dense | Sparse | ColBERT |
|---|---|---|---|
| FlagEmbedding (Python) | да | да | да (без сервера, нужна обёртка, напр. `m3serve`) |
| HF TEI | да | нет (sparse только для SPLADE) | нет |
| Infinity | да | нет | для ColBERT-моделей |
| Ollama / LM Studio / llama.cpp | да | нет | нет |
| **ONNX in-process** | да | да | да |
- ONNX: `yuniko-software/bge-m3-onnx` (C#/Java/Python, токенизатор тоже в ONNX), `onnx-community/bge-m3-ONNX`, `philipchung/bge-m3-onnx` (есть int8). Rust: `ort` + `tokenizers`.
- Подвохи: в ColBERT отбрасывать CLS-токен; sparse = max-pool по token-id без спецтокенов и нулей.

## Qdrant
- Named vectors: dense, sparse (modifier IDF), colbert (multivector, `max_sim`).
- Query API (1.10+): вложенный prefetch; fusion RRF (параметр k с 1.16, weighted с 1.17), DBSF (1.11); формулы/decay (1.14); group_by (1.11).
- Типичная схема: prefetch dense+sparse → RRF/DBSF → rescore ColBERT; для colbert HNSW отключить (`m=0`).
- 1.15: multilingual-токенизатор, стемминг, фразы; серверный BM25 через Inference API.
- Multitenancy: одна коллекция + payload-индекс с `is_tenant` (из памяти).
- Ссылки: https://qdrant.tech/documentation/search/hybrid-queries/, https://qdrant.tech/blog/qdrant-1.15.x/, https://qdrant.tech/documentation/inference/inference-bm25/

## Реранкеры
- bge-reranker-v2-m3 (568M, мультиязычный): CPU ~200–400 мс на запрос с несколькими документами; T4 ~80 мс; L4 p50 ~40 мс. Есть `bge-reranker-v2-m3-onnx-o3-cpu`.
- Альтернативы: jina-reranker-v2-multilingual (278M, быстрее), Qwen3-Reranker-0.6B, mxbai-rerank-v2.
- Совет: 30–50 кандидатов, обрезка до 512 токенов, реранк опционален на CPU.
- MemReranker (реранк для памяти агентов): https://arxiv.org/pdf/2605.06132

## Чанкинг и иерархический RAG
- Anthropic Contextual Retrieval (https://www.anthropic.com/news/contextual-retrieval): контекст чанка −35% неудачных извлечений, +BM25 −49%, +rerank −67%. Детерминированная дешёвая версия — breadcrumb из пути и заголовков.
- Late chunking (https://arxiv.org/abs/2409.04701): +~3% на BEIR, но bge-m3 dense обучен на CLS → не основной метод.
- RAPTOR (https://arxiv.org/abs/2401.18059): для заметок естественное дерево (папки/заголовки) лучше синтетической кластеризации.
- Схема: AST → разрез по H1–H3, код/таблицы/списки не рвать, frontmatter в метаданные; лист 300–500 токенов (макс. 800), overlap 10–15% только внутри секции, склейка секций <100 токенов; breadcrumb-префикс; резюме секций/заметок как «виртуальные чанки» с `level`; гибрид по листьям → group_by note → small-to-big; для широких запросов — сверху вниз по резюме; `content_hash` у узлов, точечная инвалидация.

## Суммаризация
- Сервер + LLM: готовая кэшируемая выжимка, но 1–10+ с, нужна GPU/API, риск галлюцинаций.
- Возврат чанков: быстро, детерминированно, агент сжимает под свою задачу, но тратит контекст.
- Рекомендация: `mode=chunks` по умолчанию, `mode=summary` с подключаемым провайдером, кэш по `(session, query, set(chunk_hash))`, в выжимке — ссылки на источники. MCP sampling / MRTR (спецификация 2026-07-28) — возможность использовать LLM клиента, поддержку в клиентах проверить.

## Язык
| | C# .NET 10 | Go | Rust | Python |
|---|---|---|---|---|
| Qdrant-клиент | офиц. 1.19 | офиц. | офиц. (основной) | офиц., самый полный |
| MCP SDK | офиц. v2.0 (2026-07) | офиц. v1.7 (2026-07) | `rmcp` (офиц.) | офиц. + FastMCP |
| SQLite+FTS5 | Microsoft.Data.Sqlite | mattn (cgo)/modernc | rusqlite (bundled) | встроен |
| ONNX | первоклассно | onnxruntime_go (cgo), токенизатор — трение | `ort` + `tokenizers` (отлично) | да |
| Markdown AST | Markdig | goldmark | comrak / pulldown-cmark | markdown-it-py |
| Один бинарник | AOT/single-file | лучший | отличный | плохо |
- Автор (ogoun) — C#-разработчик (ZeroLevel, ZeroLevel.SqLite/HNSW/Qdrant), но в приоритете скорость и память → выбор **Rust** (см. ТЗ, раздел «Стек»).

## SQLite
- Отдельная БД на пользователя: изоляция, бэкап копированием, свой WAL; минусы — миграции на N файлов (`PRAGMA user_version`, лениво при открытии), кросс-поиск через обход.
- PRAGMA: WAL, synchronous=NORMAL, busy_timeout=5000, foreign_keys=ON; один писатель на файл; не хранить в OneDrive.
- FTS5: external content + триггеры; `unicode61 remove_diacritics 2` + `trigram` для кода и подстрок; нет русского стемминга → закрывает sparse bge-m3.
- История: `note_revisions` append-only.
- sqlite-vec: pre-v1, brute force (~68 мс/100k×384); DiskANN/IVF в разработке → Qdrant остаётся основным, векторный индекс за абстракцией.
