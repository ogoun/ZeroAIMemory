# Eval: hybrid, bge-m3-fp16

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- индексация корпуса: 30.8 с
- время запроса (клиент): p50 26 мс, p95 32 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.765 [0.706–0.823] | 0.721 [0.663–0.779] | 0.750 [0.697–0.801] |
| broad | 25 | 0.912 [0.804–0.992] | 0.945 [0.855–1.000] | 0.914 [0.824–0.983] |
| cross-lang | 38 | 0.868 [0.763–0.974] | 0.758 [0.646–0.859] | 0.815 [0.729–0.895] |
| exact | 31 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.988 [0.964–1.000] | 0.991 [0.974–1.000] |
| paraphrase | 61 | 0.361 [0.246–0.475] | 0.282 [0.192–0.381] | 0.348 [0.258–0.442] |
| перефраз без общих слов | 46 | 0.174 [0.065–0.283] | 0.078 [0.047–0.111] | 0.158 [0.104–0.212] |
| запрос en | 88 | 0.782 [0.693–0.864] | 0.740 [0.654–0.822] | 0.766 [0.688–0.841] |
| запрос ru | 109 | 0.752 [0.670–0.830] | 0.706 [0.627–0.782] | 0.737 [0.666–0.807] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 88 %, trigram 31 %, vector 100 %, vector_only 12 %.

## Промахи: ожидаемых заметок нет в top-10 (26)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/rust/arc-mutex-deadlock, ru/git/hooks-commit-msg, ru/git/revert-vs-reset
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/devtools/npm-ci-clean-install, en/linux/ssh-config-agent-forwarding, en/git/worktree
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/devtools/semver-and-changelog, ru/db/postgres-zero-downtime-migrations
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/zam/markdown-chunking
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/research/rrf-sliyanie
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: ru/db/sqlite-busy-timeout, en/db/connection-pool-exhaustion, ru/db/postgres-zero-downtime-migrations
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: en/docker/compose-healthcheck-depends-on, en/docker/volumes-vs-bind-mounts-uid, ru/zam/qdrant-managed-process
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head, ru/devtools/pyproject-editable-install
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/thiserror-vs-anyhow, en/rust/dyn-trait-object-safety, en/rust/e0502-mutable-borrow-while-shared
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/git/worktree, en/rust/e0499-two-mutable-borrows, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0499-two-mutable-borrows, en/rust/e0597-does-not-live-long-enough, en/rust/e0502-mutable-borrow-while-shared
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/db/sqlite-wal, ru/devtools/code-review-checklist
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/research/kvantovanie-int8, ru/research/dense-vs-sparse, ru/windows/services-schtasks-robocopy
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/ops/backups-restore-testing, ru/db/sqlite-wal, ru/zam/architecture-overview
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum, ru/zam/architecture-overview
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, ru/research/dense-vs-sparse, ru/docker/container-log-rotation
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/windows/powershell-native-stderr, en/linux/systemd-unit-restart, en/devtools/npm-ci-clean-install
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/research/metriki-ir, ru/research/kross-enkodery-rerank
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, ru/zam/architecture-overview
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/rust/musl-static-cross-compile, en/docker/exit-code-137-container-oom
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart, en/git/worktree
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/devtools/pyproject-editable-install, ru/devtools/semver-and-changelog
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/devtools/code-review-checklist, ru/windows/git-autocrlf-gitattributes
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/devtools/semver-and-changelog, ru/ops/retries-backoff-jitter, ru/docker/container-log-rotation
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/docker/alpine-musl-dns, en/git/reflog-recovery
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/rust/future-not-send-mutexguard-await, en/devtools/playwright-flaky-e2e, ru/db/postgres-zero-downtime-migrations
