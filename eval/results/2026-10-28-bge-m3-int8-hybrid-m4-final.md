# Eval: hybrid, bge-m3-int8

- дата 2026-10-28, коммит `86c8106+dirty.5d0a7ff1`
- корпус: 182 заметок (отпечаток `3e0777bde1b82224`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-int8@a206e10e`, устройство cpu (ml.device = cpu)
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.05,"boost_gotcha":1.05,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.0,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 388.5 с
- время запроса (клиент): p50 63 мс, p95 83 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.857 [0.819–0.893] | 0.805 [0.767–0.842] | 0.826 [0.791–0.859] |
| broad | 25 | 0.912 [0.804–0.992] | 0.935 [0.836–1.000] | 0.912 [0.820–0.981] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.964 [0.893–1.000] | 0.974 [0.921–1.000] |
| chunk | 70 | 0.986 [0.957–1.000] | 0.955 [0.911–0.989] | 0.966 [0.933–0.992] |
| cross-lang | 38 | 0.882 [0.776–0.974] | 0.748 [0.634–0.854] | 0.797 [0.698–0.883] |
| exact | 43 | 1.000 [1.000–1.000] | 0.926 [0.868–0.977] | 0.945 [0.903–0.983] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] |
| paraphrase | 61 | 0.380 [0.262–0.500] | 0.317 [0.213–0.427] | 0.364 [0.269–0.465] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.882 [0.795–0.955] | 0.912 [0.846–0.966] |
| перефраз без общих слов | 46 | 0.178 [0.080–0.290] | 0.128 [0.062–0.207] | 0.182 [0.116–0.256] |
| длинные заметки | 70 | 0.986 [0.957–1.000] | 0.955 [0.911–0.989] | 0.966 [0.933–0.992] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.736 [0.583–0.875] | 0.805 [0.690–0.908] |
| запрос en | 152 | 0.870 [0.818–0.920] | 0.828 [0.773–0.879] | 0.847 [0.797–0.892] |
| запрос ru | 174 | 0.846 [0.793–0.897] | 0.785 [0.733–0.838] | 0.807 [0.760–0.856] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 93 %, trigram 26 %, vector 96 %, vector_only 7 %.

Разрезано границами листов доказательств запросов `chunk`: 2 из 70 (из них целиком в листе вместе с перекрытием — 2); листов длинных заметок 196. Потолок chunk-recall@5 — 0.971.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 1.000 [1.000–1.000] |
| reliable_first без бустов | 14 | 0.429 [0.143–0.714] |
| chunk-recall@5 (свой текст листа) | 70 | 0.957 [0.900–1.000] |
| chunk-nDCG@10 | 70 | 0.885 [0.824–0.938] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.986 [0.957–1.000] |

## Сравнение с прежним прогоном

`eval/results/2026-10-25-bge-m3-int8-hybrid-m3-final.json` (запросы `49a3b34b15e9b432`, корпус `3e0777bde1b82224`): общих запросов 326.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 326 | +0.011 [+0.000…+0.025] |
| Δ nDCG@10 | 326 | +0.001 [-0.001…+0.004] |
| Δ nDCG@10 без группы e | 301 | +0.001 [-0.001…+0.004] |
| Δ nDCG@10 без новых целей корпуса | 326 | +0.001 [-0.001…+0.004] |

Общий запрос — тот же id, вид, `expect` (текст прежнего прогона не найден — не сверялся); метрики нового прогона — по разметке прежнего (`expect`; с `also`, если прежний прогон его знает — тогда и `also` тот же). «Без новых целей» — из выдачи убраны цели, которых не было в корпусе прежнего коммита (заметки его `eval/corpus` и insight его `eval/insights.toml` остаются).

Интервал Δ nDCG@10 не лежит целиком ниже нуля — регрессии нет.

## Промахи: ожидаемых заметок нет в top-10 (28)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, insight:p12-a
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/interactive-rebase-autosquash, en/git/merge-vs-rebase
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/db/postgres-zero-downtime-migrations, ru/long/postgres-major-upgrade-runbook
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/long/dev-environment-onboarding
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/long/kafka-consumer-lag-analysis
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, ru/db/postgres-zero-downtime-migrations, en/long/postgres-index-tuning-notes
- `c-03` (paraphrase, без общих слов): ожидалось ru/linux/rsync-delete-trailing-slash; top-3: ru/long/incident-disk-full-logs, ru/long/postgres-major-upgrade-runbook, ru/long/object-storage-backup-strategy
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log, en/rust/thiserror-vs-anyhow
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/long/ml-model-serving-notes, en/long/frontend-performance-audit, en/long/redis-cluster-migration
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `e-11` (paraphrase, без общих слов): ожидалось ru/zam/vault-secrets; top-3: ru/long/personal-data-retention-policy, ru/long/object-storage-backup-strategy, ru/windows/services-schtasks-robocopy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/long/dns-outage-postmortem, en/windows/powershell-pipeline-objects, insight:p13-a
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/research/dense-vs-sparse, ru/long/catalog-search-relevance-study, ru/long/ci-pipeline-retrospective
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/long/kafka-consumer-lag-analysis
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/long/frontend-performance-audit
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, ru/research/dense-vs-sparse
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/long/feature-flag-platform, en/docker/exit-code-137-container-oom
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-unit-restart, en/linux/systemd-timers-vs-cron, en/rust/e0499-two-mutable-borrows
- `g-03` (paraphrase, без общих слов): ожидалось en/devtools/eslint-prettier-conflict; top-3: en/git/interactive-rebase-autosquash, en/rust/thiserror-vs-anyhow, ru/git/blame-ignore-whitespace-moves
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/long/personal-data-retention-policy, ru/long/mobile-release-train, ru/research/metriki-ir
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/long/dev-environment-onboarding, ru/long/personal-data-retention-policy
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/long/payment-webhooks-reliability, insight:p06-h, en/git/sparse-checkout-partial-clone
