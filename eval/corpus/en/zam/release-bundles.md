---
tags: [release, packaging, ci]
---
# Release bundles: cpu and gpu kits, zam-models, zam-cuda

Every tagged release must ship ready-to-run kits for **Windows x64 and Linux x64**. A release
missing either platform is not published. Pushing a `vX.Y.Z` tag runs `release.yml` on GitHub
Actions and produces a draft release with all archives, `SHA256SUMS` and notes taken from
`CHANGELOG.md`. The tag version has to match the workspace version.

## What gets published

| Archive | Contents |
|---|---|
| `zam-server-<ver>-<os>-x64-cpu` | server with embedded WebUI, CPU ONNX Runtime, pdfium, native Qdrant under `qdrant/<qver>/`, the `zam` client, `config.example.toml`, copies of `deploy/qdrant.toml` and `deploy/models.toml`, `README-install.md`, `LICENSES/` |
| `zam-server-<ver>-<os>-x64-gpu` | same, but ONNX Runtime with the CUDA execution provider plus `deploy/cuda.toml`; no NVIDIA libraries inside, falls back to CPU without a GPU |
| `zam-cuda-<ver>-<os>-x64` | the NVIDIA redistributables for offline machines |
| `zam-models-<ver>-<id>.zip` | one ONNX model per archive: `bge-m3-int8`, `bge-m3-fp16`, `bge-reranker-v2-m3` |
| `zam-<ver>-<os>-x64` | CLI only |

`<os>` is `windows` (zip) or `linux` (tar.gz).

## Platform details worth remembering

- **Windows.** `zam-server.exe` and `zam.exe` use the static CRT, but `onnxruntime.dll` imports
  MSVCP140, MSVCP140_1, VCRUNTIME140 and VCRUNTIME140_1, and the CUDA EP additionally needs
  MSVCP140_ATOMIC_WAIT. These DLLs ship app-local next to the binaries, so the VC++
  Redistributable is not required. Without them, vector search silently never came up on a
  clean machine.
- **Linux.** `zam-server` is built for `x86_64-unknown-linux-gnu` on Ubuntu 22.04 (glibc 2.35+),
  because ONNX Runtime and pdfium are only distributed for glibc and a musl binary cannot load
  them. `zam` and Qdrant are static musl builds.

## Why models are split per archive

GitHub limits a release asset to 2 GiB. The three bundled models together are about 2.9 GB, so
each model gets its own `zam-models-…` archive. `cargo xtask package` and the publish job both
check the size of every file before upload. The jina reranker is never bundled because of its
non-commercial license.

## Smoke test before publishing

Each kit (both OSes, both flavors) is unpacked into a clean directory and started with an
empty `data_dir`. The smoke test checks that Qdrant came up, writes a note, finds it via FTS
and via vector search using the small MiniLM test model (a paraphrase must land in the top 3
without `degraded`), runs `zam status` and stops the server — no Qdrant process may survive.
The GPU kit is smoke-tested on a CPU runner, where it must work on CPU.

## Dry run

Push to the `release-dry-run` branch (`git push origin HEAD:release-dry-run`) or start the
Release workflow manually. Everything runs with version `0.0.0-dry` except `publish`; archives
stay as run artifacts. Do this before any tag that touches packaging, `deploy/` manifests or
workflows.
