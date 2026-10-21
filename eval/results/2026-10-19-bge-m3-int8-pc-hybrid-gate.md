# Eval: hybrid, bge-m3-int8-pc

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-int8-pc@bc866a6b`, устройство cpu (ml.device = cpu)
- индексация корпуса: 279.5 с
- время запроса (клиент): p50 60 мс, p95 124 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.741 [0.678–0.800] | 0.705 [0.644–0.765] | 0.719 [0.660–0.776] |
| broad | 25 | 0.919 [0.845–0.980] | 0.950 [0.870–1.000] | 0.926 [0.843–0.987] |
| cross-lang | 38 | 0.868 [0.763–0.974] | 0.711 [0.594–0.825] | 0.758 [0.650–0.856] |
| exact | 31 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.988 [0.964–1.000] | 0.991 [0.974–1.000] |
| paraphrase | 61 | 0.279 [0.180–0.393] | 0.255 [0.157–0.360] | 0.279 [0.183–0.384] |
| перефраз без общих слов | 46 | 0.043 [0.000–0.109] | 0.028 [0.010–0.051] | 0.057 [0.022–0.098] |
| запрос en | 88 | 0.759 [0.668–0.841] | 0.728 [0.639–0.813] | 0.738 [0.648–0.821] |
| запрос ru | 109 | 0.726 [0.639–0.807] | 0.686 [0.602–0.766] | 0.704 [0.624–0.782] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 97 %, trigram 34 %, vector 100 %, vector_only 3 %.

## Промахи: ожидаемых заметок нет в top-10 (43)

- `a-01` (paraphrase, без общих слов): ожидалось ru/windows/execution-policy; top-3: ru/db/postgres-zero-downtime-migrations, ru/db/sqlite-wal, ru/zam/proactive-recall-hooks
- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/git/hooks-commit-msg, ru/rust/arc-mutex-deadlock, ru/devtools/flaky-tests-quarantine
- `a-04` (paraphrase, без общих слов): ожидалось ru/windows/defender-exclusions-build; top-3: ru/research/dense-vs-sparse, ru/research/fts5-tokenizatory, ru/windows/file-locked-running-exe
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/devtools/npm-ci-clean-install, en/linux/ssh-config-agent-forwarding, en/git/worktree
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: en/zam/release-bundles, en/devtools/playwright-flaky-e2e, en/git/merge-vs-rebase
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/research/chunking-strategies, en/git/merge-vs-rebase, en/devtools/typescript-strict-flags
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/devtools/semver-and-changelog, ru/db/postgres-zero-downtime-migrations
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/zam/bge-m3-embeddings
- `b-18` (cross-lang): ожидалось en/git/bisect-run; top-3: ru/git/revert-vs-reset, ru/devtools/flaky-tests-quarantine, ru/git/cherry-pick-range
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: ru/db/postgres-zero-downtime-migrations, en/zam/windows-data-dir-path-limit, ru/db/sqlite-busy-timeout
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: en/docker/compose-healthcheck-depends-on, ru/zam/qdrant-managed-process, en/docker/volumes-vs-bind-mounts-uid
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/git/cherry-pick-range, ru/git/blame-ignore-whitespace-moves, ru/devtools/python-venv
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/devtools/pyproject-editable-install, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/thiserror-vs-anyhow, en/rust/dyn-trait-object-safety, en/db/postgres-explain-analyze
- `c-06` (paraphrase, без общих слов): ожидалось ru/docker/container-log-rotation; top-3: ru/zam/spaces-and-human-isolation, ru/zam/architecture-overview, ru/windows/long-paths-max-path
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/cargo-feature-unification-resolver, en/git/worktree, en/rust/e0499-two-mutable-borrows
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared, en/rust/dyn-trait-object-safety
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/research/recall-vs-ndcg, en/devtools/typescript-strict-flags, en/rust/future-not-send-mutexguard-await
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/db/sqlite-wal, ru/devtools/flaky-tests-quarantine
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/devtools/semver-and-changelog, ru/zam/architecture-overview, ru/research/kvantovanie-int8
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/db/sqlite-vacuum, ru/ops/backups-restore-testing, ru/db/sqlite-wal
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/devtools/code-review-checklist, ru/zam/architecture-overview
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, ru/windows/powershell-encoding, ru/research/dense-vs-sparse
- `e-14` (paraphrase, без общих слов): ожидалось en/zam/reranker-license; top-3: en/research/recall-vs-ndcg, en/devtools/gha-matrix-strategy, en/rust/thiserror-vs-anyhow
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/windows/powershell-native-stderr, en/windows/windows-terminal-profiles, en/windows/powershell-pipeline-objects
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/kross-enkodery-rerank, ru/research/metriki-ir, ru/research/rrf-sliyanie
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/hybrid-search-rrf, ru/zam/l0-l1-levels
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/windows/services-schtasks-robocopy
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, ru/zam/architecture-overview
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/research/learned-sparse-splade, ru/research/metriki-ir, en/git/submodules-pitfalls
- `f-19` (cross-lang): ожидалось ru/research/kross-enkodery-rerank; top-3: en/research/colbert-late-interaction, ru/zam/hybrid-search-rrf, en/zam/reranker-license
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/zam/bge-m3-embeddings, ru/research/fts5-tokenizatory
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/git/interactive-rebase-autosquash, en/docker/exit-code-137-container-oom
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-unit-restart, en/linux/systemd-timers-vs-cron, en/rust/e0499-two-mutable-borrows
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/git/interactive-rebase-autosquash, en/windows/editor-line-endings, en/rust/thiserror-vs-anyhow
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/ops/log-rotation, ru/ops/nginx-reverse-proxy-headers
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/code-review-checklist, ru/windows/wsl-mnt-c-performance
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/ops/retries-backoff-jitter, ru/devtools/semver-and-changelog, ru/db/sqlite-vacuum
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/devtools/gha-workflow-concurrency, en/docker/alpine-musl-dns
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: ru/db/postgres-zero-downtime-migrations, en/rust/future-not-send-mutexguard-await, en/devtools/playwright-flaky-e2e
- `h-06` (paraphrase, без общих слов): ожидалось en/ops/load-testing-percentiles; top-3: en/db/postgres-explain-analyze, en/git/worktree, en/git/stash-pitfalls
