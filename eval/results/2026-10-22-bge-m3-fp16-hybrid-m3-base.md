# Eval: hybrid, bge-m3-fp16

- дата 2026-10-22, коммит `a2ae4a7`
- корпус: 182 заметок (отпечаток `61048e2b77fd9bce`), запросов 314 (отпечаток `e7e27f84a0a26026`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.15,"boost_gotcha":1.15,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_hard_max":800,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 50.9 с
- время запроса (клиент): p50 20 мс, p95 23 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 314 | 0.839 [0.799–0.879] | 0.761 [0.720–0.802] | 0.792 [0.754–0.828] |
| broad | 25 | 0.932 [0.860–0.992] | 0.880 [0.770–0.970] | 0.866 [0.776–0.939] |
| certainty | 14 | 1.000 [1.000–1.000] | 0.869 [0.714–1.000] | 0.902 [0.787–1.000] |
| chunk | 58 | 0.983 [0.948–1.000] | 0.917 [0.857–0.966] | 0.934 [0.881–0.975] |
| cross-lang | 38 | 0.842 [0.711–0.947] | 0.688 [0.575–0.798] | 0.752 [0.656–0.843] |
| exact | 43 | 1.000 [1.000–1.000] | 0.890 [0.820–0.953] | 0.918 [0.867–0.966] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.328 [0.213–0.443] | 0.270 [0.181–0.366] | 0.335 [0.249–0.428] |
| superseded | 33 | 0.970 [0.909–1.000] | 0.867 [0.780–0.943] | 0.901 [0.835–0.957] |
| перефраз без общих слов | 46 | 0.130 [0.043–0.239] | 0.072 [0.044–0.106] | 0.148 [0.097–0.204] |
| длинные заметки | 58 | 0.983 [0.948–1.000] | 0.917 [0.857–0.966] | 0.934 [0.881–0.975] |
| FTS-only (index: false) | 12 | 1.000 [1.000–1.000] | 0.688 [0.542–0.833] | 0.768 [0.659–0.877] |
| запрос en | 146 | 0.855 [0.793–0.910] | 0.778 [0.719–0.833] | 0.809 [0.755–0.859] |
| запрос ru | 168 | 0.824 [0.768–0.878] | 0.746 [0.690–0.803] | 0.777 [0.725–0.829] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 92 %, trigram 27 %, vector 96 %, vector_only 8 %.

## Метрики M3

cancelled@5 — доля запросов `superseded` с замещённым или отозванным insight в top-5; reliable_first — доля запросов `certainty`, где theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` (лист целиком содержит `evidence`, перекрытие с предыдущим листом не считается).

| Метрика M3 | n | значение |
|---|---:|---|
| cancelled@5 (цель 0) | 33 | 0.000 [0.000–0.000] |
| cancelled@5 с include_history (контроль, > 0) | 33 | 1.000 [1.000–1.000] |
| reliable_first, бусты certainty сервера | 14 | 0.786 [0.571–1.000] |
| reliable_first без бустов | 14 | 0.143 [0.000–0.357] |
| chunk-recall@5 (свой текст листа) | 58 | 0.897 [0.810–0.966] |
| chunk-nDCG@10 | 58 | 0.789 [0.703–0.872] |
| chunk-recall@5 с перекрытием (справочно) | 58 | 0.897 [0.810–0.966] |

## Сравнение с прежним прогоном

`eval/results/2026-10-20-bge-m3-fp16-hybrid-tuned.json` (запросы `5819d7ddcda33ef0`, корпус `2ed461ea7979b814`): общих запросов 197.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 197 | -0.003 [-0.015…+0.008] |
| Δ nDCG@10 | 197 | -0.023 [-0.040…-0.008] |
| Δ nDCG@10 без группы e | 172 | -0.020 [-0.038…-0.004] |

**Регрессия:** интервал Δ nDCG@10 целиком ниже нуля.

## Промахи: ожидаемых заметок нет в top-10 (30)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/long/dev-environment-onboarding, ru/long/catalog-search-relevance-study, ru/long/object-storage-backup-strategy
- `a-04` (paraphrase, без общих слов): ожидалось ru/windows/defender-exclusions-build; top-3: insight:c02-b, ru/long/ci-pipeline-retrospective, ru/research/fts5-tokenizatory
- `b-01` (paraphrase, без общих слов): ожидалось en/git/force-with-lease; top-3: insight:p11-a, en/git/merge-vs-rebase, en/git/interactive-rebase-autosquash
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/long/mobile-release-train, ru/long/dev-environment-onboarding
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/long/dev-environment-onboarding, ru/devtools/uv-package-manager
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/long/kafka-consumer-lag-analysis, ru/research/kvantovanie-int8
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: en/long/dns-outage-postmortem, en/long/postgres-index-tuning-notes, ru/db/postgres-zero-downtime-migrations
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/long/mobile-release-train, ru/git/detached-head
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/long/kubernetes-upgrade-log, en/long/dns-outage-postmortem, en/rust/thiserror-vs-anyhow
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/rust/e0499-two-mutable-borrows, en/git/worktree, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-03` (paraphrase, без общих слов): ожидалось en/rust/tokio-spawn-blocking; top-3: en/long/ml-model-serving-notes, en/long/frontend-performance-audit, en/long/postgres-index-tuning-notes
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/long/ci-pipeline-retrospective, ru/ops/postmortem-blameless, ru/db/sqlite-wal
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/long/kafka-consumer-lag-analysis, ru/long/object-storage-backup-strategy, ru/long/personal-data-retention-policy
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/long/object-storage-backup-strategy, ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/long/dev-environment-onboarding, ru/windows/git-autocrlf-gitattributes, insight:p10-a
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/long/dns-outage-postmortem, en/devtools/npm-ci-clean-install, en/windows/powershell-pipeline-objects
- `e-20` (cross-lang): ожидалось en/zam/windows-data-dir-path-limit; top-3: ru/windows/long-paths-max-path, insight:c02-b, ru/windows/services-schtasks-robocopy
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/devtools/code-review-checklist, ru/research/dense-vs-sparse, ru/research/rrf-sliyanie
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/long/catalog-search-relevance-study, ru/long/incident-disk-full-logs
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, en/long/frontend-performance-audit
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: ru/research/metriki-ir, en/research/learned-sparse-splade, en/long/api-versioning-guide
- `f-20` (cross-lang): ожидалось en/research/multilingual-embeddings; top-3: ru/zam/degraded-search-mode, ru/long/catalog-search-relevance-study, insight:p05-h
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/long/feature-flag-platform, en/rust/workspace-dependencies-and-profiles, en/rust/musl-static-cross-compile
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/long/oncall-handbook, en/linux/systemd-unit-restart
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/long/mobile-release-train, ru/long/personal-data-retention-policy
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/long/personal-data-retention-policy, ru/long/object-storage-backup-strategy, ru/devtools/code-review-checklist
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: insight:c07-b, ru/long/personal-data-retention-policy, ru/long/incident-disk-full-logs
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/windows/wsl2-localhost-forwarding, en/long/payment-webhooks-reliability, en/devtools/playwright-flaky-e2e
- `k-11` (chunk): ожидалось ru/long/catalog-search-relevance-study; top-3: ru/research/bm25-parametry, ru/zam/hybrid-search-rrf, ru/research/kross-enkodery-rerank
