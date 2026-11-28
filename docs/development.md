# ZeroAIMemory: разработка

Окружение, сборка, тесты, eval, замеры и CI (перенесено из README 2026-11-28).

## Окружение

Тулчейн Rust закреплён в `rust-toolchain.toml` (1.99.1, edition 2024). Он ставится не в
систему, а в каталог проекта `.toolchain/`. Перед работой подключите окружение — скрипт
выставит `RUSTUP_HOME`, `CARGO_HOME`, `PATH`, перенаправит временные файлы и кэш npm в
`.tmp/`:

```
. scripts/dev-env.ps1        # PowerShell
source scripts/dev-env.sh    # bash
```

Временные файлы создавайте только в `target/` и `.tmp/`, оба каталога в `.gitignore`. Для
WebUI нужен Node.js 22. Для упаковки на Windows нужна Visual Studio с VC Tools: каталог MSVC
runtime ищется через vswhere или задаётся в `ZAM_VC_CRT_DIR`.

## Сборка и тесты

```
cargo xtask fetch-deps --platform windows-x64      # Qdrant, ONNX Runtime CPU и GPU, pdfium → target/deps/
cargo xtask fetch-models --test                    # тестовая модель MiniLM → target/models/
cargo test --workspace
cargo build --release -p zam-server
cargo build --release -p zam-cli                   # отдельной командой, см. ниже
```

- `zam` для выпуска собирается отдельной командой: в одной команде с `zam-server` cargo
  объединяет фичи общих крейтов, и в `zam` попадают зависимости сервера, которых нет в сводке
  лицензий архива клиента (тест `release_cli_is_built_separately` в `xtask`).
- Упаковке (`cargo xtask package`) нужен cargo-about ровно 0.9.2
  (`cargo install cargo-about@0.9.2 --locked --features cli`), другая версия — ошибка. С M6
  упаковка проходит без сети, если реестр cargo, кэш npm, `web/dist`, `target/deps` и
  `tokenizer.json` уже получены: уточнённые тексты лицензий лежат в `deploy/licenses/`
  (`clarify.files` в `about.toml`), их SHA-256 и строки авторских прав в сводке проверяются.
- Тесты ML и индекса пропускаются, если нет ONNX Runtime или модели. С `ZAM_REQUIRE_ML=1`
  (так в CI) они вместо этого падают.
- Интеграционные тесты запускают настоящий Qdrant из `target/deps`, Docker не нужен.
  `ZAM_TEST_QDRANT_MAX` ограничивает число одновременно запущенных экземпляров в одном
  тестовом бинарнике (по умолчанию 4). На HDD это убирает падения тестов от взаимной нагрузки.
  Тесты сервера и `zam-index` (`worker.rs`) ждут создания коллекции до 240 с без
  продвижения: пока добавляются payload-индексы, ожидание продлевается, но не дольше 900 с.
  Тесты сервера печатают время готовности строкой `ZAM_TIMING векторный канал готов за …`.
- Замеры времени для приёмки помечены `#[ignore = "замер времени для приёмки: тихое окно"]`:
  `m4_acceptance::hook_budgets`, `m57_acceptance::acceptance_recall_p95_with_50_ideas`,
  `m6_acceptance::acceptance_hook_budgets`. Под общим прогоном на занятой машине их пороги
  проверить нельзя. Запускайте их отдельно, когда машина свободна (сборок, игр и редакторов
  нет), например
  `cargo test -p zam-cli --test m4_acceptance hook_budgets -- --ignored --exact`. Так же
  запускается `m57_acceptance`, а `m6_acceptance acceptance_hook_budgets` — с `--release` и
  бинарником предыдущего выпуска (`target/tags/<тег>/release`, см. описание теста). Пороги в них
  не ослаблены.
- Тесты `zam-index`, которые пишут в Qdrant, сначала проверяют свободное место на диске
  (по умолчанию 4096 МБ, порог задаёт `ZAM_TEST_MIN_FREE_MB`). Если места мало, тест
  пропускается со строкой
  `SKIP <тест>: свободно N МБ < M МБ на <путь> — освободите место (cargo xtask clean-test-tmp)`.
  В CI (переменная `CI`) и с `ZAM_REQUIRE_DISK=1` такой тест падает. Данные успешных тестов
  `zam-index` удаляются сразу. Каталоги прогонов, которые упали или были прерваны, удаляет
  следующий запуск набора.
- Данные тестов копятся в `target/tmp`, `target/<сборка>/tmp` (при `CARGO_TARGET_DIR`) и
  `target/test-tmp`. Убрать их можно командой `cargo xtask clean-test-tmp [--older-than 6h]
  [--dry-run] [--target-dir DIR]`. Она удаляет только записи, в дереве которых ничего не
  менялось дольше порога (не меньше 1m), поэтому данные запущенных тестов остаются.
  `target/tags`, `target/models`, `target/deps`, `target/deps-cache`, `target/dist`,
  `target/release` и всё вне `target/` команда не трогает. `--target-dir` убирает вместо
  `target/` другой каталог, но только внутри `target/` — например, копию для проверки.
- CI дополнительно проверяет `cargo fmt --check` и `cargo clippy --workspace --all-targets -- -D warnings`.

## Команды xtask

| Команда | Назначение |
|---|---|
| `fetch-deps --platform windows-x64\|linux-x64 [--flavor cpu\|gpu] [--print-hashes]` | зависимости поставки по манифестам `deploy/*.toml` с проверкой размера (`size`, если задан) и SHA-256 |
| `fetch-deps --prune-cache` | удалить из `target/deps-cache` архивы, sha256 которых нет ни в одном манифесте `deploy/*.toml` (прежние версии, `*.part`) |
| `fetch-models [--test] [--id ID]… [--accept-nc-license]` | модели из `deploy/models.toml` в `target/models/<id>/` |
| `package --platform … --flavor cpu\|gpu --version X.Y.Z [--with-models] [--cuda] [--no-kit]` | архивы поставки в `target/dist/` и `SHA256SUMS` |
| `smoke --archive … [--cuda-archive …] [--expect-device cpu\|cuda]` | smoke-тест комплекта: запуск, API, `zam`, Qdrant, векторный поиск на тестовой модели; документы — pdfium комплекта грузится рабочим процессом, PDF и DOCX находятся по фрагменту с `locator` и читаются по нему, превью листа XLSX и миниатюра PDF |
| `smoke-cuda --archive …` | проверка архива `zam-cuda`: штампы, состав, формат библиотек, лицензии |
| `changelog-section <версия>` | раздел версии из `CHANGELOG.md` для текста релиза |
| `eval [--device cpu\|cuda] [--mode fts\|hybrid\|hybrid+rerank]… [--quality-gate] [--grid FILE] [--check]` | качество поиска на наборе `eval/`: recall@5, MRR@10, nDCG@10, гейт int8 против fp32, подбор весов и чанкинга; `--docs` — eval вложений на `eval/docs/` (recall@5, точность `locator`, `--compare A --compare B` — парные разности); `--hints` — подсказки хуков (M4); с M6 — `--tokens --via mcp` (расход токенов на наборе вопросов ТС-1), `--sessions` (сессии ТС-2), `--grid` у подсказок и сессий, `--split dev\|holdout`; методика — [eval/README.md](../eval/README.md) |
| `docs-corpus` | набор документов `eval/docs/` детерминированно из сида (DOCX, XLSX, PPTX, PDF, враждебные файлы; в git не кладётся) |
| `code-bench [--repo <путь>]` | замеры карты кода (M5.6) на копии `HEAD` репозитория: первая и повторная `zam code sync`, правка и удаление файла, вторая копия, `GET /code/map` 200–2000 токенов, ключевые типы среди центральных, `zam --version` и запуск фоновой синхронизации p95 → `.tmp/m56/bench/report.json` |
| `bench --profile cpu\|gpu [--notes N] [--phase …]… [--keep] [--reuse]` | замеры целей §17 ТЗ на корпусе eval с копиями до 10 000 заметок: поиск при пустой и полной очереди, FTS-only, реранк, индексация, запись, reindex, RSS, старт, падение Qdrant |
| `ab --transcripts D --runs eval/ab/runs.toml [--server URL] [--out F] [--allow-missing]` | агентный A/B (M6): отчёт по плечам с интервалами из журналов агента (только поля `usage`) и `GET /sessions/{id}` сервера; методика — [eval/README.md](../eval/README.md), «Агентный A/B» |
| `clean-test-tmp [--older-than 6h] [--dry-run] [--target-dir DIR]` | уборка данных тестов: записи `target/tmp`, `target/<сборка>/tmp` и `target/test-tmp`, в дереве которых ничего не менялось дольше порога (`<число><s\|m\|h\|d>`, не меньше 1m); `--target-dir` — другой каталог, только внутри `target/` |
| `transfer-check seed\|verify\|bindings` | проверка переезда на другую машину (M6): корпус и ожидания на старом сервере, те же данные на новом после `restore` (`--no-key` — без мастер-ключа), базы без прежнего каталога данных и имени машины |

```
cargo xtask package --platform windows-x64 --flavor cpu --version 0.0.0-dev   # → target/dist/
cargo xtask smoke --archive target/dist/zam-server-0.0.0-dev-windows-x64-cpu.zip
```

`package --flavor gpu` собирает GPU-комплект, `--cuda` — архив `zam-cuda` (проверка —
`cargo xtask smoke-cuda --archive …`), `--with-models` — архивы `zam-models` (сначала
`cargo xtask fetch-models`), `--no-kit` — только эти архивы, без комплектов. Smoke
GPU-комплекта на машине с NVIDIA:
`--cuda-archive target/dist/zam-cuda-….zip --expect-device cuda`.

## Качество поиска (eval)

Набор `eval/` (ТЗ §18.2): 160 заметок на русском и английском, 197 запросов пяти видов, из них
23 % — перефразы без единого общего слова с ответом. Прогон обязателен при изменении
чанкинга, весов или моделей; результаты — в `eval/results/`, как запускать и что значат
метрики — в [eval/README.md](../eval/README.md).

```
cargo xtask eval --mode fts                                  # без моделей
cargo xtask eval --mode hybrid --mode hybrid+rerank          # bge-m3 int8 на CPU
cargo xtask eval --device cuda --mode hybrid --mode hybrid+rerank
cargo xtask eval --quality-gate                              # int8 против fp32 BAAI
```

Последний прогон (2026-10-20, nDCG@10 по всем запросам): FTS-only 0,69; hybrid int8 CPU
0,75, с реранком jina 0,83; hybrid fp16 CUDA 0,75, с реранком bge-reranker 0,83. Веса
подобраны на том же наборе, так что оценка после подбора завышена. Ухудшения поиска int8
относительно fp32 не обнаружено (95 % интервал Δ nDCG@10 −0,007…+0,012), но порог
косинуса dense 0,99 гейта int8 не проходит (0,978) — решение и альтернативы в журнале
[docs/plans/2026-10-19-m2-index-ml-log.md](plans/2026-10-19-m2-index-ml-log.md).

## Производительность (§17)

`cargo xtask bench --profile cpu` и `--profile gpu` (данные — в `target/bench/`, удаляются после
прогона; итог — `eval/results/<дата>-bench-<профиль>.md`). Последние прогоны (2026-10-20, код
`d1d2521`, Ryzen 9 5900X, RTX 3090, данные на HDD, 10 тыс. заметок, тёплый кэш ОС, p95):
поиск `chunks` без реранка — CPU 71 мс (92 при полной очереди индексации), GPU 37 (52); с
реранком — CPU 1,45 с, GPU 114 мс; FTS-only — 30–32 мс; индексация — CPU 690–770 токенов/с
(≈ 1,7–1,9 чанка по 400 токенов), GPU 7141 токен/с на корпусе eval и 4323 на 10 тыс. заметок
(упор в HDD); запись до векторного индекса — CPU 1,98 с, GPU 0,46 с; `reindex` из кэша —
2250–2520 точек/с; старт до готовности Qdrant — 1,1–2,0 с.

Решения владельца по итогам замеров (2026-10-20, ТЗ v1.7 §17): цель индексации на GPU —
≥ 3000 токенов/с (RTX 3090, HDD); RSS с моделью на CPU — 1,5 ГБ (две сессии); память
реранкера jina — по факту `ml.rerank_sessions`; цель 5 с до готовности Qdrant — для штатного
перезапуска, а первый старт после массовой индексации (CPU 9,6 с, GPU 5,4 с — повтор WAL
Qdrant) исследуется отдельно; FTS-only 30–32 мс (цель 20), поиск на GPU при полной очереди
52 мс (цель 50) и индексация на CPU 690–770 токенов/с (цель — 3 чанка по 400 токенов) приняты
как есть. Открыто: RSS на GPU (808–1248 МиБ при 0,6 ГБ), RSS без моделей на 10 тыс. заметок
(54 МиБ при 50 МБ). Таблица и причины — в [docs/journal.md](journal.md).

## WebUI

Исходники лежат в `web/` (Svelte 5, Vite, TypeScript). Собранный `web/dist` встраивается в
бинарник `zam-server` при сборке. Если `web/dist` нет, сервер собирается с заглушкой.

```
cd web
npm ci
npm run check        # svelte-check
npm run build        # → web/dist
npm run size         # предел начального бандла (size-limit)
npm run e2e          # Playwright с Microsoft Edge; нужен target/release/zam-server
```

После первого `npm run build` пересоберите сервер с `ZAM_WEB_REBUILD=1`, иначе build-скрипт
не заметит появления `web/dist`.

## Структура репозитория

```
crates/
  zam-core/        # идентификаторы (UUIDv7, Crockford base32), токены
  zam-store/       # SQLite: миграции, узлы, ревизии, FTS5, вложения, эмбеддинги, очередь индексации
  zam-markdown/    # comrak: разбор с позициями, секции, PATCH-splice, рендер HTML, чанкер, предложения, L0/L1
  zam-secrets/     # сканер и маскировщик секретов §7.6 (только regex; реэкспорт zam_markdown::secrets)
  zam-docs/        # документы: sniff, извлечение DOCX/PPTX/XLSX/XLS/ODS/CSV/PDF/текста с лимитами, превью листа, миниатюры, locator
  zam-ml/          # ONNX Runtime: устройство CPU/CUDA, токенизатор, эмбеддер bge-m3, реранкер
  zam-index/       # VectorIndex и адаптер Qdrant, супервизор процесса Qdrant, воркер, reindex
  zam-search/      # гибридное слияние RRF, поправки, упаковка в бюджет токенов
  zam-fetch/       # загрузка по манифестам: SHA-256, распаковка, модели, библиотеки CUDA
  zam-server/      # axum: REST API, auth, WebUI, команды сервера
  zam-cli/         # клиент zam
web/               # WebUI (Svelte 5 + Vite), e2e на Playwright
xtask/             # cargo xtask: fetch-deps, fetch-models, package, smoke, smoke-cuda, eval, docs-corpus, bench, code-bench, ab, transfer-check
eval/              # набор eval: корпус, запросы, сетки, результаты (eval/README.md)
tools/             # вспомогательные скрипты (квантование bge-m3 для гейта качества)
deploy/            # манифесты зависимостей и моделей, пример конфига, systemd, лицензии, README-install.md
scripts/           # dev-env.ps1, dev-env.sh
docs/              # ТЗ, планы, исследования, журнал работ
.github/workflows/ # ci, package, cuda, release
```

Целевая структура описана в ТЗ §20. Крейт `zam-docs` и каталоги `benches/`,
`docs/adr/` появятся на следующих задачах и этапах.

## CI и релиз

- `ci.yml` — каждый push и PR: web (check, build, size-limit), fmt, clippy и тесты на
  Windows и Linux, комплекты cpu и gpu обеих платформ со smoke (`package.yml`).
- `cuda.yml` — вручную и при изменении `deploy/cuda.toml` или кода загрузки и упаковки
  CUDA: загрузка библиотек NVIDIA и проверка архива `zam-cuda` на обеих ОС.
- `release.yml` — тег `vX.Y.Z`: тесты, все комплекты и архивы (`zam-cuda`, `zam-models`),
  проверка состава и предела 2 ГиБ на файл, черновик GitHub Release с текстом из
  `CHANGELOG.md`. Версия в теге должна совпадать с версией workspace.
- **Пробный прогон релиза без публикации** — push в ветку `release-dry-run` (например,
  `git push origin HEAD:release-dry-run`) или ручной запуск `release.yml` (Actions → Release →
  Run workflow, с любого ref, в том числе с тега). Выполняется всё то же, что на теге, с
  версией `0.0.0-dry`: сборка, `zam-cuda` и `zam-models`, smoke, проверка состава, размеров и
  `SHA256SUMS`. Задача `publish` пропускается, архивы остаются артефактами прогона. Его стоит
  запускать перед каждым тегом, если менялись упаковка, манифесты `deploy/` или workflow.

Изменения записываются в [CHANGELOG.md](../CHANGELOG.md) (Keep a Changelog), ход работ — в
[docs/journal.md](journal.md).
