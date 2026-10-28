# Eval: hybrid, bge-m3-fp16

- дата 2026-10-28, коммит `5226674`
- корпус: 182 заметок (отпечаток `3e0777bde1b82224`), запросов 326 (отпечаток `49a3b34b15e9b432`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- конфигурация: `search.boost_axiom=2.0`, `search.boost_theorem=1.5`
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":2.0,"boost_failure":1.05,"boost_gotcha":1.05,"boost_hypothesis":1.0,"boost_theorem":1.5,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_boundary_lambda":0.0,"chunk_cohesion":false,"chunk_hard_max":800,"chunk_heading_path_h4":true,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_prefix_l0":false,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 136.7 с
- время запроса (клиент): p50 21 мс, p95 24 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 326 | 0.845 [0.806–0.882] | 0.749 [0.709–0.786] | 0.785 [0.750–0.820] |
| broad | 25 | 0.872 [0.759–0.967] | 0.790 [0.662–0.911] | 0.820 [0.720–0.913] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.964 [0.893–1.000] | 0.974 [0.921–1.000] |
| chunk | 70 | 0.986 [0.957–1.000] | 0.891 [0.835–0.939] | 0.918 [0.876–0.955] |
| cross-lang | 38 | 0.855 [0.737–0.961] | 0.688 [0.575–0.799] | 0.751 [0.653–0.841] |
| exact | 43 | 1.000 [1.000–1.000] | 0.873 [0.795–0.938] | 0.904 [0.846–0.952] |
| identifier | 42 | 1.000 [1.000–1.000] | 0.976 [0.940–1.000] | 0.982 [0.956–1.000] |
| paraphrase | 61 | 0.347 [0.238–0.467] | 0.278 [0.182–0.381] | 0.338 [0.250–0.430] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.811 [0.712–0.904] | 0.859 [0.784–0.929] |
| перефраз без общих слов | 46 | 0.134 [0.047–0.232] | 0.122 [0.058–0.198] | 0.181 [0.114–0.251] |
| длинные заметки | 70 | 0.986 [0.957–1.000] | 0.891 [0.835–0.939] | 0.918 [0.876–0.955] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.628 [0.486–0.792] | 0.723 [0.619–0.846] |
| запрос en | 152 | 0.864 [0.809–0.914] | 0.772 [0.715–0.827] | 0.806 [0.753–0.855] |
| запрос ru | 174 | 0.829 [0.776–0.881] | 0.728 [0.675–0.784] | 0.767 [0.719–0.816] |

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
| chunk-nDCG@10 | 70 | 0.836 [0.772–0.891] |
| chunk-recall@5 с перекрытием (справочно) | 70 | 0.986 [0.957–1.000] |

## Сравнение с прежним прогоном

`eval/results/2026-10-28-bge-m3-fp16-hybrid-m4-final.json` (запросы `49a3b34b15e9b432`, корпус `3e0777bde1b82224`): общих запросов 326.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 326 | -0.005 [-0.013…+0.000] |
| Δ nDCG@10 | 326 | -0.039 [-0.051…-0.027] |
| Δ nDCG@10 без группы e | 301 | -0.037 [-0.050…-0.025] |
| Δ nDCG@10 без новых целей корпуса | 326 | -0.039 [-0.051…-0.027] |

Общий запрос — тот же id, вид, `expect` (текст прежнего прогона не найден — не сверялся); метрики нового прогона — по разметке прежнего (`expect`; с `also`, если прежний прогон его знает — тогда и `also` тот же). «Без новых целей» — из выдачи убраны цели, которых не было в корпусе прежнего коммита (заметки его `eval/corpus` и insight его `eval/insights.toml` остаются).

**Регрессия:** интервал Δ nDCG@10 целиком ниже нуля.

## Промахи: ожидаемых заметок нет в top-10 (28)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, ru/rust/arc-mutex-deadlock
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/merge-vs-rebase, en/git/interactive-rebase-autosquash
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/long/dev-environment-onboarding, ru/git/revert-vs-reset, ru/long/mobile-release-train
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/long/kafka-consumer-lag-analysis
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, ru/db/postgres-zero-downtime-migrations, en/ops/slo-burn-rate-alerts
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/long/mobile-release-train, ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log, en/rust/thiserror-vs-anyhow
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: insight:p11-a, en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/long/ml-model-serving-notes, en/long/frontend-performance-audit, en/long/postgres-index-tuning-notes
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, insight:p07-t
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: insight:p10-a, ru/windows/git-autocrlf-gitattributes, ru/long/incident-disk-full-logs
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/long/dns-outage-postmortem, en/windows/powershell-pipeline-objects, en/devtools/npm-ci-clean-install
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: insight:p13-a, ru/windows/long-paths-max-path, ru/windows/services-schtasks-robocopy
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/devtools/code-review-checklist, ru/long/ci-pipeline-retrospective, ru/research/rrf-sliyanie
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/long/kafka-consumer-lag-analysis, ru/long/incident-disk-full-logs
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/long/frontend-performance-audit
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: ru/research/metriki-ir, en/research/learned-sparse-splade, en/ops/http-cache-headers
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, insight:p05-h
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: insight:p06-t, en/long/feature-flag-platform, en/rust/workspace-dependencies-and-profiles
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart, en/git/worktree
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/long/personal-data-retention-policy, ru/long/mobile-release-train
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/long/personal-data-retention-policy, ru/long/dev-environment-onboarding
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/long/dns-outage-postmortem, en/long/payment-webhooks-reliability
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/windows/wsl2-localhost-forwarding, insight:p06-t, en/long/payment-webhooks-reliability
