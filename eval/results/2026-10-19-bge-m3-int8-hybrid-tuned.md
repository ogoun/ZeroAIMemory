# Eval: hybrid, bge-m3-int8

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- индексация корпуса: 241.9 с
- время запроса (клиент): p50 52 мс, p95 84 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.755 [0.693–0.814] | 0.719 [0.660–0.776] | 0.748 [0.694–0.800] |
| broad | 25 | 0.912 [0.804–0.992] | 0.945 [0.855–1.000] | 0.923 [0.836–0.986] |
| cross-lang | 38 | 0.868 [0.763–0.974] | 0.748 [0.638–0.848] | 0.808 [0.724–0.885] |
| exact | 31 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.988 [0.964–1.000] | 0.991 [0.974–1.000] |
| paraphrase | 61 | 0.328 [0.213–0.443] | 0.279 [0.186–0.377] | 0.342 [0.250–0.439] |
| перефраз без общих слов | 46 | 0.130 [0.043–0.239] | 0.074 [0.045–0.108] | 0.150 [0.096–0.208] |
| запрос en | 88 | 0.759 [0.668–0.841] | 0.742 [0.657–0.824] | 0.769 [0.692–0.842] |
| запрос ru | 109 | 0.752 [0.670–0.830] | 0.700 [0.620–0.777] | 0.730 [0.657–0.801] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 89 %, trigram 31 %, vector 100 %, vector_only 11 %.

## Промахи: ожидаемых заметок нет в top-10 (27)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/ops/postmortem-blameless, ru/rust/arc-mutex-deadlock, ru/git/hooks-commit-msg
- `a-04` (paraphrase, без общих слов): ожидалось ru/windows/defender-exclusions-build; top-3: ru/research/fts5-tokenizatory, ru/zam/qdrant-managed-process, ru/windows/git-autocrlf-gitattributes
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/research/chunking-strategies, en/devtools/typescript-strict-flags, en/ops/http-cache-headers
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/devtools/semver-and-changelog
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/zam/markdown-chunking
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/research/metriki-ir
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/devtools/node-version-pinning, ru/db/sqlite-busy-timeout, en/db/connection-pool-exhaustion
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/git/cherry-pick-range, ru/git/blame-ignore-whitespace-moves, ru/devtools/python-venv
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head, ru/zam/markdown-chunking
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/thiserror-vs-anyhow, en/rust/dyn-trait-object-safety, en/rust/future-not-send-mutexguard-await
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0499-two-mutable-borrows, en/rust/e0597-does-not-live-long-enough, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/research/recall-vs-ndcg, en/docker/exit-code-137-container-oom, en/rust/future-not-send-mutexguard-await
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/db/sqlite-wal, ru/devtools/code-review-checklist
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/research/kvantovanie-int8, ru/research/dense-vs-sparse, ru/zam/architecture-overview
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/db/sqlite-wal, ru/ops/backups-restore-testing, ru/zam/architecture-overview
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum, ru/devtools/code-review-checklist
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, ru/research/dense-vs-sparse, ru/zam/hybrid-search-rrf
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/research/pamyat-agentov
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, ru/zam/architecture-overview
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/docker/exit-code-137-container-oom, en/rust/musl-static-cross-compile
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-unit-restart, en/linux/systemd-timers-vs-cron, en/rust/e0499-two-mutable-borrows
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/windows/services-schtasks-robocopy
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/code-review-checklist, ru/windows/git-autocrlf-gitattributes
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/ops/retries-backoff-jitter, ru/devtools/semver-and-changelog, ru/docker/container-log-rotation
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/docker/alpine-musl-dns, en/zam/index-status-endpoint
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/rust/future-not-send-mutexguard-await, en/devtools/playwright-flaky-e2e, ru/db/postgres-zero-downtime-migrations
