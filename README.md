# ZeroAIMemory
A centralized, human-readable memory for AI tools.

Установка и эксплуатация — [`deploy/README-install.md`](deploy/README-install.md),
техническое задание — [`docs/specs/`](docs/specs/), журнал работ — [`docs/journal.md`](docs/journal.md).

## Разработка

Окружение: `. scripts/dev-env.ps1` (PowerShell) или `. scripts/dev-env.sh` — тулчейн Rust
из `.toolchain/`. Временные файлы — только в `target/` и `.tmp/`.

```
cargo xtask fetch-deps --platform windows-x64      # Qdrant, ONNX Runtime CPU и GPU, pdfium → target/deps/
cargo xtask fetch-models --test                    # тестовая модель → target/models/
cargo test --workspace
cargo build --release -p zam-server -p zam-cli
cargo xtask package --platform windows-x64 --flavor cpu --version 0.0.0-dev   # → target/dist/
cargo xtask smoke --archive target/dist/zam-server-0.0.0-dev-windows-x64-cpu.zip
```

`package --flavor gpu` собирает GPU-комплект, `--cuda` — архив `zam-cuda` (библиотеки NVIDIA,
проверка — `cargo xtask smoke-cuda --archive …`), `--with-models` — архивы `zam-models`
(сначала `cargo xtask fetch-models`), `--no-kit` — только эти архивы. Smoke GPU-комплекта на
машине с NVIDIA: `--cuda-archive target/dist/zam-cuda-….zip --expect-device cuda`.

### CI и релиз

- `ci.yml` — каждый push и PR: тесты на Windows и Linux, комплекты cpu и gpu обеих платформ
  со smoke (`package.yml`).
- `cuda.yml` — вручную и при изменении `deploy/cuda.toml` или кода загрузки и упаковки
  CUDA: загрузка библиотек NVIDIA и проверка архива `zam-cuda` на обеих ОС.
- `release.yml` — тег `vX.Y.Z`: тесты, все комплекты и архивы (`zam-cuda`, `zam-models`),
  проверка состава и предела 2 ГиБ на файл, черновик GitHub Release.
- **Пробный прогон релиза без публикации** — push в ветку `release-dry-run` (например,
  `git push origin HEAD:release-dry-run`) или ручной запуск `release.yml` (Actions → Release →
  Run workflow, с любого ref, в том числе с тега). Выполняется всё, что на теге, с версией `0.0.0-dry`: сборка, `zam-cuda` и
  `zam-models`, smoke, проверка состава, размеров и `SHA256SUMS`; задача `publish`
  пропускается, архивы остаются артефактами прогона. Стоит запускать перед каждым тегом,
  если менялись упаковка, манифесты `deploy/` или workflow.
