# Eval: hybrid+rerank, bge-m3-int8

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- реранкер `jina-reranker-v2`
- индексация корпуса: 241.9 с
- время запроса (клиент): p50 1565 мс, p95 1777 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.848 [0.798–0.894] | 0.812 [0.760–0.860] | 0.827 [0.780–0.871] |
| broad | 25 | 0.919 [0.819–0.992] | 0.926 [0.831–1.000] | 0.901 [0.816–0.971] |
| cross-lang | 38 | 1.000 [1.000–1.000] | 0.987 [0.961–1.000] | 0.990 [0.971–1.000] |
| exact | 31 | 0.968 [0.903–1.000] | 0.972 [0.917–1.000] | 0.978 [0.935–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.982 [0.946–1.000] | 0.986 [0.959–1.000] |
| paraphrase | 61 | 0.557 [0.442–0.672] | 0.459 [0.352–0.571] | 0.509 [0.406–0.613] |
| перефраз без общих слов | 46 | 0.413 [0.283–0.565] | 0.297 [0.197–0.414] | 0.360 [0.256–0.468] |
| запрос en | 88 | 0.873 [0.804–0.939] | 0.834 [0.762–0.900] | 0.850 [0.783–0.911] |
| запрос ru | 109 | 0.827 [0.755–0.899] | 0.795 [0.723–0.866] | 0.809 [0.745–0.874] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 83 %, trigram 27 %, vector 100 %, vector_only 16 %.

## Промахи: ожидаемых заметок нет в top-10 (20)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/windows/file-locked-running-exe, ru/docker/rootless-podman, ru/rust/arc-mutex-deadlock
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/zam/qdrant-managed-process
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank, ru/devtools/semver-and-changelog
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/ffi-cstring-repr-c, ru/research/bm25-parametry, en/research/chunking-strategies
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless, en/db/connection-pool-exhaustion
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/dyn-trait-object-safety, ru/rust/arc-mutex-deadlock, en/db/postgres-explain-analyze
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/windows/powershell-pipeline-objects, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, ru/rust/string-str-cow
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/devtools/flaky-tests-quarantine, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/zam/architecture-overview, ru/devtools/uv-package-manager, ru/devtools/semver-and-changelog
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: en/devtools/playwright-flaky-e2e, en/git/worktree, en/docker/compose-healthcheck-depends-on
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, en/research/colbert-late-interaction, ru/research/hnsw-parametry
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/research/metriki-ir, ru/windows/powershell-encoding, ru/windows/git-autocrlf-gitattributes
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/devtools/npm-ci-clean-install, en/windows/powershell-pipeline-objects
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/zam/answer-cache-protocol, ru/zam/hybrid-search-rrf, en/research/chunking-strategies
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/build-rs-rerun-if-changed, en/git/bisect-run, en/rust/workspace-dependencies-and-profiles
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: ru/git/gitignore-tracked-file, en/linux/systemd-timers-vs-cron, en/rust/thiserror-vs-anyhow
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/ops/timeouts
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/git/clean-fdx, ru/devtools/local-build-cache
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/zam/qdrant-managed-process, ru/zam/reindex-from-embeddings, ru/zam/vault-secrets
