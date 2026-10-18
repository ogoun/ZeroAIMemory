# Исследование 04. Аудит библиотек и поставка Qdrant без Docker

Дата: 2026-10-18. Версии сверены 18.10.2026 по crates.io, npm, GitHub API и исходникам. Размеры бандлов измерены сборкой esbuild `--bundle --minify` с последующим `gzip -9`. Пометка «не проверено» означает, что факт не подтверждён по источнику в этой сессии.

Повод — два замечания владельца:
1. Qdrant лучше ставить без Docker: для Windows и Linux он поставляется одним исполняемым файлом.
2. Нужно проверить, оптимальны ли выбранные библиотеки, особенно для Markdown.

## 1. Qdrant без Docker

### 1.1. Бинарники
- **Стабильная версия:** v1.19.2 от 2026-10-05 (https://github.com/qdrant/qdrant/releases/tag/v1.19.2).
- **Где взять:** готовые бинарники лежат только в GitHub Releases. Страница установки (https://qdrant.tech/documentation/installation/) предлагает Docker, Kubernetes, Cloud или сборку из исходников.
- **Состав релиза:**
  - `qdrant-x86_64-pc-windows-msvc.zip` — внутри один `qdrant.exe` на 86 МБ;
  - Linux x86_64 в вариантах gnu и musl;
  - Linux aarch64 — только musl;
  - macOS x86_64 и arm64;
  - `.deb` и AppImage.
- **Зависимости:**
  - **Windows:** нужен `VCRUNTIME140.dll`. Кладём app-local копию рядом с exe или требуем VC++ Redistributable.
  - **Linux gnu:** нужна glibc ≥ 2.39, поэтому поставляем **musl-сборки**.
  - **Аллокатор:** jemalloc есть только вне MSVC. На Windows работает системный аллокатор, влияние на скорость и память не измерено.

### 1.2. Запуск (исходники v1.19.2)
- **Аргументы:** `--config-path`, `--disable-telemetry`, `--version`.
- **Порядок слоёв конфигурации:**
  1. встроенный `config.yaml`;
  2. `config/*` из **текущего каталога**;
  3. файл из `--config-path`;
  4. переменные окружения `QDRANT__…`.

  Поэтому рабочий каталог процесса задаём явно.
- **Побочный файл:** при старте Qdrant создаёт `.qdrant-initialized` в рабочем каталоге. Путь меняется через `QDRANT_INIT_FILE_PATH`.
- **Значения по умолчанию, которые надо менять:**

  | Параметр | По умолчанию | Ставим |
  |---|---|---|
  | `service.host` | `0.0.0.0` | `127.0.0.1` |
  | `enable_cors` | `true` | `false` |
  | телеметрия | включена, вместе с отчётами о panic | `telemetry_disabled: true` |

- **Health-эндпоинты:** `/healthz`, `/livez` и `/readyz` в одноузловом режиме всегда отвечают 200 и не требуют ключа. Готовность проверяем по статусу коллекции, версию берём из `GET /`.
- **Остановка:**
  - **Linux:** SIGTERM или SIGINT.
  - **Windows:** Qdrant слушает только Ctrl+C. Надёжный вариант — сначала остановить запись (записи идут с `wait=true`), затем вызвать `TerminateProcess`. Целостность обеспечивает WAL Qdrant, а индекс у нас производный.
- **Блокировка каталога хранения:** не найдена (не проверено). Защищаемся своим лок-файлом.
- **Размещение данных:** в 1.19 `on_disk` устарел, вместо него уровни `memory: cold|cached|pinned`. Рекомендуемая схема: оригинальные векторы `cold` (mmap), int8-копии `pinned`.
- **Оценка памяти для 1 млн × 1024:**
  - int8 в RAM — около 1 ГБ;
  - HNSW (m=16) — около 0,15 ГБ;
  - sparse-векторы и payload — сотни МБ;
  - **итого 1,5–2,5 ГБ RAM;**
  - оригиналы float32 — 4,1 ГБ на диске.
- **ColBERT** на том же объёме занимает сотни гигабайт. Его можно хранить только `cold` без HNSW, а лучше не хранить вовсе: тогда он относится к v2.

### 1.3. Управление процессом
- **Привязка к родителю:**
  - **Windows:** Job Object с `KILL_ON_JOB_CLOSE`; порядок запуска `CREATE_SUSPENDED` → назначение в job → `ResumeThread`; флаг `CREATE_NO_WINDOW`.
  - **Linux:** `prctl(PR_SET_PDEATHSIG)` в `pre_exec`. Процесс порождаем из долгоживущего потока, а не из blocking-пула tokio.
- **Перед стартом:** проверяем pid-файл и порт, убиваем осиротевший процесс.
- **Перезапуск:** backoff от 1 до 60 с с jitter, circuit breaker.
- **Логи:** вывод Qdrant (`logger.format: json`) через pipe идёт в наш `tracing`.
- **Проверка версии и целостности:** `--version` и `GET /` сверяем с манифестом поставки, SHA-256 — с ним же.
- **Обновления формата хранения:** пропускать минорные версии нельзя (https://qdrant.tech/documentation/upgrades/). Индекс у нас производный, поэтому при прыжке через несколько версий, откате или ошибке миграции удаляем storage и пересобираем его из SQLite, без инференса.
- **Совместимость клиента:** `qdrant-client` 1.19.0 работает с сервером ±1 минорной версии.

### 1.4. Qdrant Edge (встраиваемая библиотека)
- **Что это:** крейт `qdrant-edge` 0.8.0 от 2026-08-05, Apache-2.0, официально в статусе **beta** (https://qdrant.tech/documentation/edge/).
- **Возможности:** именованные dense-векторы, multivector, sparse-векторы, квантизация, фильтры, payload-индексы, Prefetch, RRF, MMR, BM25. Поддержка Windows подтверждается Python-колесом `win_amd64`, но сборку Rust-крейта под MSVC не проверяли.
- **Ограничения:**
  - один шард;
  - `optimize()` синхронный, фоновых оптимизаторов нет — нужен свой фоновый поток;
  - работает только внутри одного процесса.
- **Риски:**
  - бета, за полгода вышло 4 минорных версии;
  - падение или нехватка памяти в Edge роняет весь сервер;
  - растут время сборки и размер бинарника.
- **Вывод:** держать вторым адаптером за трейтом `VectorIndex` под feature-флагом. Переходить, если прототип покажет выигрыш и выйдет версия 1.0.

### 1.5. Альтернативы
Ни одна не лучше Qdrant для нашего набора «dense + sparse + фильтры + RRF»:
- **lancedb** — нет sparse-векторов;
- **usearch, hnsw_rs** — только ANN-индекс;
- **sqlite-vec** — полный перебор;
- **tantivy** — нет dense-векторов.

## 2. Markdown

### 2.1. Сервер

| | comrak 0.56.0 | pulldown-cmark 0.13.4 | markdown-rs 1.0.0 |
|---|---|---|---|
| Модель | AST (arena) | поток событий | mdast |
| Позиции | строка:колонка, колонка в байтах UTF-8 | точные байтовые `Range`, в том числе для inline | line/col/offset |
| GFM-автоссылки | да | **нет** | да |
| Wikilinks | да (`NodeWikiLink`) | да (с 0.13) | **нет** |
| Alerts `> [!NOTE]` | 5 типов GitHub | 5 типов GitHub | нет |
| Math, frontmatter, атрибуты заголовков | да | да | частично |
| Подсветка | адаптер, есть syntect | нет | нет |
| Активность | активен, BSD-2 | активен, MIT | заморожен с 2025-04 |

**Вывод: comrak остаётся.** Требования к использованию:
- версия **≥ 0.56**: в ней закрыт DoS в autolink (GHSA-xg9p-p4jc-c46g) и патологические случаи emphasis;
- inline-позиции нестабильны: «все известные проблемы» закрыты только в 0.47 (2025-10). Нужны свои property-тесты и фаззинг, при необходимости — сверка с pulldown-cmark в тестах;
- callout в стиле Obsidian (`[!info]`, сворачивание `+/-`) comrak не поддерживает, пишем свой проход по AST;
- PATCH секции делаем только байтовой заменой фрагмента исходника (`splice`), без обратной сериализации через `format_commonmark`.

### 2.2. Главная находка: в ТЗ v0.4 два независимых парсера
Сервер разбирает Markdown через comrak, а WebUI — через markdown-it с плагинами. У плагинов своя семантика wikilinks, alerts и якорей, поэтому секции, якоря и ссылки, которые видит пользователь, могут не совпадать с теми, что индексирует сервер.

**Решение: сервер — единственный парсер.**
- Сервер строит HTML через comrak (`data-sourcepos`, id заголовков, разрешённые ссылки), санитизирует его через `ammonia` и кэширует по хэшу.
- Клиент только «оживляет» готовый HTML: формулы, Mermaid, медиа.
- Предпросмотр в редакторе — `POST /render` с debounce около 150 мс. comrak рендерит заметку быстрее чем за миллисекунду.
- Парсера в начальном бандле нет.
- На будущее: тот же крейт `zam-markdown`, собранный в WASM, даёт офлайн-предпросмотр. Готовая обёртка `comrak-wasm` весит 1,06 МБ gzip, своя минимальная сборка будет меньше (не измерено).

### 2.3. Клиентские библиотеки

| Задача | Было | Стало | Причина (gzip — измерено) |
|---|---|---|---|
| Парсер | markdown-it (41,6–47,9 КБ с плагинами) | — | парсит сервер |
| Подсветка в просмотре | highlight.js | syntect на сервере (адаптер comrak), CSS-классы | 0 КБ на клиенте; позже сравнить с arborium (tree-sitter) |
| Подсветка в редакторе | — | Lezer (входит в CM6) | 0 КБ сверх редактора |
| Формулы | KaTeX | KaTeX 0.19, лениво | 76,9 КБ + шрифты по требованию. Temml — 59 КБ, но нужен математический шрифт. MathJax 4 весит 281+ КБ. Перспектива — серверный `math-core` → MathML (молодой, не проверен) |
| Диаграммы | Mermaid | Mermaid 12, лениво, `securityLevel: 'strict'`, изоляция | около 206 КБ на flowchart. beautiful-mermaid весит 492 КБ |
| Редактор | CodeMirror 6 | CM6 `minimalSetup` + `lang-markdown` + свои расширения Lezer (wikilinks, alerts, math, сноски) + свой гибридный live-preview на декорациях | 104 КБ, отдельный ленивый чанк. ProseMirror, Milkdown и Tiptap нарушают побайтовую сохранность исходника. Готовые CM6 live-preview пакеты незрелые |
| Санитизация | DOMPurify | ammonia 4.2.4 на сервере (основная) + DOMPurify 3.4 только в точках вставки на клиенте (SVG Mermaid) + CSP | один источник правды о разрешённом HTML |

## 3. Остальной стек (Rust)

| Назначение | Было | Вердикт | Ключевое |
|---|---|---|---|
| HTTP | axum, tokio, tower-http | оставить | axum 0.8.9 |
| MCP | rmcp | оставить, **3.x** | 3.5.1, спецификация MCP 2026-07-28 (stateless) + совместимость с 2025-11-25. Встраивается в axum через `nest_service`. Bearer проверяем своим middleware (OAuth в rmcp только клиентский). Минорную версию закрепить |
| SQLite | rusqlite bundled | оставить | 0.40.2. sqlx даёт лишние накладные расходы. В turso/Limbo нет FTS5 |
| ONNX | ort 2.0 | оставить, закрепить `=2.0.0-rc.14` | ONNX Runtime 1.31; `load-dynamic`; готовые бинарники собраны только под CUDA 13; с rc.14 обязательно явное создание `Environment` |
| GPU на Windows | DirectML | **CUDA основной, DirectML — запасной** | DirectML в режиме sustained engineering, Microsoft продвигает WinML. Поддержку WinML в ort не нашли |
| Токенизатор | tokenizers | оставить, `default-features = false` | 0.23.2, есть 1.0-rc |
| Обёртка эмбеддингов | своя | своя; **fastembed-rs как образец** | fastembed 7.1.1 умеет bge-m3 dense + sparse + ColBERT, но тянет hf-hub и свою версию ort |
| candle, burn, tract | — | не брать | на CPU int8 медленнее ORT (экспертная оценка, без своего бенчмарка) |
| PDF | pdfium-render **или** pdf-extract | **pdfium-render основной + A/B с pdf_oxide** | pdf-extract и lopdf отбрасываем: у них низкая доля успешно разобранных файлов. pdf_oxide 0.3.78 — чистый Rust, но бенчмарк только авторский. mupdf под AGPL — исключить |
| DOCX, PPTX | zip + quick-xml | оставить | — |
| XLSX | calamine | оставить | 0.36.1; для больших листов — потоковое чтение |
| Изображения | image | **image + fast_image_resize** | быстрее за счёт SIMD. AVIF и HEIC требуют C-библиотек (dav1d, libheif под LGPL), по умолчанию выключены |
| Крипто | argon2, chacha20poly1305, blake3, sha2 | оставить | — |
| Конфиг | figment | **заменить**: serde + toml + свой слой env и CLI | последний релиз figment — 2024-05 |
| Логи, метрики | tracing, metrics | оставить | в путь хука `zam` tracing не инициализируем |
| CLI-парсер | clap | оставить | на время старта влияет ничтожно |
| HTTP в CLI | reqwest | **ureq 3** | синхронный, без tokio, быстрее холодный старт |
| Кэш триггеров | — | **rkyv** (zero-copy, mmap); готовые DFA через regex-automata | основное время старта уходит на компиляцию regex |
| ID | ULID | **UUIDv7** в хранилище, текстовая форма — Crockford base32 из 26 символов (совместима с ULID) | нативный тип Qdrant; короткая текстовая форма экономит токены |
| Время | — | jiff 0.2 (закрепить) | — |
| Диффы | — | similar (UI), imara-diff (большие тексты) | — |
| Подсчёт токенов | — | токенизатор bge-m3 | уже загружен |
| Предложения (L0) | — | unicode-segmentation + правила для сокращений | — |
| Раздача WebUI | — | memory-serve 2.4 | сжатие brotli/gzip при сборке, ETag |

## 4. WebUI (кроме Markdown)
- **Фреймворк:** Svelte 5.57 оставляем.
- **Компоненты:** bits-ui 2.x.
- **Виртуализация:** @tanstack/svelte-virtual.
- **PDF:** pdf.js 6.4 оставляем. EmbedPDF (pdfium wasm) нужен только для аннотаций.
- **DOCX:** docx-preview оставляем. mammoth теряет вёрстку.
- **XLSX:** calamine на сервере + своя виртуальная таблица. SheetJS и Univer избыточны.
- **Граф:** локальный — d3-force (SVG); общий на тысячи узлов — sigma 4 + graphology (WebGL, ForceAtlas2 в воркере).

## 5. Источники
- Qdrant: https://qdrant.tech/documentation/installation/, https://github.com/qdrant/qdrant/releases, https://qdrant.tech/documentation/upgrades/, https://qdrant.tech/documentation/capacity-planning/, https://qdrant.tech/documentation/guides/quantization/, https://qdrant.tech/documentation/edge/, https://crates.io/crates/qdrant-edge, https://crates.io/crates/qdrant-client
- Markdown: https://github.com/kivikakk/comrak (CHANGELOG), https://github.com/pulldown-cmark/pulldown-cmark/releases, https://github.com/wooorm/markdown-rs, https://github.com/rust-ammonia/ammonia, https://github.com/cure53/DOMPurify, https://github.com/KaTeX/KaTeX, https://github.com/ronkok/Temml, https://github.com/bearcove/arborium, https://github.com/1jehuang/mermaid-rs-renderer
- Стек: https://github.com/modelcontextprotocol/rust-sdk, https://github.com/pykeio/ort/releases, https://ort.pyke.io/setup/linking, https://github.com/Anush008/fastembed-rs, https://pdf.oxide.fyi/docs/comparison/rust, https://docs.turso.tech/sql-reference/functions/fts
- Скачанные для проверки бинарники Qdrant и замеры бандлов лежат в `.tmp/` (не в git).
