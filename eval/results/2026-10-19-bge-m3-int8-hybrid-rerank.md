# Eval: hybrid+rerank, bge-m3-int8

- дата 2026-10-19, коммит `ff91374+dirty`
- корпус: 160 заметок (отпечаток `2ed461ea7979b814`), запросов 197
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- реранкер `jina-reranker-v2`
- индексация корпуса: 260.3 с
- время запроса (клиент): p50 1998 мс, p95 2266 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.788 [0.731–0.842] | 0.775 [0.717–0.828] | 0.778 [0.723–0.830] |
| broad | 25 | 0.892 [0.791–0.979] | 0.926 [0.837–1.000] | 0.901 [0.821–0.965] |
| cross-lang | 38 | 0.895 [0.789–0.974] | 0.895 [0.789–0.974] | 0.895 [0.789–0.974] |
| exact | 31 | 0.968 [0.903–1.000] | 0.972 [0.917–1.000] | 0.978 [0.935–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.988 [0.964–1.000] | 0.991 [0.974–1.000] |
| paraphrase | 61 | 0.443 [0.328–0.557] | 0.391 [0.273–0.510] | 0.407 [0.289–0.524] |
| перефраз без общих слов | 46 | 0.261 [0.130–0.391] | 0.192 [0.091–0.309] | 0.214 [0.114–0.333] |
| запрос en | 88 | 0.793 [0.705–0.870] | 0.788 [0.706–0.866] | 0.791 [0.708–0.866] |
| запрос ru | 109 | 0.784 [0.703–0.862] | 0.764 [0.684–0.839] | 0.768 [0.688–0.842] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 91 %, trigram 29 %, vector 100 %, vector_only 8 %.

## Промахи: ожидаемых заметок нет в top-10 (37)

- `a-01` (paraphrase, без общих слов): ожидалось ru/windows/execution-policy; top-3: ru/windows/powershell-encoding, ru/db/postgres-zero-downtime-migrations, ru/devtools/code-review-checklist
- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/windows/file-locked-running-exe, ru/git/hooks-commit-msg, ru/rust/arc-mutex-deadlock
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: en/zam/release-bundles, en/git/lfs, en/git/submodules-pitfalls
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/devtools/typescript-strict-flags, en/ops/http-cache-headers, en/research/chunking-strategies
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/zam/qdrant-managed-process
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank, ru/zam/markdown-chunking
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/ffi-cstring-repr-c, ru/linux/crontab-gotchas, ru/zam/qdrant-managed-process
- `b-18` (cross-lang): ожидалось en/git/bisect-run; top-3: ru/devtools/flaky-tests-quarantine, ru/git/cherry-pick-range, ru/git/revert-vs-reset
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/zam/windows-data-dir-path-limit, ru/db/postgres-zero-downtime-migrations, en/devtools/playwright-flaky-e2e
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/devtools/local-build-cache, ru/git/cherry-pick-range, ru/research/kvantovanie-int8
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless, ru/devtools/pyproject-editable-install
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/dyn-trait-object-safety, ru/rust/arc-mutex-deadlock, en/db/postgres-explain-analyze
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/windows/powershell-pipeline-objects, en/rust/cargo-feature-unification-resolver
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: ru/research/kross-enkodery-rerank, en/devtools/gha-matrix-strategy, en/devtools/gha-workflow-concurrency
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/zam/l0-l1-levels, ru/research/metriki-ir
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/zam/architecture-overview, ru/devtools/semver-and-changelog, ru/rust/arc-mutex-deadlock
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: en/git/worktree, en/windows/powershell-pipeline-objects, en/git/interactive-rebase-autosquash
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/zam/agent-token-claude-md, ru/windows/services-schtasks-robocopy, ru/windows/git-bash-paths-cygpath
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/research/hnsw-parametry, ru/zam/architecture-overview
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/powershell-encoding, ru/windows/git-autocrlf-gitattributes, ru/linux/ulimit-open-files-emfile
- `e-14` (paraphrase, без общих слов): ожидалось en/zam/reranker-license; top-3: en/research/recall-vs-ndcg, en/research/onnx-runtime-execution-providers, en/devtools/gha-matrix-strategy
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/devtools/npm-ci-clean-install, en/rust/musl-static-cross-compile
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/path-setx-truncation, ru/windows/services-schtasks-robocopy
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/zam/hybrid-search-rrf, ru/research/metriki-ir
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: en/research/chunking-strategies, ru/research/dense-vs-sparse, ru/zam/bge-m3-embeddings
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/zam/answer-cache-protocol, ru/zam/hybrid-search-rrf, en/research/chunking-strategies
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/devtools/playwright-flaky-e2e, en/ops/http-cache-headers, ru/research/metriki-ir
- `f-19` (cross-lang): ожидалось ru/research/kross-enkodery-rerank; top-3: ru/zam/hybrid-search-rrf, en/zam/reranker-license, en/research/colbert-late-interaction
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/research/hyde-gipoteticheskiy-dokument, ru/research/dense-vs-sparse
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/rust/build-rs-rerun-if-changed, en/ops/http-cache-headers
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/git/worktree, en/git/merge-vs-rebase
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/windows/editor-line-endings, en/devtools/npm-install-and-lockfile, en/git/interactive-rebase-autosquash
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/zam/architecture-overview
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/git/hooks-commit-msg, ru/git/clean-fdx, ru/db/sqlite-wal
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/zam/qdrant-managed-process, ru/docker/container-log-rotation, ru/zam/reindex-from-embeddings
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/docker/compose-healthcheck-depends-on, en/zam/index-status-endpoint, en/windows/wsl2-localhost-forwarding
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/rust/thiserror-vs-anyhow, en/devtools/playwright-flaky-e2e, en/rust/future-not-send-mutexguard-await
