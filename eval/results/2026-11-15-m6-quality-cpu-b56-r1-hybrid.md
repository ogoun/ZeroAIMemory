# Eval: hybrid, bge-m3-int8

- дата 2026-11-14, коммит `238e7e8`
- корпус: 212 заметок (отпечаток `34aafe1af1e9c1ca`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- действующие `[search]` сервера: `{"attach_prefetch":30,"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.05,"boost_gotcha":1.05,"boost_hypothesis":1.0,"boost_theorem":1.1,"contradicts_order":true,"graph_weight":0.5,"max_folders":2,"max_per_attachment":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_alias":0.5,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.0,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 443.8 с
- время запроса (клиент): p50 59 мс, p95 77 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.846 [0.807–0.884] | 0.793 [0.752–0.831] | 0.815 [0.778–0.851] |
| broad | 25 | 0.919 [0.847–0.980] | 0.896 [0.788–0.980] | 0.888 [0.797–0.959] |
| certainty | 14 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| chunk | 70 | 0.986 [0.957–1.000] | 0.949 [0.905–0.986] | 0.962 [0.928–0.989] |
| cross-lang | 38 | 0.855 [0.737–0.947] | 0.711 [0.592–0.823] | 0.768 [0.671–0.860] |
| exact | 43 | 1.000 [1.000–1.000] | 0.926 [0.860–0.977] | 0.945 [0.896–0.983] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.998 [0.994–1.000] |
| paraphrase | 61 | 0.331 [0.221–0.451] | 0.288 [0.191–0.394] | 0.334 [0.239–0.432] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.883 [0.797–0.955] | 0.913 [0.848–0.966] |
| перефраз без общих слов | 46 | 0.112 [0.033–0.203] | 0.104 [0.048–0.170] | 0.153 [0.092–0.215] |
| длинные заметки | 70 | 0.986 [0.957–1.000] | 0.949 [0.905–0.986] | 0.962 [0.928–0.989] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.792 [0.667–0.917] | 0.846 [0.754–0.938] |
| запрос en | 152 | 0.862 [0.808–0.913] | 0.826 [0.771–0.876] | 0.845 [0.792–0.890] |
| запрос ru | 174 | 0.831 [0.778–0.883] | 0.764 [0.711–0.819] | 0.790 [0.741–0.841] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 93 %, trigram 26 %, vector 96 %, vector_only 7 %.

Разрезано границами листов доказательств запросов `chunk`: 2 из 70 (из них целиком в листе вместе с перекрытием — 2); листов длинных заметок 199. Потолок chunk-recall@5 — 0.971.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 1.000 [1.000–1.000] |
| reliable_first без бустов | 14 | 0.714 [0.429–0.929] |
| chunk-recall@5 (свой текст листа) | 70 | 0.957 [0.900–1.000] |
| chunk-nDCG@10 | 70 | 0.874 [0.810–0.929] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.986 [0.957–1.000] |

## Промахи: ожидаемых заметок нет в top-10 (30)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, ru/rust/arc-mutex-deadlock
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/interactive-rebase-autosquash, en/git/merge-vs-rebase
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/long/postgres-major-upgrade-runbook
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/long/dev-environment-onboarding
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, insight:k13-t, ru/research/kvantovanie-int8
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, en/api/health-endpoint-design, ru/db/postgres-zero-downtime-migrations
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/long/incident-disk-full-logs, ru/long/postgres-major-upgrade-runbook, ru/long/object-storage-backup-strategy
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: insight:p20-t, en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log
- `c-06` (paraphrase, без общих слов): ожидалось ru/docker/container-log-rotation; top-3: ru/long/personal-data-retention-policy, insight:c07-b, ru/long/incident-disk-full-logs
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: insight:k03-t, en/long/ml-model-serving-notes, en/long/frontend-performance-audit
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: insight:p18-t, insight:k17-t, en/web/infinite-scroll
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/long/personal-data-retention-policy, ru/long/object-storage-backup-strategy, ru/windows/services-schtasks-robocopy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: insight:k02-t, en/web/fetch-retry-abort, en/long/dns-outage-postmortem
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/dense-vs-sparse, ru/long/catalog-search-relevance-study, ru/long/ci-pipeline-retrospective
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/long/kafka-consumer-lag-analysis
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, ru/research/hyde-gipoteticheskiy-dokument
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/long/feature-flag-platform, insight:k04-n
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: insight:k14-t, en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/git/interactive-rebase-autosquash, en/rust/thiserror-vs-anyhow, ru/git/blame-ignore-whitespace-moves
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/long/mobile-release-train, ru/long/personal-data-retention-policy, ru/research/metriki-ir
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/long/dev-environment-onboarding, ru/long/personal-data-retention-policy
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/long/dns-outage-postmortem, en/long/payment-webhooks-reliability
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: ru/web/service-worker-cache, insight:k17-t, en/windows/wsl2-localhost-forwarding
