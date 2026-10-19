# Eval: hybrid+rerank, bge-m3-fp16

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- реранкер `bge-reranker-v2-m3`
- индексация корпуса: 21.4 с
- время запроса (клиент): p50 87 мс, p95 107 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.788 [0.730–0.842] | 0.769 [0.713–0.822] | 0.775 [0.721–0.827] |
| broad | 25 | 0.892 [0.791–0.979] | 0.927 [0.840–1.000] | 0.898 [0.818–0.962] |
| cross-lang | 38 | 0.895 [0.789–0.974] | 0.882 [0.776–0.974] | 0.885 [0.780–0.974] |
| exact | 31 | 0.968 [0.903–1.000] | 0.971 [0.914–1.000] | 0.977 [0.932–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.443 [0.328–0.574] | 0.373 [0.263–0.488] | 0.397 [0.286–0.511] |
| перефраз без общих слов | 46 | 0.261 [0.130–0.391] | 0.169 [0.081–0.268] | 0.201 [0.108–0.302] |
| запрос en | 88 | 0.782 [0.693–0.861] | 0.781 [0.695–0.859] | 0.785 [0.700–0.862] |
| запрос ru | 109 | 0.794 [0.713–0.870] | 0.760 [0.680–0.836] | 0.767 [0.689–0.840] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 92 %, trigram 30 %, vector 100 %, vector_only 7 %.

## Промахи: ожидаемых заметок нет в top-10 (36)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/rust/arc-mutex-deadlock, ru/windows/git-autocrlf-gitattributes, ru/ops/postmortem-blameless
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/linux/ssh-config-agent-forwarding, en/linux/systemd-unit-restart, en/research/onnx-runtime-execution-providers
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: en/git/sparse-checkout-partial-clone, en/git/interactive-rebase-autosquash, en/git/submodules-pitfalls
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/devtools/typescript-strict-flags, en/research/chunking-strategies, en/git/interactive-rebase-autosquash
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/devtools/semver-and-changelog
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/research/kross-enkodery-rerank, ru/ops/postmortem-blameless
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/rust/ffi-cstring-repr-c, ru/ops/backups-restore-testing
- `b-18` (cross-lang): ожидалось en/git/bisect-run; top-3: ru/devtools/flaky-tests-quarantine, ru/git/revert-vs-reset, ru/git/blame-ignore-whitespace-moves
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/zam/windows-data-dir-path-limit, ru/db/postgres-zero-downtime-migrations, en/db/sqlite-fts5-search-index
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/devtools/python-venv, ru/db/postgres-zero-downtime-migrations, ru/devtools/code-review-checklist
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless, en/db/connection-pool-exhaustion
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/db/postgres-explain-analyze, en/rust/dyn-trait-object-safety, ru/rust/arc-mutex-deadlock
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/windows/powershell-pipeline-objects, en/zam/answer-cache-protocol
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/rust/future-not-send-mutexguard-await, en/devtools/gha-matrix-strategy, en/linux/systemd-timers-vs-cron
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/devtools/code-review-checklist, ru/git/revert-vs-reset
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/rust/arc-mutex-deadlock, ru/zam/architecture-overview, ru/windows/services-schtasks-robocopy
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/docker/named-volume-backup, ru/zam/architecture-overview, ru/ops/backups-restore-testing
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/db/sqlite-wal, ru/db/sqlite-vacuum, ru/zam/hybrid-search-rrf
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/powershell-encoding, ru/windows/git-autocrlf-gitattributes, ru/research/metriki-ir
- `e-14` (paraphrase, без общих слов): ожидалось en/zam/reranker-license; top-3: en/research/recall-vs-ndcg, en/research/onnx-runtime-execution-providers, en/devtools/gha-pull-request-target-secrets
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/windows/powershell-pipeline-objects, en/linux/disk-full-but-df-shows-space
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/path-setx-truncation, ru/devtools/local-build-cache
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/hnsw-parametry, ru/research/metriki-ir, ru/research/pamyat-agentov
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/zam/answer-cache-protocol, en/research/chunking-strategies, ru/zam/hybrid-search-rrf
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/devtools/playwright-flaky-e2e, en/zam/answer-cache-protocol, en/devtools/gha-pull-request-target-secrets
- `f-19` (cross-lang): ожидалось ru/research/kross-enkodery-rerank; top-3: en/zam/reranker-license, ru/zam/hybrid-search-rrf, en/research/colbert-late-interaction
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/research/hyde-gipoteticheskiy-dokument, ru/research/kvantovanie-int8, ru/research/dense-vs-sparse
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/docker/multistage-build-layer-cache, en/zam/answer-cache-protocol, en/rust/build-rs-rerun-if-changed
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/git/worktree, en/git/merge-vs-rebase, en/linux/systemd-timers-vs-cron
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/devtools/playwright-flaky-e2e, en/windows/editor-line-endings, en/git/interactive-rebase-autosquash
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/devtools/pyproject-editable-install, ru/ops/timeouts
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/local-build-cache, ru/db/sqlite-wal
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/docker/container-log-rotation, ru/zam/qdrant-managed-process, ru/zam/reindex-from-embeddings
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/docker/compose-healthcheck-depends-on, en/git/interactive-rebase-autosquash, en/zam/index-status-endpoint
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/docker/compose-healthcheck-depends-on, en/rust/future-not-send-mutexguard-await, en/devtools/playwright-flaky-e2e
