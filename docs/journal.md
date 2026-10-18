# Журнал работ

## 2026-10-17 — Сессия 1: от наброска к ТЗ
- Изучен набросок `ZeroAIMemory.md` и материалы по ссылкам из него.
- Проведены три исследования, результаты в `docs/research/`:
  - 01 — аналоги систем памяти (OpenViking, Graphiti, personal-os, basic-memory, mem0, Letta);
  - 02 — MD-движки (AppFlowy, AFFiNE, Aquilum);
  - 03 — стек (bge-m3, Qdrant, реранкеры, чанкинг, язык реализации, SQLite).
- Ответы пользователя:
  - язык — быстрый (Rust или Go), приоритет у скорости и экономии памяти;
  - главные боли — агент забывает разобранные ошибки (пример: CRLF/LF в Linux-скриптах) и пройденное в исследованиях;
  - память нужна общая для разных сессий и машин, один проект ведётся из разных мест;
  - нужен мощный и контролируемый инструмент.
- Написано ТЗ v0.1: `docs/specs/2026-10-17-zeroaimemory-tz.md`. Выбран язык Rust.
- Независимая проверка ТЗ («глазами исполнителя») выявила нереалистичные цели производительности и ряд противоречий. Исправлено, ТЗ v0.2 (см. §23 ТЗ).
- Открытые вопросы владельцу — §22 ТЗ. Следующий шаг: ответы → ТЗ v1.0 → план реализации.

## 2026-10-18 — Сессия 1 (продолжение): ТЗ v0.3
- Ответы владельца:
  - нужен собственный WebUI с полной поддержкой Markdown, медиа и просмотром DOCX/XLSX/PDF, совместимость с Obsidian не нужна;
  - сервис самодостаточный: никаких внешних энкодеров, работает и с GPU, и без него;
  - авторизация: админ из конфига, пользователи работают только по токену; токен хранится в CLAUDE.md/AGENTS.md;
  - vault доступен агенту, админ видит всё;
  - сервер слушает только LAN, внешний доступ — через прокси;
  - граф знаний закладываем;
  - Rust подтверждён.
- ТЗ переписано целиком, версия v0.3. Изменения перечислены в §23 ТЗ. Ждём оценки владельца.
- Новые замечания владельца:
  - выжимку и реранк делает сам агент: поиск отдаёт чанки и источники, агент дочитывает нужное и формирует память; токены экономить, использовать кэш;
  - WebUI выдаёт готовый текст для CLAUDE.md/AGENTS.md.
- ТЗ v0.4 по этим замечаниям:
  - протокол «кэш → карточки → чтение → `cache_put`» (§9.3);
  - кэш выжимок агентов (§9.4);
  - страница «Подключение агента» (§15.5);
  - удалены LLM на сервере и экстрактивная выжимка.
- Замечания владельца к v0.4:
  - Qdrant ставить без Docker: он есть одним исполняемым файлом;
  - проверить выбор библиотек, особенно для Markdown.
- Проведено исследование 04 (`docs/research/2026-10-18-04-libraries-audit.md`): Qdrant без Docker, Qdrant Edge, Markdown на сервере и клиенте, аудит всего стека.
  - Скачанные для проверки бинарники Qdrant и замеры бандлов лежат в `.tmp/`, в git не попадают.
- ТЗ v0.5:
  - нативный Qdrant под управлением сервера (§4.3);
  - единственный парсер Markdown — comrak в `zam-markdown`, HTML строит сервер (§5.3, §15.2);
  - замены библиотек по аудиту (§5.2); перечень — в §23 ТЗ.
- Владелец утвердил ТЗ с правками → **v1.0**:
  - GPU только CUDA, DirectML убран;
  - каждый релиз собирает комплекты Windows x64 и Linux x64 со всеми зависимостями, включая Qdrant (§16.3).
- Составлен план этапа M0 (каркас и конвейер релиза): `docs/plans/2026-10-18-m0-skeleton.md`, 15 задач. Планы M1–M7 составляются по одному, после завершения предыдущего этапа.
- Состояние машины: Rust не установлен (будет поставлен в `.toolchain/` внутри проекта), MSVC Build Tools, Node 22 и WSL2 есть, `gh` нет.

## 2026-10-18 — Этап M0 (каркас и конвейер релиза): итог
- Выполнены задачи 0–14 плана `docs/plans/2026-10-18-m0-skeleton.md` на ветке `m0-skeleton`:
  - `zam-core`, `zam-store`, `zam-server` (конфигурация, SQLite, каталог данных, аутентификация, admin API), WebUI-заглушка, CLI `zam status`;
  - `cargo xtask fetch-deps|package|smoke|changelog-section`, манифесты зависимостей (Qdrant 1.19.2, ONNX Runtime 1.31.0, pdfium) с проверкой SHA-256;
  - CI (`ci.yml`: web, check, package-smoke), релизный конвейер (`release.yml`) и переиспользуемый `package.yml` (сборка, упаковка и smoke-тест для Windows x64 и Linux x64).
- Замеры холодного старта `zam --version` (release, Windows 11, 50 запусков через `.tmp/bench-start.ps1`): среднее 5.8 мс, p95 7.7 мс при цели ≤ 20 мс.
- Локально на Windows: `cargo xtask package` и `cargo xtask smoke` — `SMOKE OK`.
- Не проверено локально (проверяется после push):
  - Linux-комплект целиком (сборка `zam-server` под gnu и `zam` под musl на ubuntu-22.04, упаковка tar.gz, smoke);
  - все workflow GitHub Actions: YAML проверен `actionlint` 1.7.12 (ошибок нет), но реальный прогон не выполнялся.
- Релиз: тег `v0.0.1-m0` (создаёт владелец после push ветки и зелёного CI) даст черновик GitHub Release с четырьмя архивами и `SHA256SUMS`; ссылка на релиз будет добавлена после его создания.
- Review Task 13 исправлен: `SHA256SUMS` строится только по собранным архивам (общий файл релиза пересчитывается в job `publish`), инструкция установки Linux согласована с юнитом systemd, license-checker закреплён (5.0.1), внешние команды smoke выполняются с таймаутом 15 с.
- Fix round 1 (ревью Task 14): статический CRT для Windows (exe не требуют VC Redistributable; smoke проверяет `dumpbin /dependents`), версия workspace `0.0.1-m0` с проверкой тега в `release.yml`, хэш коммита 12 символов, job `test` перед publish, `concurrency` в `ci.yml`.
- Финальное ревью ветки, волна исправлений:
  - C1: `zam-server` падал при `ZAM_GIT_SHA` в окружении CI («unknown field git_sha») — smoke в CI не прошёл бы ни на одной ОС. Сборочные переменные (`ZAM_GIT_SHA`, `ZAM_WEB_REBUILD`, `ZAM_WEB_INDEX`, `ZAM_VCRUNTIME_DLL`) исключены из слоя конфигурации, `ZAM_GIT_SHA` задаётся только на шагах `cargo build`, smoke и e2e запускают сервер без `ZAM_*` родителя (кроме `ZAM_ADMIN_PASSWORD`). Локально: `ZAM_GIT_SHA=abc` + package + smoke — `SMOKE OK`.
  - Критерий M0 «`zam status` с токеном из CLAUDE.md → код 0» покрыт шагом smoke (без флагов, из подкаталога, чистое окружение и пустой домашний каталог; источник URL и токена — файл).
  - WebUI проверен в браузере: Playwright + установленный Edge (`npm run e2e`), страница по LAN-IP без secure context; «Копировать» работает через запасной путь `execCommand('copy')` (Clipboard API по HTTP недоступен).
  - Тексты лицензий пакетов бандла WebUI (rollup-plugin-license 3.7.1) — `LICENSES/THIRD-PARTY-WEB-NOTICES.txt`; тулчейн закреплён на 1.99.1; README-install: интерактивный `hash-password`, раздел про Traefik, порядок поиска настроек `zam` (`CLAUDE.local.md` → `CLAUDE.md` → `AGENTS.md`).
  - Мелкие: маска в `Debug` у `NewToken`, первый токен восстанавливается только если у администратора нет ни одного токена, комментарий `audit.actor`, тест логов с отозванным токеном, многострочные литералы заменены на `\n`.
- Первый прогон CI по PR (run 39123068896): `check (ubuntu-22.04)` упал на тесте `xtask package::tests::dumpbin_picks_highest_version` — путь к `dumpbin.exe` собирался строкой с `\`, на Linux это один компонент имени. Исправлено: путь из отдельных компонентов. Linux-комплект (`package-smoke / linux-x64`) прошёл smoke с первого раза.
- Второй прогон (run 39123653004, `defef20`): web, check (ubuntu-22.04 и windows-latest) и package-smoke linux-x64 — success. Результат package-smoke windows-x64 проверить в следующей сессии. Затем merge в main и тег `v0.0.1-m0`.
- Итог M0: все критерии этапа проверены локально на Windows (тесты, fmt, clippy, actionlint, check/build/size-limit WebUI, package + smoke, e2e WebUI). Не проверены до push: Linux-комплект и реальный прогон GitHub Actions.

## 2026-10-18 — Этап M1 (заметки): итог
- Выполнены задачи 1–13 плана `docs/plans/2026-10-18-m1-notes.md` на ветке `m1-notes`; решения и отложенные замечания — `docs/plans/2026-10-18-m1-notes-log.md`:
  - `zam-markdown`: разбор с байтовыми позициями, секции, frontmatter, PATCH байтовым splice, рендер HTML (ammonia, syntect, callout, вложения);
  - `zam-store`: узлы, ревизии, история и откат, корзина и перемещение, чанки секций, FTS5 (bm25 + trigram, RRF), блобы вложений;
  - `zam-server`: REST API заметок, дерева, ревизий, рендера, вложений (Range, `sandbox`) и поиска (`degraded: true` — векторов ещё нет);
  - CLI `zam`: `tree`, `read`, `write`, `history`, `search`, `attach`; smoke комплекта с заметкой;
  - WebUI: дерево, просмотр документа с медиа, редактор с предпросмотром, история, поиск Ctrl+K, вложения.
- Версия workspace и WebUI — `0.0.2-m1`.
- Критерии M1 (§19 ТЗ) и тесты:
  - создание, правка, история, поиск через CLI — `crates/zam-cli/tests/notes.rs` (`notes_full_cycle`, `solution_toml_scopes_write_and_search`), шаг smoke `zam write → search → read`;
  - то же через WebUI — `web/e2e/editor.spec.ts` (правка, Ctrl+S, поиск Ctrl+K, история, дифф, откат; конфликт; CRLF; вложение), `web/e2e/notes.spec.ts` (документ, дерево, корзина);
  - инвариант PATCH — `crates/zam-markdown/tests/patch.rs` (proptest `splice_keeps_bytes_outside_changed_range`, CRLF и BOM), API — `crates/zam-server/tests/nodes.rs` (`patch_from_two_machines`, `put_requires_if_match_and_reports_conflict_diff`);
  - XSS-набор — `crates/zam-markdown/tests/xss.rs` (`xss_vectors_are_neutralized`, `xss_vectors_neutralized_inside_containers`), загруженные HTML/SVG — `crates/zam-server/tests/blobs.rs` (`svg_and_html_are_attachment_with_sandbox`);
  - синтаксис FTS5 в запросе — `crates/zam-server/tests/search.rs` (`fts_syntax_is_plain_words`); Human и токены — `human_hidden_from_token`, `ro_token_reads_but_never_writes`, `rules_write_gated_by_setting`.
- Прогоны (Windows 11, локально): `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` — чисто; `cargo test --workspace` — 391 passed, 0 failed, 2 ignored (бенчмарки FTS); `npm run check` — 0 ошибок; `npm run build`, `size-limit` — entry 54.95 КБ gzip из 300; `npm run e2e` (Edge) — 12 passed; `cargo xtask package` + `cargo xtask smoke` — `SMOKE OK`.
- Замеры §17 (release, `.tmp/bench-m1.ps1`, localhost, 1000 заметок по ~2 КБ, 100 повторов; каталог данных на HDD Seagate ST4000NE001; 4 прогона, в первом заметки ~2.9 КБ):
  - чтение узла по id (`GET /nodes/{id}?format=md`): p50 0.41–0.47 мс, p95 0.58–1.05 мс (цель ≤ 5) — выполнено;
  - FTS-поиск (`POST /search`, 1–3 слова): p50 4.5–5.3 мс, p95 8.5–11.0 мс (цель ≤ 20) — выполнено;
  - запись до FTS (`POST /nodes`, 201 после фиксации ревизии, чанков и FTS; заметка сразу находится поиском): p50 1.5 мс, p95 33–66 мс (цель ≤ 30) — **не выполнено на HDD**. 7–10 % записей ждут 370–720 мс при почти нулевом CPU сервера: это fsync автоматического checkpoint WAL на писателе (каждые 1000 страниц). Контрольный прогон с отключённым автоматическим checkpoint (локальная правка, не закоммичена): p95 2.84 мс, max 24.8 мс, загрузка корпуса 2.7 с вместо 21 с. Предложение — вынести checkpoint с пути записи (PASSIVE в фоне) — решение за владельцем / M2.
- Не проверено локально: Linux-комплект (gnu `zam-server`, musl `zam`) и smoke с заметкой — выполняются в CI (`package.yml`) после push; замер на SSD (правило: данные только в каталоге проекта, а он на HDD).

## 2026-10-18 — Этап M1: финальная волна исправлений
- По итогам финального ревью ветки `m1-notes` (подробности — раздел «Финальная волна исправлений» в `docs/plans/2026-10-18-m1-notes-log.md`): бюджет подсветки на документ, callout с кириллическим типом, стабильный proptest (зерно и `PROPTEST_RNG_SEED` в CI), кэш рендера с поколением ссылок, checkpoint WAL в фоновом потоке, чанк для заметок без текста, `index: false` только вне векторов, `pending_count`, `/spaces` без Human для токена, неизменяемые ревизии (миграция 0003), поиск WebUI в режиме `brief`, PATCH без ленивого продолжения абзаца, матрица доступа в тестах.
- Прогоны (Windows 11): `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` — чисто; `cargo test --workspace` дважды — 419 passed, 0 failed, 2 ignored; `npm run check` — 0 ошибок; `npm run build`, `size-limit` — entry 54.95 КБ gzip; `npm run e2e` — 12 passed; `cargo xtask package` + `smoke` — `SMOKE OK`.
- Замеры §17 после переноса checkpoint (release, `.tmp/bench-m1.ps1`, localhost, 1000 заметок по ~2 КБ, 100 повторов, HDD Seagate ST4000NE001; 6 прогонов):
  - чтение узла по id: p50 0.41–0.44 мс, p95 0.56–0.82 мс (цель ≤ 5) — выполнено;
  - FTS-поиск: p50 4.2–4.6 мс, p95 8.5–8.9 мс (цель ≤ 20) — выполнено;
  - запись до FTS: p50 1.3–1.5 мс, p95 2.6–2.9 мс в пяти прогонах и 6.0 мс в одном (цель ≤ 30; было 33–66) — выполнено; max 28–70 мс, в двух прогонах из шести одна запись ждала 0.47 с (ввод-вывод HDD во время фонового checkpoint).
