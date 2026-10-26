# Eval: fts, без моделей (FTS-only)

- дата 2026-10-25, коммит `67c074f`
- корпус: 182 заметок (отпечаток `3e0777bde1b82224`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.0,"boost_gotcha":1.0,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.5,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- время запроса (клиент): p50 6 мс, p95 9 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.819 [0.777–0.858] | 0.779 [0.736–0.819] | 0.782 [0.740–0.821] |
| broad | 25 | 0.804 [0.691–0.907] | 0.913 [0.800–1.000] | 0.817 [0.710–0.907] |
| certainty | 14 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| chunk | 70 | 1.000 [1.000–1.000] | 0.935 [0.884–0.979] | 0.951 [0.913–0.984] |
| cross-lang | 38 | 0.829 [0.697–0.934] | 0.645 [0.526–0.759] | 0.691 [0.579–0.800] |
| exact | 43 | 0.977 [0.942–1.000] | 1.000 [1.000–1.000] | 0.982 [0.955–1.000] |
| identifier | 42 | 0.988 [0.964–1.000] | 1.000 [1.000–1.000] | 0.993 [0.982–1.000] |
| paraphrase | 61 | 0.260 [0.153–0.369] | 0.240 [0.141–0.345] | 0.244 [0.146–0.348] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.833 [0.742–0.924] | 0.869 [0.786–0.944] |
| перефраз без общих слов | 46 | 0.018 [0.000–0.047] | 0.031 [0.000–0.082] | 0.027 [0.000–0.061] |
| длинные заметки | 70 | 1.000 [1.000–1.000] | 0.935 [0.884–0.979] | 0.951 [0.913–0.984] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| запрос en | 152 | 0.832 [0.774–0.886] | 0.811 [0.754–0.867] | 0.811 [0.756–0.865] |
| запрос ru | 174 | 0.807 [0.750–0.864] | 0.751 [0.694–0.810] | 0.755 [0.700–0.812] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 100 %, trigram 29 %.

Разрезано границами листов доказательств запросов `chunk`: 1 из 70 (из них целиком в листе вместе с перекрытием — 1); листов длинных заметок 252. Потолок chunk-recall@5 — 0.986.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 1.000 [1.000–1.000] |
| reliable_first без бустов | 14 | 0.571 [0.286–0.786] |
| chunk-recall@5 (свой текст листа) | 70 | 0.914 [0.843–0.971] |
| chunk-nDCG@10 | 70 | 0.861 [0.788–0.922] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.929 [0.857–0.971] |

## Промахи: ожидаемых заметок нет в top-10 (51)

- `a-01` (paraphrase, без общих слов): ожидалось ru/windows/execution-policy; top-3: ru/db/postgres-zero-downtime-migrations, ru/devtools/code-review-checklist, ru/db/sqlite-wal
- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/windows/git-autocrlf-gitattributes, ru/long/object-storage-backup-strategy
- `a-03` (paraphrase, без общих слов): ожидалось ru/windows/cmd-chcp-65001; top-3: ru/windows/powershell-encoding, ru/research/fts5-tokenizatory, ru/docker/rootless-podman
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log, en/devtools/npm-ci-clean-install
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/zam/release-bundles, en/git/submodules-pitfalls
- `b-03` (paraphrase, без общих слов): ожидалось en/git/lfs; top-3: en/devtools/typescript-strict-flags, en/long/postgres-index-tuning-notes, en/long/frontend-performance-audit
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/long/mobile-release-train, ru/zam/qdrant-managed-process, ru/long/kafka-consumer-lag-analysis
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/uv-package-manager, ru/zam/qdrant-managed-process, ru/long/tls-certificate-rotation
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/long/object-storage-backup-strategy, ru/long/kafka-consumer-lag-analysis, ru/zam/hybrid-search-rrf
- `b-18` (cross-lang): ожидалось en/git/bisect-run; top-3: ru/devtools/semver-and-changelog, ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/zam/windows-data-dir-path-limit, en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: en/research/onnx-runtime-execution-providers, en/docker/compose-healthcheck-depends-on, en/devtools/gha-cache-keys
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/research/dense-vs-sparse, ru/zam/l0-l1-levels, ru/docker/rootless-podman
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/devtools/pyproject-editable-install, ru/ops/nginx-reverse-proxy-headers, ru/long/mobile-release-train
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/db/postgres-explain-analyze, en/rust/e0499-two-mutable-borrows
- `c-06` (paraphrase, без общих слов): ожидалось ru/docker/container-log-rotation; top-3: insight:p14-a, ru/long/personal-data-retention-policy, insight:c07-b
- `c-20` (cross-lang): ожидалось en/docker/multistage-build-layer-cache; top-3: ru/docker/dockerignore-build-context, ru/long/ci-pipeline-retrospective, ru/zam/hybrid-search-rrf
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/windows/powershell-pipeline-objects, en/long/redis-cluster-migration, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/research/learned-sparse-splade, en/long/redis-cluster-migration, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: ru/db/postgres-zero-downtime-migrations, en/long/postgres-index-tuning-notes, en/devtools/typescript-strict-flags
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/long/ci-pipeline-retrospective, ru/long/checkout-load-testing-report
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/postgres-major-upgrade-runbook, ru/long/kafka-consumer-lag-analysis, ru/rust/arc-mutex-deadlock
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: en/git/interactive-rebase-autosquash, en/windows/powershell-pipeline-objects, en/docker/compose-healthcheck-depends-on
- `d-20` (cross-lang): ожидалось en/rust/workspace-dependencies-and-profiles; top-3: insight:p12-a, ru/windows/powershell-encoding, ru/devtools/code-review-checklist
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/long/personal-data-retention-policy, ru/git/revert-vs-reset, ru/long/object-storage-backup-strategy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/devtools/code-review-checklist, ru/long/object-storage-backup-strategy, ru/rust/string-str-cow
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/long/tls-certificate-rotation, insight:p10-a, ru/research/metriki-ir
- `e-14` (paraphrase, без общих слов): ожидалось en/zam/reranker-license; top-3: en/long/ml-model-serving-notes, en/windows/wsl2-localhost-forwarding, en/long/dns-outage-postmortem
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/windows/powershell-native-stderr, en/windows/windows-terminal-profiles, en/windows/powershell-pipeline-objects
- `e-16` (paraphrase, без общих слов): ожидалось ru/zam/spaces-and-human-isolation; top-3: ru/research/hnsw-parametry, ru/long/ci-pipeline-retrospective, ru/docker/rootless-podman
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/long/dev-environment-onboarding
- `e-23` (broad): ожидалось ru/zam/degraded-search-mode, ru/zam/reindex-from-embeddings; top-3: ru/zam/hybrid-search-rrf, ru/zam/architecture-overview, ru/zam/spaces-and-human-isolation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/long/incident-disk-full-logs, ru/ops/nginx-reverse-proxy-headers, ru/zam/hybrid-search-rrf
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: insight:p14-a, ru/research/rrf-sliyanie, insight:p10-h
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/fts5-tokenizatory, insight:c05-c, ru/windows/services-schtasks-robocopy
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/long/redis-cluster-migration, en/long/postgres-index-tuning-notes, en/research/chunking-strategies
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/research/learned-sparse-splade, en/ops/http-cache-headers, en/rust/serde-attributes-rename-flatten
- `f-07` (paraphrase, без общих слов): ожидалось ru/research/pamyat-agentov; top-3: ru/long/checkout-load-testing-report, ru/long/dev-environment-onboarding, ru/long/personal-data-retention-policy
- `f-19` (cross-lang): ожидалось ru/research/kross-enkodery-rerank; top-3: en/long/ml-model-serving-notes, ru/zam/hybrid-search-rrf, en/research/colbert-late-interaction
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/research/fts5-tokenizatory, insight:p05-h
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/docker/exit-code-137-container-oom, en/long/feature-flag-platform, en/git/bisect-run
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/long/frontend-performance-audit, en/rust/musl-static-cross-compile, en/zam/release-bundles
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/windows/editor-line-endings, en/research/learned-sparse-splade, en/git/interactive-rebase-autosquash
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/long/mobile-release-train, ru/long/tls-certificate-rotation, ru/devtools/code-review-checklist
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/dev-environment-onboarding, ru/ops/tls-incomplete-chain, ru/git/clean-fdx
- `h-01` (paraphrase, без общих слов): ожидалось ru/ops/retries-backoff-jitter; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless
- `h-03` (paraphrase, без общих слов): ожидалось ru/ops/postmortem-blameless; top-3: ru/zam/qdrant-managed-process, ru/long/tls-certificate-rotation, ru/ops/backups-restore-testing
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/long/dns-outage-postmortem, en/long/oncall-handbook
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/windows/wsl2-localhost-forwarding, en/long/payment-webhooks-reliability, en/linux/oom-killer-dmesg
- `h-06` (paraphrase, без общих слов): ожидалось en/ops/load-testing-percentiles; top-3: en/db/postgres-explain-analyze, en/git/bisect-run, en/devtools/eslint-prettier-conflict
- `s-26` (superseded): ожидалось insight:c13-b; top-3: ru/long/object-storage-backup-strategy, ru/ops/tls-expired-certificate, ru/zam/qdrant-managed-process
