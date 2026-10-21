# Eval: hybrid+rerank, bge-m3-fp16

- дата 2026-10-20, коммит `30cd824+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197 (отпечаток `5819d7ddcda33ef0`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- реранкер `bge-reranker-v2-m3`
- действующие `[search]` сервера: `{"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_hard_max":800,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 29.2 с
- время запроса (клиент): p50 122 мс, p95 591 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.857 [0.805–0.902] | 0.822 [0.771–0.868] | 0.834 [0.786–0.877] |
| broad | 25 | 0.872 [0.767–0.965] | 0.907 [0.813–0.980] | 0.877 [0.785–0.953] |
| cross-lang | 38 | 1.000 [1.000–1.000] | 0.987 [0.961–1.000] | 0.990 [0.971–1.000] |
| exact | 31 | 0.968 [0.903–1.000] | 0.971 [0.914–1.000] | 0.977 [0.932–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.607 [0.475–0.721] | 0.487 [0.380–0.600] | 0.532 [0.424–0.642] |
| перефраз без общих слов | 46 | 0.478 [0.326–0.630] | 0.330 [0.221–0.452] | 0.388 [0.277–0.505] |
| запрос en | 88 | 0.884 [0.816–0.943] | 0.861 [0.791–0.924] | 0.868 [0.800–0.927] |
| запрос ru | 109 | 0.835 [0.768–0.902] | 0.791 [0.721–0.859] | 0.807 [0.742–0.870] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 85 %, trigram 28 %, vector 100 %, vector_only 15 %.

## Промахи: ожидаемых заметок нет в top-10 (20)

- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/devtools/semver-and-changelog
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/research/kross-enkodery-rerank, en/devtools/conventional-commits
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/rust/ffi-cstring-repr-c, en/rust/e0499-two-mutable-borrows
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless, en/db/connection-pool-exhaustion
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/db/postgres-explain-analyze, en/rust/future-not-send-mutexguard-await, en/rust/dyn-trait-object-safety
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/windows/powershell-pipeline-objects, en/rust/e0502-mutable-borrow-while-shared
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/docker/multistage-build-layer-cache, en/rust/e0499-two-mutable-borrows
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/rust/future-not-send-mutexguard-await, en/linux/systemd-timers-vs-cron, en/devtools/gha-workflow-concurrency
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, en/git/interactive-rebase-autosquash, ru/devtools/code-review-checklist
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/ops/retries-backoff-jitter, ru/devtools/semver-and-changelog, ru/zam/architecture-overview
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/db/sqlite-wal, ru/docker/named-volume-backup, ru/zam/reindex-from-embeddings
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/windows/powershell-pipeline-objects, en/linux/disk-full-but-df-shows-space
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/metriki-ir, en/zam/answer-cache-protocol, ru/ops/backups-restore-testing
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/zam/answer-cache-protocol, en/research/chunking-strategies, ru/zam/hybrid-search-rrf
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/docker/multistage-build-layer-cache, en/git/bisect-run, en/rust/build-rs-rerun-if-changed
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/git/worktree, en/git/merge-vs-rebase, ru/git/gitignore-tracked-file
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/docker/rootless-podman
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/local-build-cache, ru/devtools/semver-and-changelog
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/rust/tokio-spawn-blocking, en/windows/wsl2-localhost-forwarding, en/devtools/playwright-flaky-e2e
