# Eval: hybrid+rerank, bge-m3-fp16

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- реранкер `bge-reranker-v2-m3`
- индексация корпуса: 24.8 с
- время запроса (клиент): p50 92 мс, p95 101 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.862 [0.813–0.904] | 0.824 [0.774–0.869] | 0.838 [0.791–0.879] |
| broad | 25 | 0.872 [0.767–0.965] | 0.907 [0.813–0.980] | 0.877 [0.785–0.953] |
| cross-lang | 38 | 1.000 [1.000–1.000] | 0.987 [0.961–1.000] | 0.990 [0.971–1.000] |
| exact | 31 | 0.968 [0.903–1.000] | 0.971 [0.914–1.000] | 0.977 [0.932–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.623 [0.508–0.754] | 0.493 [0.385–0.605] | 0.544 [0.438–0.652] |
| перефраз без общих слов | 46 | 0.500 [0.348–0.652] | 0.339 [0.232–0.457] | 0.404 [0.294–0.516] |
| запрос en | 88 | 0.884 [0.816–0.943] | 0.863 [0.793–0.924] | 0.871 [0.804–0.930] |
| запрос ru | 109 | 0.844 [0.775–0.908] | 0.793 [0.725–0.861] | 0.811 [0.748–0.873] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 84 %, trigram 27 %, vector 100 %, vector_only 15 %.

## Промахи: ожидаемых заметок нет в top-10 (18)

- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/devtools/semver-and-changelog
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/research/kross-enkodery-rerank, en/devtools/conventional-commits
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/rust/ffi-cstring-repr-c, en/rust/e0499-two-mutable-borrows
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless, en/db/connection-pool-exhaustion
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/db/postgres-explain-analyze, en/rust/future-not-send-mutexguard-await, en/rust/dyn-trait-object-safety
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/windows/powershell-pipeline-objects, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/rust/future-not-send-mutexguard-await, en/linux/systemd-timers-vs-cron, en/devtools/gha-workflow-concurrency
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, en/git/interactive-rebase-autosquash, ru/devtools/code-review-checklist
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/ops/retries-backoff-jitter, ru/zam/architecture-overview, ru/devtools/semver-and-changelog
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/db/sqlite-wal, ru/docker/named-volume-backup, ru/zam/reindex-from-embeddings
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/windows/powershell-pipeline-objects, en/linux/disk-full-but-df-shows-space
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/zam/answer-cache-protocol, en/research/chunking-strategies, ru/zam/hybrid-search-rrf
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/docker/multistage-build-layer-cache, en/git/bisect-run, en/rust/build-rs-rerun-if-changed
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/git/worktree, en/git/merge-vs-rebase, ru/git/gitignore-tracked-file
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/docker/rootless-podman
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/local-build-cache, ru/devtools/semver-and-changelog
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/docker/compose-healthcheck-depends-on, en/devtools/playwright-flaky-e2e, en/rust/future-not-send-mutexguard-await
