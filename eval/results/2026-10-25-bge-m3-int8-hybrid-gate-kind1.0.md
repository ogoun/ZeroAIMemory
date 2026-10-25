# Eval: hybrid, bge-m3-int8

- дата 2026-10-25, коммит `67c074f`
- корпус: 182 заметок (отпечаток `3e0777bde1b82224`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.0,"boost_gotcha":1.0,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.5,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 311.7 с
- время запроса (клиент): p50 53 мс, p95 69 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.847 [0.807–0.883] | 0.803 [0.764–0.840] | 0.824 [0.789–0.857] |
| broad | 25 | 0.932 [0.860–0.992] | 0.938 [0.844–1.000] | 0.913 [0.824–0.980] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.964 [0.893–1.000] | 0.974 [0.921–1.000] |
| chunk | 70 | 0.986 [0.957–1.000] | 0.954 [0.907–0.989] | 0.962 [0.922–0.992] |
| cross-lang | 38 | 0.882 [0.776–0.974] | 0.747 [0.632–0.854] | 0.796 [0.697–0.883] |
| exact | 43 | 1.000 [1.000–1.000] | 0.926 [0.868–0.977] | 0.945 [0.903–0.983] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] |
| paraphrase | 61 | 0.314 [0.208–0.434] | 0.313 [0.210–0.422] | 0.367 [0.270–0.465] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.867 [0.779–0.955] | 0.901 [0.835–0.966] |
| перефраз без общих слов | 46 | 0.112 [0.033–0.203] | 0.124 [0.059–0.202] | 0.186 [0.118–0.260] |
| длинные заметки | 70 | 0.986 [0.957–1.000] | 0.954 [0.907–0.989] | 0.962 [0.922–0.992] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.736 [0.583–0.875] | 0.805 [0.690–0.908] |
| запрос en | 152 | 0.864 [0.809–0.914] | 0.828 [0.772–0.879] | 0.848 [0.800–0.892] |
| запрос ru | 174 | 0.831 [0.779–0.885] | 0.780 [0.728–0.835] | 0.803 [0.756–0.852] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 93 %, trigram 26 %, vector 96 %, vector_only 7 %.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 1.000 [1.000–1.000] |
| chunk-recall@5 (свой текст листа) | 70 | 0.971 [0.929–1.000] |
| chunk-nDCG@10 | 70 | 0.888 [0.834–0.935] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.971 [0.929–1.000] |

## Промахи: ожидаемых заметок нет в top-10 (28)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, ru/rust/arc-mutex-deadlock
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/db/postgres-zero-downtime-migrations, ru/git/revert-vs-reset, ru/long/postgres-major-upgrade-runbook
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/long/kafka-consumer-lag-analysis
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/postgres-index-tuning-notes, en/long/dns-outage-postmortem, ru/db/postgres-zero-downtime-migrations
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/long/object-storage-backup-strategy, ru/long/incident-disk-full-logs, ru/long/postgres-major-upgrade-runbook
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log, en/rust/thiserror-vs-anyhow
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/rust/cargo-feature-unification-resolver, en/git/worktree
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/long/frontend-performance-audit, en/long/ml-model-serving-notes, en/long/postgres-index-tuning-notes
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy, ru/windows/services-schtasks-robocopy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/long/dns-outage-postmortem, en/windows/powershell-pipeline-objects, insight:p13-a
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/long/ci-pipeline-retrospective, ru/research/dense-vs-sparse, ru/long/catalog-search-relevance-study
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/long/catalog-search-relevance-study
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/long/frontend-performance-audit, en/long/observability-stack-design
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, ru/research/dense-vs-sparse
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/long/feature-flag-platform, en/docker/exit-code-137-container-oom
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-unit-restart, en/linux/systemd-timers-vs-cron, en/rust/e0499-two-mutable-borrows
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/git/interactive-rebase-autosquash, en/rust/thiserror-vs-anyhow, ru/git/blame-ignore-whitespace-moves
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/long/personal-data-retention-policy, ru/research/metriki-ir, ru/long/mobile-release-train
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/dev-environment-onboarding, ru/long/personal-data-retention-policy, ru/long/object-storage-backup-strategy
- `h-06` (paraphrase, без общих слов): ожидалось en/ops/load-testing-percentiles; top-3: en/db/postgres-explain-analyze, en/git/worktree, en/long/oncall-handbook
- `k-11` (chunk): ожидалось ru/long/catalog-search-relevance-study; top-3: ru/research/bm25-parametry, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
