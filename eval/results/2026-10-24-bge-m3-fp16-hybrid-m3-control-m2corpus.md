# Eval: hybrid, bge-m3-fp16

- дата 2026-10-24, коммит `9e67022`
- корпус: 160 заметок (отпечаток `9b01f11a95c030de`), запросов 197 (отпечаток `5819d7ddcda33ef0`)
- модель `bge-m3-fp16@4f1a646a`, устройство cuda ()
- действующие `[search]` сервера: `{"boost_area":1.1,"boost_axiom":1.2,"boost_failure":1.15,"boost_gotcha":1.15,"boost_hypothesis":1.0,"boost_theorem":1.1,"graph_weight":0.5,"max_folders":2,"max_per_node":3,"recency_half_life_days":180.0,"rrf_k":60.0,"w_folder":0.0,"w_fts":1.0,"w_fts_relaxed":0.25,"w_trigram":0.7,"w_vector":1.0}`; `[index]`: `{"batch_tokens":8192,"chunk_hard_max":800,"chunk_merge_below":100,"chunk_overlap_sentences":1,"chunk_target_max":500,"chunk_target_min":300,"rollup_debounce_ms":2000,"yield_ms":5}`
- индексация корпуса: 33.1 с
- время запроса (клиент): p50 19 мс, p95 23 мс

В скобках — 95 % интервал бутстрепа по запросам (eval/README.md).

| Запросы | n | recall@5 | MRR@10 | nDCG@10 |
|---|---:|---|---|---|
| все | 197 | 0.760 [0.701–0.817] | 0.725 [0.667–0.781] | 0.750 [0.696–0.801] |
| broad | 25 | 0.912 [0.804–0.992] | 0.947 [0.860–1.000] | 0.916 [0.827–0.986] |
| cross-lang | 38 | 0.868 [0.763–0.974] | 0.760 [0.651–0.859] | 0.817 [0.733–0.894] |
| exact | 31 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| identifier | 42 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 61 | 0.344 [0.230–0.459] | 0.282 [0.190–0.384] | 0.340 [0.251–0.438] |
| перефраз без общих слов | 46 | 0.152 [0.065–0.261] | 0.067 [0.041–0.095] | 0.140 [0.089–0.190] |
| запрос en | 88 | 0.770 [0.680–0.852] | 0.745 [0.658–0.826] | 0.767 [0.686–0.843] |
| запрос ru | 109 | 0.752 [0.670–0.830] | 0.708 [0.628–0.785] | 0.736 [0.662–0.806] |

Каналы, которыми найдены ожидаемые заметки в top-10: fts 89 %, trigram 31 %, vector 100 %, vector_only 11 %.

## Сравнение с прежним прогоном

`eval/results/2026-10-20-bge-m3-fp16-hybrid-tuned.json` (запросы `5819d7ddcda33ef0`, корпус `2ed461ea7979b814`): общих запросов 197.

| Разность (новый − прежний) | n | значение |
|---|---:|---|
| Δ recall@5 | 197 | +0.005 [+0.000…+0.015] |
| Δ nDCG@10 | 197 | +0.001 [-0.006…+0.008] |
| Δ nDCG@10 без группы e | 172 | -0.002 [-0.009…+0.005] |
| Δ nDCG@10 без новых целей корпуса | 197 | +0.001 [-0.006…+0.008] |

Общий запрос — тот же id, вид, `expect` и текст; метрики нового прогона — по `expect` (без `also`). «Без новых целей» — из выдачи убраны insight и заметки, которых не было в корпусе прежнего коммита.

Интервал Δ nDCG@10 не лежит целиком ниже нуля — регрессии нет.

## Промахи: ожидаемых заметок нет в top-10 (28)

- `a-02` (paraphrase, без общих слов): ожидалось ru/windows/crlf-bash-scripts; top-3: ru/rust/arc-mutex-deadlock, ru/git/hooks-commit-msg, ru/git/revert-vs-reset
- `a-05` (paraphrase, без общих слов): ожидалось en/windows/winget-package-setup; top-3: en/devtools/npm-ci-clean-install, en/linux/ssh-config-agent-forwarding, en/git/worktree
- `b-04` (paraphrase, без общих слов): ожидалось ru/git/detached-head; top-3: ru/git/revert-vs-reset, ru/devtools/semver-and-changelog, ru/db/postgres-zero-downtime-migrations
- `b-05` (paraphrase, без общих слов): ожидалось ru/git/tags-annotated-lightweight; top-3: ru/devtools/semver-and-changelog, ru/devtools/uv-package-manager, ru/zam/markdown-chunking
- `b-06` (paraphrase, без общих слов): ожидалось ru/git/blame-ignore-whitespace-moves; top-3: ru/rust/arc-mutex-deadlock, ru/research/kvantovanie-int8, ru/research/rrf-sliyanie
- `c-01` (paraphrase, без общих слов): ожидалось en/docker/compose-healthcheck-depends-on; top-3: ru/db/sqlite-busy-timeout, en/db/connection-pool-exhaustion, ru/db/postgres-zero-downtime-migrations
- `c-02` (paraphrase, без общих слов): ожидалось en/linux/disk-full-but-df-shows-space; top-3: en/docker/compose-healthcheck-depends-on, ru/zam/qdrant-managed-process, en/git/sparse-checkout-partial-clone
- `c-04` (paraphrase, без общих слов): ожидалось ru/linux/ulimit-open-files-emfile; top-3: ru/ops/nginx-reverse-proxy-headers, ru/git/detached-head, ru/devtools/pyproject-editable-install
- `c-05` (paraphrase, без общих слов): ожидалось en/linux/ssh-config-agent-forwarding; top-3: en/rust/thiserror-vs-anyhow, en/rust/dyn-trait-object-safety, en/rust/e0502-mutable-borrow-while-shared
- `d-01` (paraphrase, без общих слов): ожидалось en/rust/split-at-mut; top-3: en/git/worktree, en/rust/e0499-two-mutable-borrows, en/rust/cargo-feature-unification-resolver
- `d-02` (paraphrase, без общих слов): ожидалось en/rust/e0106-missing-lifetime-specifier; top-3: en/rust/e0597-does-not-live-long-enough, en/rust/e0499-two-mutable-borrows, en/rust/e0502-mutable-borrow-while-shared
- `d-04` (paraphrase, без общих слов): ожидалось ru/rust/clippy-ci-deny-warnings; top-3: ru/ops/postmortem-blameless, ru/db/sqlite-wal, ru/devtools/code-review-checklist
- `d-05` (paraphrase, без общих слов): ожидалось ru/rust/rwlock-vs-mutex; top-3: ru/devtools/semver-and-changelog, ru/research/kvantovanie-int8, ru/research/dense-vs-sparse
- `e-12` (paraphrase, без общих слов): ожидалось ru/zam/index-jobs-outbox; top-3: ru/zam/hybrid-search-rrf, ru/db/sqlite-vacuum, ru/devtools/code-review-checklist
- `e-13` (paraphrase, без общих слов): ожидалось ru/zam/config-layers-env; top-3: ru/windows/git-autocrlf-gitattributes, ru/research/dense-vs-sparse, ru/rust/rwlock-vs-mutex
- `e-15` (paraphrase, без общих слов): ожидалось en/zam/windows-data-dir-path-limit; top-3: en/windows/powershell-native-stderr, en/linux/systemd-unit-restart, en/devtools/npm-ci-clean-install
- `f-01` (paraphrase, без общих слов): ожидалось ru/research/butstrep-intervaly; top-3: ru/devtools/code-review-checklist, ru/research/kross-enkodery-rerank, ru/research/rrf-sliyanie
- `f-02` (paraphrase, без общих слов): ожидалось ru/research/lineynaya-smes-ballov; top-3: ru/research/rrf-sliyanie, ru/research/metriki-ir, ru/research/bm25-parametry
- `f-03` (paraphrase, без общих слов): ожидалось ru/research/hyde-gipoteticheskiy-dokument; top-3: ru/research/dense-vs-sparse, ru/research/metriki-ir, ru/windows/services-schtasks-robocopy
- `f-04` (paraphrase, без общих слов): ожидалось en/research/contextual-retrieval; top-3: en/research/chunking-strategies, en/zam/answer-cache-protocol, ru/zam/architecture-overview
- `f-05` (paraphrase, без общих слов): ожидалось en/research/query-expansion-prf; top-3: en/research/learned-sparse-splade, ru/research/metriki-ir, en/git/submodules-pitfalls
- `g-01` (paraphrase, без общих слов): ожидалось en/devtools/gha-workflow-concurrency; top-3: en/rust/workspace-dependencies-and-profiles, en/rust/musl-static-cross-compile, en/docker/exit-code-137-container-oom
- `g-02` (paraphrase, без общих слов): ожидалось en/devtools/makefile-phony-targets; top-3: en/linux/systemd-timers-vs-cron, en/linux/systemd-unit-restart, en/rust/e0499-two-mutable-borrows
- `g-04` (paraphrase, без общих слов): ожидалось ru/devtools/python-venv; top-3: ru/research/metriki-ir, ru/devtools/pyproject-editable-install, ru/devtools/semver-and-changelog
- `g-05` (paraphrase, без общих слов): ожидалось ru/devtools/pre-commit-hooks; top-3: ru/devtools/code-review-checklist, ru/windows/git-autocrlf-gitattributes, ru/ops/tls-incomplete-chain
- `h-02` (paraphrase, без общих слов): ожидалось ru/ops/log-rotation; top-3: ru/devtools/semver-and-changelog, ru/ops/retries-backoff-jitter, ru/docker/container-log-rotation
- `h-04` (paraphrase, без общих слов): ожидалось en/ops/dns-ttl-migration; top-3: en/git/interactive-rebase-autosquash, en/docker/alpine-musl-dns, en/git/reflog-recovery
- `h-05` (paraphrase, без общих слов): ожидалось en/db/connection-pool-exhaustion; top-3: en/windows/wsl2-localhost-forwarding, en/linux/systemd-unit-restart, en/devtools/playwright-flaky-e2e
