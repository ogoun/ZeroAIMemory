# Журнал изменений

Формат основан на [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/),
версионирование — [SemVer](https://semver.org/lang/ru/).

## [Unreleased]

### Added
- Каркас workspace: `rust-toolchain.toml`, корневой `Cargo.toml`, крейт `zam-core`, CI (GitHub Actions: Windows и Ubuntu 22.04), скрипты окружения `scripts/dev-env.*`, `.gitattributes`.

- WebUI-заглушка (`web/`: Svelte 5, Vite, bits-ui): вход администратора, пользователи и токены (показ токена один раз). Встраивается в `zam-server` через memory-serve; `index.html` отдаётся с подстановкой `<base href>` и CSP, неизвестные пути вне `/api` и `/mcp` отдают `index.html`. Без собранного `web/dist` сервер собирается с заглушкой. CI: job `web` (check, build, size-limit), артефакт `web-dist` для Rust-job.

- CLI `zam` (`crates/zam-cli`): `zam --version` и `zam status` (сервер, токен по маске, пользователь, `solution.toml`; поиск настроек: флаги, `ZAM_URL`/`ZAM_TOKEN`, CLAUDE.md/AGENTS.md, `~/.zeroaimemory/config.toml`; коды выхода 0/1/2/3).
