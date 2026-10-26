# Eval: hybrid, bge-m3-fp16

- дата 2026-10-24, коммит `9e67022`
- корпус: 182 заметок (отпечаток `5515768f5fcd6903`), запросов 316 (отпечаток `092eed412c33f095`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.15,"boost_gotcha":1.15,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_hard_max":800,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 55.4 с
- время запроса (клиент): p50 21 мс, p95 27 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 316 | 0.845 [0.804–0.882] | 0.782 [0.740–0.822] | 0.804 [0.766–0.839] |
| broad | 25 | 0.932 [0.860–0.992] | 0.938 [0.844–1.000] | 0.904 [0.814–0.975] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.917 [0.786–1.000] | 0.938 [0.840–1.000] |
| chunk | 60 | 0.983 [0.950–1.000] | 0.929 [0.871–0.975] | 0.943 [0.893–0.982] |
| cross-lang | 38 | 0.855 [0.737–0.961] | 0.708 [0.599–0.816] | 0.766 [0.672–0.855] |
| exact | 43 | 1.000 [1.000–1.000] | 0.913 [0.849–0.971] | 0.935 [0.887–0.978] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.996 [0.989–1.000] |
| paraphrase | 61 | 0.347 [0.230–0.467] | 0.301 [0.202–0.409] | 0.344 [0.251–0.442] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.867 [0.780–0.943] | 0.901 [0.835–0.957] |
| перефраз без общих слов | 46 | 0.134 [0.043–0.232] | 0.116 [0.052–0.193] | 0.162 [0.099–0.233] |
| длинные заметки | 60 | 0.983 [0.950–1.000] | 0.929 [0.871–0.975] | 0.943 [0.893–0.982] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.688 [0.542–0.833] | 0.768 [0.659–0.877] |
| запрос en | 145 | 0.850 [0.792–0.906] | 0.790 [0.730–0.846] | 0.813 [0.758–0.864] |
| запрос ru | 171 | 0.840 [0.787–0.892] | 0.775 [0.717–0.828] | 0.797 [0.746–0.845] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 93 %, trigram 27 %, vector 96 %, vector_only 7 %.

Разрезано границами листов доказательств запросов `chunk`: 9 из 60 (из них целиком в листе вместе с перекрытием — 3); листов длинных заметок 213. Потолок chunk-recall@5 — 0.850.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 0.857 [0.643–1.000] |
| reliable_first без бустов | 14 | 0.357 [0.143–0.643] |
| chunk-recall@5 (свой текст листа) | 60 | 0.833 [0.733–0.917] |
| chunk-nDCG@10 | 60 | 0.761 [0.663–0.849] |
| chunk-recall@5 с перекрытием (справочно) | 60 | 0.867 [0.783–0.950] |

## Сравнение с прежним прогоном

`eval/results/2026-10-20-bge-m3-fp16-hybrid-tuned.json` (запросы `5819d7ddcda33ef0`, корпус `2ed461ea7979b814`): общих запросов 197.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 197 | -0.003 [-0.020…+0.015] |
| Δ nDCG@10 | 197 | -0.027 [-0.046…-0.011] |
| Δ nDCG@10 без группы e | 172 | -0.025 [-0.044…-0.006] |
| Δ nDCG@10 без новых целей корпуса | 197 | +0.009 [-0.007…+0.027] |

Общий запрос — тот же id, вид, `expect` и текст; метрики нового прогона — по `expect` (без `also`). «Без новых целей» — из выдачи убраны insight и заметки, которых не было в корпусе прежнего коммита.

**Регрессия:** интервал Δ nDCG@10 целиком ниже нуля.

## Промахи: ожидаемых заметок нет в top-10 (31)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/object-storage-backup-strategy, ru/rust/arc-mutex-deadlock
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/devtools/npm-ci-clean-install, en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/merge-vs-rebase, en/git/interactive-rebase-autosquash
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/long/mobile-release-train, ru/db/postgres-zero-downtime-migrations
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/research/kross-enkodery-rerank
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/long/kafka-consumer-lag-analysis, ru/research/rrf-sliyanie
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, en/long/postgres-index-tuning-notes, ru/db/postgres-zero-downtime-migrations
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/long/mobile-release-train, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/dns-outage-postmortem, en/long/kubernetes-upgrade-log, en/rust/thiserror-vs-anyhow
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/long/ml-model-serving-notes, en/long/frontend-performance-audit, en/long/postgres-index-tuning-notes
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/personal-data-retention-policy, ru/long/object-storage-backup-strategy
- `d-06` (paraphrase, без общих слов): ожидалось en/rust/e0502-mutable-borrow-while-shared; top-3: en/git/interactive-rebase-autosquash, en/devtools/playwright-flaky-e2e, en/long/dns-outage-postmortem
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/long/dev-environment-onboarding, ru/windows/git-autocrlf-gitattributes, insight:p10-a
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/long/dns-outage-postmortem, en/windows/powershell-pipeline-objects, en/linux/systemd-unit-restart
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, insight:c02-b, ru/windows/services-schtasks-robocopy
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/devtools/code-review-checklist, ru/long/ci-pipeline-retrospective, ru/research/dense-vs-sparse
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/long/catalog-search-relevance-study
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/long/frontend-performance-audit
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/long/ml-model-serving-notes, ru/research/metriki-ir, en/research/learned-sparse-splade
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, insight:p05-h
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/long/feature-flag-platform, en/rust/workspace-dependencies-and-profiles, en/rust/musl-static-cross-compile
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart, en/long/oncall-handbook
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/long/mobile-release-train, ru/long/personal-data-retention-policy
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/ci-pipeline-retrospective, ru/long/personal-data-retention-policy, ru/devtools/code-review-checklist
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/windows/wsl2-localhost-forwarding, en/long/payment-webhooks-reliability, en/long/dns-outage-postmortem
- `h-06` (paraphrase, без общих слов): ожидалось en/ops/load-testing-percentiles; top-3: en/long/oncall-handbook, en/git/worktree, en/research/contextual-retrieval
- `k-11` (chunk): ожидалось ru/long/catalog-search-relevance-study; top-3: ru/research/bm25-parametry, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
