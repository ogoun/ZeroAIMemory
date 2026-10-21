# Eval: hybrid, bge-m3-int8

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- индексация корпуса: 260.3 с
- время запроса (клиент): p50 64 мс, p95 107 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.736 [0.673–0.796] | 0.704 [0.644–0.765] | 0.718 [0.659–0.775] |
| broad | 25 | 0.919 [0.845–0.980] | 0.943 [0.857–1.000] | 0.921 [0.837–0.985] |
| cross-lang | 38 | 0.842 [0.711–0.947] | 0.711 [0.592–0.825] | 0.757 [0.648–0.858] |
| exact | 31 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.279 [0.180–0.393] | 0.248 [0.155–0.349] | 0.275 [0.180–0.378] |
| перефраз без общих слов | 46 | 0.043 [0.000–0.109] | 0.030 [0.010–0.056] | 0.058 [0.022–0.101] |
| запрос en | 88 | 0.759 [0.668–0.841] | 0.725 [0.638–0.811] | 0.735 [0.647–0.817] |
| запрос ru | 109 | 0.717 [0.630–0.798] | 0.687 [0.602–0.767] | 0.705 [0.623–0.781] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 97 %, trigram 34 %, vector 100 %, vector_only 3 %.

## Промахи: ожидаемых заметок нет в top-10 (43)

- `a-01` (paraphrase, без общих слов): ожидалось ru/windows/execution-policy; top-3: ru/db/postgres-zero-downtime-migrations, ru/devtools/code-review-checklist, ru/db/sqlite-wal
- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/ops/postmortem-blameless, ru/git/hooks-commit-msg, ru/rust/arc-mutex-deadlock
- `a-04` (paraphrase, без общих слов): ожидалось ru/windows/defender-exclusions-build; top-3: ru/research/fts5-tokenizatory, ru/windows/file-locked-running-exe, ru/research/rrf-sliyanie
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/devtools/npm-ci-clean-install, en/linux/ssh-config-agent-forwarding, en/research/chunking-strategies
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: en/devtools/playwright-flaky-e2e, en/zam/release-bundles, en/research/chunking-strategies
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/research/chunking-strategies, en/devtools/typescript-strict-flags, en/git/merge-vs-rebase
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/db/postgres-zero-downtime-migrations, ru/devtools/semver-and-changelog, ru/git/revert-vs-reset
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/research/kross-enkodery-rerank
- `b-18` (cross-lang): ожидалось en/git/bisect-run; top-3: ru/git/revert-vs-reset, ru/devtools/flaky-tests-quarantine, ru/git/cherry-pick-range
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/devtools/node-version-pinning, ru/db/postgres-zero-downtime-migrations, en/zam/windows-data-dir-path-limit
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: ru/zam/qdrant-managed-process, en/docker/volumes-vs-bind-mounts-uid, en/docker/compose-healthcheck-depends-on
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/git/cherry-pick-range, ru/git/blame-ignore-whitespace-moves, ru/devtools/python-venv
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head, ru/devtools/pyproject-editable-install
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/thiserror-vs-anyhow, en/rust/dyn-trait-object-safety, en/db/postgres-explain-analyze
- `c-06` (paraphrase, без общих слов): ожидалось ru/docker/container-log-rotation; top-3: ru/zam/spaces-and-human-isolation, ru/windows/long-paths-max-path, ru/zam/architecture-overview
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/cargo-feature-unification-resolver, en/git/worktree, en/rust/e0499-two-mutable-borrows
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared, en/rust/dyn-trait-object-safety
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/research/recall-vs-ndcg, en/rust/future-not-send-mutexguard-await, en/docker/exit-code-137-container-oom
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/db/sqlite-wal, ru/zam/architecture-overview
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/windows/services-schtasks-robocopy, ru/devtools/semver-and-changelog, ru/research/kvantovanie-int8
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/ops/backups-restore-testing, ru/db/sqlite-vacuum, ru/db/sqlite-wal
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/devtools/code-review-checklist, ru/db/sqlite-vacuum
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, ru/zam/hybrid-search-rrf, ru/windows/powershell-encoding
- `e-14` (paraphrase, без общих слов): ожидалось en/zam/reranker-license; top-3: en/research/recall-vs-ndcg, en/research/onnx-runtime-execution-providers, en/devtools/gha-matrix-strategy
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/windows/windows-terminal-profiles, en/windows/powershell-pipeline-objects, en/windows/powershell-native-stderr
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/kross-enkodery-rerank, ru/research/metriki-ir, ru/research/hnsw-parametry
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/l0-l1-levels, ru/zam/hybrid-search-rrf
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/windows/services-schtasks-robocopy
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/devtools/playwright-flaky-e2e
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/research/learned-sparse-splade, ru/research/metriki-ir, en/devtools/playwright-flaky-e2e
- `f-19` (cross-lang): ожидалось ru/research/kross-enkodery-rerank; top-3: en/research/colbert-late-interaction, ru/zam/hybrid-search-rrf, en/zam/reranker-license
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/zam/bge-m3-embeddings, ru/research/fts5-tokenizatory
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/docker/exit-code-137-container-oom, en/git/bisect-run
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-unit-restart, en/rust/e0499-two-mutable-borrows, en/linux/systemd-timers-vs-cron
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/git/interactive-rebase-autosquash, en/windows/editor-line-endings, en/rust/thiserror-vs-anyhow
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/zam/qdrant-managed-process, ru/ops/log-rotation, ru/research/metriki-ir
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/code-review-checklist, ru/git/clean-fdx
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/ops/retries-backoff-jitter, ru/devtools/semver-and-changelog, ru/docker/container-log-rotation
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/docker/alpine-musl-dns, en/windows/wsl2-localhost-forwarding
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: ru/db/postgres-zero-downtime-migrations, en/rust/future-not-send-mutexguard-await, en/devtools/playwright-flaky-e2e
- `h-06` (paraphrase, без общих слов): ожидалось en/ops/load-testing-percentiles; top-3: en/db/postgres-explain-analyze, en/git/worktree, en/git/stash-pitfalls
