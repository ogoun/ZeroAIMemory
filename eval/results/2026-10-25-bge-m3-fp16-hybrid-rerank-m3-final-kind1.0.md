# Eval: hybrid+rerank, bge-m3-fp16

- дата 2026-10-25, коммит `67c074f`
- корпус: 182 заметок (отпечаток `3e0777bde1b82224`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- реранкер `bge-reranker-v2-m3`
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.0,"boost_gotcha":1.0,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.5,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 47.8 с
- время запроса (клиент): p50 87 мс, p95 92 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.909 [0.878–0.937] | 0.856 [0.822–0.889] | 0.869 [0.838–0.899] |
| broad | 25 | 0.855 [0.749–0.950] | 0.899 [0.798–0.980] | 0.869 [0.778–0.949] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.764 [0.621–0.893] | 0.824 [0.719–0.921] |
| chunk | 70 | 1.000 [1.000–1.000] | 0.955 [0.917–0.986] | 0.966 [0.938–0.989] |
| cross-lang | 38 | 0.974 [0.921–1.000] | 0.961 [0.895–1.000] | 0.962 [0.900–1.000] |
| exact | 43 | 0.977 [0.930–1.000] | 0.967 [0.914–1.000] | 0.975 [0.933–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.998 [0.994–1.000] |
| paraphrase | 61 | 0.604 [0.486–0.727] | 0.480 [0.370–0.588] | 0.516 [0.412–0.619] |
| superseded | 33 | 1.000 [1.000–1.000] | 0.904 [0.833–0.970] | 0.929 [0.876–0.978] |
| перефраз без общих слов | 46 | 0.475 [0.333–0.620] | 0.325 [0.213–0.447] | 0.369 [0.256–0.485] |
| длинные заметки | 70 | 1.000 [1.000–1.000] | 0.955 [0.917–0.986] | 0.966 [0.938–0.989] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| запрос en | 152 | 0.936 [0.895–0.972] | 0.889 [0.843–0.932] | 0.902 [0.860–0.940] |
| запрос ru | 174 | 0.885 [0.837–0.930] | 0.828 [0.778–0.876] | 0.841 [0.794–0.884] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 90 %, trigram 24 %, vector 96 %, vector_only 10 %.

Разрезано границами листов доказательств запросов `chunk`: 0 из 70 (из них целиком в листе вместе с перекрытием — 0); листов длинных заметок 215. Потолок chunk-recall@5 — 1.000.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 0.571 [0.286–0.786] |
| reliable_first без бустов | 14 | 0.571 [0.286–0.786] |
| chunk-recall@5 (свой текст листа) | 70 | 0.929 [0.857–0.986] |
| chunk-nDCG@10 | 70 | 0.841 [0.778–0.900] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.929 [0.857–0.986] |

## Промахи: ожидаемых заметок нет в top-10 (21)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/windows/execution-policy, ru/long/object-storage-backup-strategy
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: en/long/api-versioning-guide, ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/research/kross-enkodery-rerank, ru/long/dev-environment-onboarding
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/long/kafka-consumer-lag-analysis, ru/rust/arc-mutex-deadlock, ru/rust/ffi-cstring-repr-c
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/zam/windows-data-dir-path-limit, ru/db/postgres-zero-downtime-migrations, en/long/frontend-performance-audit
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/ops/postmortem-blameless
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/db/postgres-explain-analyze, en/rust/dyn-trait-object-safety
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/rust/cargo-feature-unification-resolver, en/devtools/gha-matrix-strategy
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/docker/multistage-build-layer-cache, en/rust/e0499-two-mutable-borrows
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/long/postgres-major-upgrade-runbook, ru/long/ci-pipeline-retrospective
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/postgres-major-upgrade-runbook, ru/long/kafka-consumer-lag-analysis, ru/ops/retries-backoff-jitter
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/db/sqlite-wal, ru/long/postgres-major-upgrade-runbook
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/long/incident-disk-full-logs, ru/windows/git-autocrlf-gitattributes, ru/long/dev-environment-onboarding
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, en/windows/powershell-pipeline-objects, en/linux/systemd-timers-vs-cron
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/path-setx-truncation, insight:c02-b
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/metriki-ir, ru/long/catalog-search-relevance-study, en/zam/answer-cache-protocol
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/metriki-ir, ru/research/hnsw-parametry, en/research/chunking-strategies
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/long/feature-flag-platform, en/docker/multistage-build-layer-cache, en/rust/build-rs-rerun-if-changed
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/git/worktree, en/git/merge-vs-rebase, ru/git/gitignore-tracked-file
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/docker/rootless-podman
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/dev-environment-onboarding, ru/git/hooks-commit-msg, ru/devtools/local-build-cache
