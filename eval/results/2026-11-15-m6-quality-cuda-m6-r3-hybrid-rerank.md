# Eval: hybrid+rerank, bge-m3-fp16

- дата 2026-11-15, коммит `ac0a32d`
- корпус: 212 заметок (отпечаток `34aafe1af1e9c1ca`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- реранкер `bge-reranker-v2-m3`
- действующие `[search]` сервера: `{"attach_prefetch":30,"boost_area":1.0,"boost_axiom":1.2,"boost_failure":1.05,"boost_gotcha":1.05,"boost_hypothesis":1.0,"boost_theorem":1.1,"brief_min_rel":0.0,"budget_tokens":800,"contradicts_order":true,"graph_weight":0.5,"kind_boost_mode":"always","max_folders":2,"max_per_attachment":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_attach":1.0,"w_folder":0.0,"w_fts":1.0,"w_fts_alias":0.5,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.0,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 0.2 с
- время запроса (клиент): p50 201 мс, p95 215 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.905 [0.874–0.934] | 0.860 [0.827–0.891] | 0.872 [0.840–0.900] |
| broad | 25 | 0.835 [0.725–0.930] | 0.880 [0.780–0.967] | 0.852 [0.759–0.931] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.964 [0.893–1.000] | 0.974 [0.921–1.000] |
| chunk | 70 | 1.000 [1.000–1.000] | 0.964 [0.936–0.993] | 0.974 [0.953–0.995] |
| cross-lang | 38 | 0.974 [0.921–1.000] | 0.947 [0.882–1.000] | 0.952 [0.890–0.998] |
| exact | 43 | 0.977 [0.930–1.000] | 0.953 [0.895–1.000] | 0.960 [0.898–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.998 [0.994–1.000] |
| paraphrase | 61 | 0.593 [0.475–0.716] | 0.469 [0.360–0.578] | 0.509 [0.403–0.610] |
| superseded | 33 | 1.000 [1.000–1.000] | 0.904 [0.833–0.970] | 0.929 [0.876–0.978] |
| перефраз без общих слов | 46 | 0.460 [0.319–0.609] | 0.311 [0.203–0.428] | 0.359 [0.249–0.473] |
| длинные заметки | 70 | 1.000 [1.000–1.000] | 0.964 [0.936–0.993] | 0.974 [0.953–0.995] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| запрос en | 152 | 0.936 [0.895–0.972] | 0.887 [0.839–0.930] | 0.898 [0.854–0.936] |
| запрос ru | 174 | 0.878 [0.830–0.924] | 0.837 [0.787–0.885] | 0.848 [0.801–0.892] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 89 %, trigram 24 %, vector 96 %, vector_only 11 %.

Разрезано границами листов доказательств запросов `chunk`: 2 из 70 (из них целиком в листе вместе с перекрытием — 2); листов длинных заметок 199. Потолок chunk-recall@5 — 0.971.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 0.929 [0.786–1.000] |
| reliable_first без бустов | 14 | 0.857 [0.643–1.000] |
| chunk-recall@5 (свой текст листа) | 70 | 0.914 [0.843–0.971] |
| chunk-nDCG@10 | 70 | 0.811 [0.737–0.875] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.943 [0.886–0.986] |

## Сравнение с прежним прогоном

`.tmp/m6/task13b/quality/cuda/b56-r3/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (запросы `49a3b34b15e9b432`, корпус `34aafe1af1e9c1ca`): общих запросов 326.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 326 | -0.003 [-0.009…+0.000] |
| Δ nDCG@10 | 326 | -0.003 [-0.009…+0.000] |
| Δ nDCG@10 без группы e | 301 | +0.000 [-0.000…+0.000] |
| Δ nDCG@10 без новых целей корпуса | 326 | -0.003 [-0.009…+0.000] |

Общий запрос — тот же id, вид, `expect` (текст прежнего прогона не найден — не сверялся); метрики нового прогона — по разметке прежнего (`expect`; с `also`, если прежний прогон его знает — тогда и `also` тот же). «Без новых целей» — из выдачи убраны цели, которых не было в корпусе прежнего коммита (заметки его `eval/corpus` и insight его `eval/insights.toml` остаются).

Интервал Δ nDCG@10 не лежит целиком ниже нуля — регрессии нет.

## Промахи: ожидаемых заметок нет в top-10 (23)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/windows/execution-policy, ru/rust/arc-mutex-deadlock
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: en/long/api-versioning-guide, ru/git/revert-vs-reset, ru/long/kafka-consumer-lag-analysis
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/research/kross-enkodery-rerank, ru/long/dev-environment-onboarding
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/long/kafka-consumer-lag-analysis, insight:k13-t, ru/ops/backups-restore-testing
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/zam/windows-data-dir-path-limit, ru/db/postgres-zero-downtime-migrations, en/api/database-statement-timeout
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, en/db/connection-pool-exhaustion
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, insight:p20-t, en/db/postgres-explain-analyze
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/rust/cargo-feature-unification-resolver, en/research/multilingual-embeddings
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/docker/multistage-build-layer-cache, en/rust/e0499-two-mutable-borrows
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/rust/future-not-send-mutexguard-await, insight:k03-t, en/api/idempotency-keys
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/long/postgres-major-upgrade-runbook
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/postgres-major-upgrade-runbook, ru/long/kafka-consumer-lag-analysis, ru/ops/retries-backoff-jitter
- `e-07` (exact): ожидалось ru/zam/markdown-chunking; top-3: ru/zam/architecture-overview, ru/zam/hybrid-search-rrf, en/db/sqlite-fts5-search-index
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy, ru/db/sqlite-wal
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/long/incident-disk-full-logs, ru/windows/git-autocrlf-gitattributes, ru/long/dev-environment-onboarding
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/services-schtasks-robocopy, insight:k02-t, en/windows/powershell-pipeline-objects
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/path-setx-truncation, insight:c02-b
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/metriki-ir, ru/long/catalog-search-relevance-study, en/zam/answer-cache-protocol
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/metriki-ir, ru/research/hnsw-parametry, en/research/chunking-strategies
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/long/feature-flag-platform, en/docker/multistage-build-layer-cache, en/rust/build-rs-rerun-if-changed
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/git/worktree, en/git/merge-vs-rebase, ru/git/gitignore-tracked-file
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/devtools/pyproject-editable-install, ru/research/metriki-ir, ru/docker/rootless-podman
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/git/hooks-commit-msg, ru/long/dev-environment-onboarding
