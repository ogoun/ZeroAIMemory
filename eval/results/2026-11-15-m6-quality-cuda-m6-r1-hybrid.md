# Eval: hybrid, bge-m3-fp16

- дата 2026-11-15, коммит `ac0a32d`
- корпус: 212 заметок (отпечаток `34aafe1af1e9c1ca`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- действующие `[search]` сервера: `{"attach_prefetch":30,"boost_area":1.0,"boost_axiom":1.2,"boost_failure":1.05,"boost_gotcha":1.05,"boost_hypothesis":1.0,"boost_theorem":1.1,"brief_min_rel":0.0,"budget_tokens":800,"contradicts_order":true,"graph_weight":0.5,"kind_boost_mode":"always","max_folders":2,"max_per_attachment":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_attach":1.0,"w_folder":0.0,"w_fts":1.0,"w_fts_alias":0.5,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.0,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 0.2 с
- время запроса (клиент): p50 23 мс, p95 33 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.845 [0.806–0.883] | 0.795 [0.755–0.834] | 0.818 [0.782–0.854] |
| broad | 25 | 0.912 [0.804–0.992] | 0.917 [0.817–1.000] | 0.889 [0.796–0.962] |
| certainty | 14 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| chunk | 70 | 0.986 [0.957–1.000] | 0.949 [0.905–0.986] | 0.962 [0.928–0.989] |
| cross-lang | 38 | 0.855 [0.737–0.961] | 0.720 [0.599–0.835] | 0.775 [0.675–0.868] |
| exact | 43 | 1.000 [1.000–1.000] | 0.922 [0.853–0.977] | 0.942 [0.891–0.983] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] |
| paraphrase | 61 | 0.331 [0.221–0.451] | 0.291 [0.195–0.394] | 0.350 [0.259–0.445] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.883 [0.795–0.955] | 0.912 [0.847–0.966] |
| перефраз без общих слов | 46 | 0.112 [0.033–0.203] | 0.108 [0.051–0.175] | 0.173 [0.110–0.245] |
| длинные заметки | 70 | 0.986 [0.957–1.000] | 0.949 [0.905–0.986] | 0.962 [0.928–0.989] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.778 [0.625–0.917] | 0.835 [0.721–0.938] |
| запрос en | 152 | 0.857 [0.801–0.910] | 0.831 [0.774–0.881] | 0.847 [0.795–0.892] |
| запрос ru | 174 | 0.834 [0.780–0.888] | 0.765 [0.711–0.820] | 0.794 [0.746–0.843] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 92 %, trigram 26 %, vector 96 %, vector_only 8 %.

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
| chunk-nDCG@10 | 70 | 0.869 [0.810–0.922] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.986 [0.957–1.000] |

## Сравнение с прежним прогоном

`.tmp/m6/task13b/quality/cuda/b56-r1/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (запросы `49a3b34b15e9b432`, корпус `34aafe1af1e9c1ca`): общих запросов 326.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 326 | +0.000 [+0.000…+0.000] |
| Δ nDCG@10 | 326 | +0.002 [-0.001…+0.006] |
| Δ nDCG@10 без группы e | 301 | +0.002 [-0.001…+0.006] |
| Δ nDCG@10 без новых целей корпуса | 326 | +0.002 [-0.001…+0.006] |

Общий запрос — тот же id, вид, `expect` (текст прежнего прогона не найден — не сверялся); метрики нового прогона — по разметке прежнего (`expect`; с `also`, если прежний прогон его знает — тогда и `also` тот же). «Без новых целей» — из выдачи убраны цели, которых не было в корпусе прежнего коммита (заметки его `eval/corpus` и insight его `eval/insights.toml` остаются).

Интервал Δ nDCG@10 не лежит целиком ниже нуля — регрессии нет.

## Промахи: ожидаемых заметок нет в top-10 (28)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, ru/rust/arc-mutex-deadlock
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/merge-vs-rebase, en/git/interactive-rebase-autosquash
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/long/dev-environment-onboarding, ru/git/revert-vs-reset, ru/long/mobile-release-train
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, insight:k13-t, ru/long/kafka-consumer-lag-analysis
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, en/api/health-endpoint-design, en/api/database-statement-timeout
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: en/api/idempotency-keys, en/long/dns-outage-postmortem, ru/zam/qdrant-managed-process
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: insight:p20-t, en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared, en/rust/e0597-does-not-live-long-enough
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: insight:k03-t, en/long/dns-outage-postmortem, en/long/frontend-performance-audit
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: insight:p18-t, insight:k17-t, en/web/infinite-scroll
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, insight:p10-a, ru/long/postgres-major-upgrade-runbook
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/web/fetch-retry-abort, en/long/dns-outage-postmortem, insight:k10-n
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy, ru/windows/path-setx-truncation
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/devtools/code-review-checklist, ru/long/ci-pipeline-retrospective, ru/research/rrf-sliyanie
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/infra/readiness-liveness-probes, ru/long/kafka-consumer-lag-analysis
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/long/frontend-performance-audit
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, insight:p05-h
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/long/feature-flag-platform, insight:k04-n, en/rust/workspace-dependencies-and-profiles
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: insight:k14-t, en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/long/mobile-release-train, ru/long/personal-data-retention-policy
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/long/personal-data-retention-policy, ru/long/dev-environment-onboarding
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: ru/web/service-worker-cache, en/long/payment-webhooks-reliability, en/long/observability-stack-design
