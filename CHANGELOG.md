# Журнал изменений

Формат основан на [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/),
версионирование — [SemVer](https://semver.org/lang/ru/).

## [Unreleased]

## [0.0.1-m0] - 2026-10-18

Этап M0: каркас, сервер, хранилище, аутентификация, админ-API, WebUI-заглушка, клиент `zam status` и поставка комплектов для Windows x64 и Linux x64.

### Added
- Каркас workspace: `rust-toolchain.toml`, корневой `Cargo.toml`, крейт `zam-core` (идентификаторы UUIDv7 в Crockford base32, токены: генерация, sha256, маска), скрипты окружения `scripts/dev-env.*`, `.gitattributes`.
- Сервер `zam-server`: слоистая конфигурация (файл, `ZAM_*`, флаги), каркас axum, `GET /api/v1/health`, ошибки RFC 9457 (`application/problem+json`), логи без секретов, `hash-password`, первый запуск (первый токен администратора печатается один раз в stderr в виде `ZeroAIMemoryToken=<токен>`).
- Хранилище `zam-store`: SQLite (WAL, писатель в отдельном потоке, пул читателей, миграции с копией файла перед ними), каталог данных с блокировкой единственного экземпляра, `system.db` (пользователи, токены, сессии, аудит, настройки), базы пользователей с фиксированными пространствами.
- Аутентификация: токены (хранится только sha256; режимы `rw`/`ro`), вход администратора по конфигурации (argon2id или `ZAM_ADMIN_PASSWORD`), cookie сессии (`HttpOnly`, `SameSite=Strict`), CSRF для изменяющих запросов, ограничение частоты (5 попыток/мин на IP), учёт доверенного прокси.
- Admin API: пользователи, выпуск и отзыв токенов, журнал аудита.
- WebUI-заглушка (`web/`: Svelte 5, Vite, bits-ui): вход администратора, пользователи и токены (токен показывается один раз). Встраивается в `zam-server` через memory-serve; `index.html` отдаётся с подстановкой `<base href>` и CSP, неизвестные пути вне `/api` и `/mcp` отдают `index.html`. Без собранного `web/dist` сервер собирается с заглушкой. Начальный бандл укладывается в лимит 300 КБ gzip (size-limit).
- CLI `zam` (`crates/zam-cli`): `zam --version` и `zam status` (сервер, токен по маске, пользователь, `solution.toml`; поиск настроек: флаги, `ZAM_URL`/`ZAM_TOKEN`, файлы проекта вверх от cwd (в каталоге — `CLAUDE.local.md`, `CLAUDE.md`, `AGENTS.md`), `~/.zeroaimemory/config.toml`; коды выхода 0/1/2/3). Без tokio и tracing; холодный старт `zam --version` в среднем 5.8 мс (цель ≤ 20 мс). Прокси из окружения учитываются (кроме loopback); битый `config.toml` и URL без `http(s)://` — ошибка конфигурации (код 1); файлы настроек > 1 МиБ пропускаются; таймауты: resolve 2 с, connect 2 с, ответ 5 с, всего 10 с.
- Поставка (`cargo xtask fetch-deps|package|smoke|changelog-section`): закреплённые манифесты зависимостей (`deploy/*.toml`: Qdrant 1.19.2, ONNX Runtime 1.31.0 CPU, pdfium) с обязательной проверкой SHA-256; архивы `zam-server-<ver>-<platform>-cpu.(zip|tar.gz)` и `zam-<ver>-<platform>.(zip|tar.gz)` в `target/dist/` + `SHA256SUMS` (только для собранных архивов); vcruntime140.dll (app-local для Qdrant), лицензии (`deploy/licenses/`, cargo-about, license-checker закреплённой версии, тексты лицензий пакетов бандла WebUI — `LICENSES/THIRD-PARTY-WEB-NOTICES.txt` из rollup-plugin-license), `deploy/systemd/zam-server.service`, `deploy/README-install.md`. `--flavor gpu` — ошибка до этапа M2. Smoke: запуск сервера из распакованного комплекта (окружение без `ZAM_*`, кроме `ZAM_ADMIN_PASSWORD`), health, первый токен, `zam status` с флагами и без флагов по блоку в `CLAUDE.md` (чистое окружение и пустой домашний каталог), вход администратора и токен `ro`, `qdrant --version`, тексты лицензий WebUI, остановка; внешние команды выполняются с таймаутом 15 с.
- CI (GitHub Actions): job `web` (check, build, size-limit, артефакт `web-dist`), job `check` (fmt, clippy, test на Windows и Ubuntu 22.04), job `package-smoke` (сборка и smoke-тест комплектов обеих платформ).
- Релизный конвейер `.github/workflows/release.yml`: по тегу `v*` собираются и проверяются комплекты Windows x64 (`x86_64-pc-windows-msvc`) и Linux x64 (`zam-server` — gnu, `zam` — musl), создаётся черновик GitHub Release с четырьмя архивами и общим `SHA256SUMS`; текст релиза берётся из этого журнала.
- Windows: `zam-server.exe` и `zam.exe` собираются со статическим CRT (`.cargo/config.toml`, `+crt-static`) и запускаются без VC Redistributable; smoke проверяет импорты через `dumpbin /dependents`.
- Версия workspace `0.0.1-m0` (совпадает с тегом; `release.yml` сверяет тег и версию), git-хэш в `--version`/health — 12 символов, перед публикацией релиза выполняется `cargo test --workspace` на обеих ОС.
- Тулчейн Rust закреплён: `rust-toolchain.toml` — `1.99.1`, во всех workflow `dtolnay/rust-toolchain` с `toolchain: 1.99.1`.
- E2E-тест WebUI (`web/e2e/`, `npm run e2e`, Playwright с установленным Microsoft Edge): вход администратора, создание пользователя, копирование токена, выпуск `ro`, отзыв, восстановление сессии по F5, выход; страница открывается по LAN-IP (без secure context). В CI на этапе M0 не запускается.
- `deploy/README-install.md`: публикация за Traefik (`base_path`, `trusted_proxies`, `Secure`-cookie по `X-Forwarded-Proto`, пример меток и файлового провайдера), порядок поиска настроек `zam`.

### Fixed
- Сборочные переменные `ZAM_GIT_SHA`, `ZAM_WEB_REBUILD`, `ZAM_WEB_INDEX`, `ZAM_VCRUNTIME_DLL` больше не читаются как слой конфигурации сервера (раньше `ZAM_GIT_SHA` из окружения CI останавливал `zam-server` ошибкой «unknown field git_sha»); в workflow `ZAM_GIT_SHA` задаётся только на шагах сборки.
- WebUI: «Копировать» работает по HTTP в LAN (без Clipboard API): токен в поле только для чтения, запасной путь `execCommand('copy')`, иначе подсказка скопировать вручную; сообщения показываются внутри диалога.
- Первый токен администратора выпускается повторно только если у администратора нет ни одного токена (включая отозванные); отозванные токены больше не приводят к выпуску нового `initial`.
- `README-install.md`: пароль для `hash-password` вводится интерактивно; конвейер `echo ... |` (ломается в Windows PowerShell 5.1 и `cmd`) заменён на `read -rs` для Linux.
- `NewToken` в отладочном выводе показывает только маску токена.
