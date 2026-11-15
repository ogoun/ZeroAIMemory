# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdfium)

Код — `6eaf2ba`, 2026-11-08; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 14 с, индексация после него 9 с. Сервер жив после набора: да.

Переопределения: `search.recency_half_life_days=0`, `search.w_attach=0.7`.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.422 [0.289–0.556] | 0.978 [0.933–1.000] | 0.418 [0.290–0.557] | 0.400 [0.267–0.533] | 0.947 [0.842–1.000] | 0.947 [0.842–1.000] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.875 [0.625–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 0.333 [0.000–1.000] | 1.000 [1.000–1.000] | 0.375 [0.000–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 0.739 [0.565–0.913] | 1.000 [1.000–1.000] | 0.733 [0.563–0.888] | 0.739 [0.565–0.913] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.091 [0.000–0.273] | 1.000 [1.000–1.000] | 0.077 [0.032–0.125] | 0.091 [0.000–0.273] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| docx | 12 | 0.333 [0.083–0.583] | 1.000 [1.000–1.000] | 0.318 [0.105–0.556] | 0.333 [0.083–0.583] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pdf | 20 | 0.400 [0.200–0.600] | 0.950 [0.850–1.000] | 0.432 [0.235–0.642] | 0.400 [0.200–0.600] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.500 [0.125–0.875] | 1.000 [1.000–1.000] | 0.422 [0.141–0.766] | 0.375 [0.125–0.750] | 0.750 [0.250–1.000] | 0.750 [0.250–1.000] |

Латентность запросов набора (клиент): p50 34.4 мс, p95 60.7 мс.
Основной набор с вложениями (326 запросов): nDCG@10 0.819 [0.783–0.853], recall@5 0.844 [0.805–0.882].

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 8 | — | — | — | ru/git/revert-vs-reset [vector,fts];  [vector]; ru/windows/services-schtasks-robocopy [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/ops/retries-backoff-jitter [vector] |
| d1-para | paraphrase | 7 | — | — | — | ru/api/rate-limiting-token-bucket [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/long/ci-pipeline-retrospective [vector]; ru/long/incident-disk-full-logs [vector] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector]; ru/zam/hybrid-search-rrf [vector]; ru/git/revert-vs-reset [vector]; ru/git/clean-fdx [vector] |
| d3-para | paraphrase | — | — | — | — | ru/long/catalog-search-relevance-study [vector]; ru/long/dev-environment-onboarding [vector];  [vector]; ru/research/metriki-ir [vector]; ru/research/rrf-sliyanie [vector] |
| d4-exact | exact | 5 | para:47 | да | да | ru/long/checkout-load-testing-report [vector,fts]; ru/rust/arc-mutex-deadlock [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/long/incident-disk-full-logs [vector,fts]; regulations-ru.docx · para:47 [vector,fts] |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles [vector,fts];  [vector,fts]; en/infra/load-balancer-health-checks [vector,fts]; en/long/payment-webhooks-reliability [vector,fts]; en/long/oncall-handbook [vector,fts] |
| d5-para | paraphrase | 9 | — | — | — | ru/long/object-storage-backup-strategy [vector,fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector]; ru/ops/backups-restore-testing [fts]; ru/long/kafka-consumer-lag-analysis [vector]; ru/long/incident-disk-full-logs [vector] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; en/long/oncall-handbook [vector]; en/long/observability-stack-design [vector]; en/long/dns-outage-postmortem [vector];  [vector] |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/windows/services-schtasks-robocopy [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/web/service-worker-cache [vector,fts] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts]; en/web/token-refresh-tabs [vector];  [vector]; ru/ops/tls-expired-certificate [vector]; en/long/dns-outage-postmortem [vector] |
| e3-para | paraphrase | 10 | — | — | — | en/devtools/gha-matrix-strategy [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/long/redis-cluster-migration [vector,fts]; en/long/payment-webhooks-reliability [vector,fts] |
| e4-para | paraphrase | 7 | — | — | — | en/long/oncall-handbook [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/feature-flag-platform [vector];  [vector] |
| x1-exact | exact | — | — | — | — | ru/ops/postmortem-blameless [vector];  [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/zam/config-layers-env [vector,fts];  [vector] |
| x1-para | paraphrase | 4 | sheet:Поставки!A1218:G1236 | да | да | ru/long/mobile-release-train [vector,fts,trigram]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/long/catalog-search-relevance-study [vector,fts]; inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; en/rust/future-not-send-mutexguard-await [vector] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/research/kross-enkodery-rerank [vector]; ru/windows/services-schtasks-robocopy [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; en/long/oncall-handbook [vector];  [vector]; en/git/force-with-lease [vector]; en/long/kubernetes-upgrade-log [vector] |
| x3-cross | cross-lang | — | — | — | — |  [vector,fts]; ru/long/ci-pipeline-retrospective [vector,fts]; ru/infra/proxy-timeouts-long-requests [vector]; ru/windows/services-schtasks-robocopy [vector]; ru/long/checkout-load-testing-report [vector] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram];  [vector];  [vector]; ru/long/checkout-load-testing-report [vector];  [vector] |
| x5-date | date | 8 | — | — | — | ru/long/postgres-major-upgrade-runbook [vector,fts]; ru/web/date-formatting-intl [vector,fts]; ru/long/personal-data-retention-policy [vector]; ru/long/checkout-load-testing-report [vector]; en/git/sparse-checkout-partial-clone [vector] |
| x6-date | date | — | — | — | — | en/long/oncall-handbook [vector];  [vector]; en/long/observability-stack-design [vector];  [vector]; en/long/feature-flag-platform [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; en/long/redis-cluster-migration [vector]; en/ops/dns-ttl-migration [vector]; en/long/kubernetes-upgrade-log [vector]; en/long/postgres-index-tuning-notes [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; ru/infra/proxy-timeouts-long-requests [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/api/service-request-timeouts [vector] |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide [vector,fts];  [vector,fts]; en/api/database-statement-timeout [vector,fts]; en/long/redis-cluster-migration [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts] |
| p3-para | paraphrase | — | — | — | — | en/long/oncall-handbook [vector,fts]; en/api/database-statement-timeout [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts]; en/research/chunking-strategies [vector,fts] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; ru/ops/retries-backoff-jitter [vector]; ru/long/mobile-release-train [vector]; ru/long/dev-environment-onboarding [vector]; en/long/kubernetes-upgrade-log [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; en/zam/reranker-license [vector]; en/long/ml-model-serving-notes [vector];  [vector]; en/research/colbert-late-interaction [vector] |
| o2-para | paraphrase | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/dns-outage-postmortem [vector];  [vector]; en/long/oncall-handbook [vector,fts]; en/long/postgres-index-tuning-notes [vector,fts] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; en/long/api-versioning-guide [vector]; en/research/colbert-late-interaction [vector]; en/research/learned-sparse-splade [vector]; en/rust/e0502-mutable-borrow-while-shared [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts];  [vector];  [vector]; en/db/connection-pool-exhaustion [vector]; en/long/kubernetes-upgrade-log [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; en/windows/wsl2-localhost-forwarding [vector,trigram]; en/db/connection-pool-exhaustion [vector,trigram]; en/devtools/npm-install-and-lockfile [vector,trigram]; onecol-en.pdf · page:6 [vector,trigram] |
| t2-para | paraphrase | — | — | — | — | en/long/dns-outage-postmortem [vector,fts]; en/rust/future-not-send-mutexguard-await [vector,fts]; en/long/oncall-handbook [vector,fts]; en/rust/e0499-two-mutable-borrows [vector,fts]; en/ops/slo-burn-rate-alerts [vector] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; en/git/submodules-pitfalls [vector]; ru/devtools/local-build-cache [vector]; en/research/onnx-runtime-execution-providers [vector];  [vector] |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective [vector,fts]; ru/long/incident-disk-full-logs [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts];  [vector]; ru/research/fts5-tokenizatory [vector] |
| c1-exact | exact | 10 | — | — | — | ru/long/incident-disk-full-logs [vector]; ru/zam/hybrid-search-rrf [vector]; ru/zam/markdown-chunking [vector]; ru/long/tls-certificate-rotation [vector]; ru/zam/bge-m3-embeddings [vector] |
| c2-para | paraphrase | 10 | — | — | — | ru/long/checkout-load-testing-report [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector];  [vector];  [vector] |
| c3-cross | cross-lang | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/kubernetes-upgrade-log [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/ops/trusted-proxies [fts]; en/git/reflog-recovery [vector] |
| k1-exact | exact | 7 | — | — | — | ru/long/object-storage-backup-strategy [vector]; ru/long/checkout-load-testing-report [vector]; ru/zam/l0-l1-levels [vector];  [vector];  [vector] |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile [vector,fts];  [vector,fts]; en/long/observability-stack-design [vector,fts]; en/infra/nginx-limit-req [vector,fts];  [vector] |
| k2-exact | exact | 8 | — | — | — | en/long/dns-outage-postmortem [vector]; en/long/ml-model-serving-notes [vector]; en/long/kubernetes-upgrade-log [vector]; en/long/oncall-handbook [vector]; ru/zam/l0-l1-levels [vector] |
| k3-exact | exact | 6 | — | — | — |  [vector]; ru/git/clean-fdx [vector]; en/rust/tokio-spawn-blocking [vector]; en/git/interactive-rebase-autosquash [vector];  [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; en/linux/disk-full-but-df-shows-space [vector]; en/linux/oom-killer-dmesg [vector]; ru/ops/retries-backoff-jitter [vector]; en/rust/workspace-dependencies-and-profiles [vector] |
| b2-para | paraphrase | — | — | — | — | en/long/postgres-index-tuning-notes [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/ml-model-serving-notes [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts] |
| b3-cross | cross-lang | — | — | — | — | ru/long/tls-certificate-rotation [vector,fts]; ru/api/rate-limiting-token-bucket [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector,fts];  [vector] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; en/long/kubernetes-upgrade-log [vector]; en/long/feature-flag-platform [vector]; en/long/redis-cluster-migration [vector];  [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; en/long/oncall-handbook [vector]; ru/long/object-storage-backup-strategy [vector]; ru/long/tls-certificate-rotation [vector]; ru/ops/log-rotation [vector] |

## Документы

Сверка с допустимыми исходами манифеста: **21 из 21**.

| Файл | Роль | Допустимо | Состояние | Причина | Предупреждения | ocr_needed | Сверка | с до конца |
|---|---|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  |  | false | да | 12.2 |
| `files/onboarding-en.docx` | content | done | done |  |  | false | да | 12.2 |
| `files/inventory.xlsx` | content | done | done |  |  | false | да | 13.1 |
| `files/roadmap.pptx` | content | done | done |  |  | false | да | 12.0 |
| `files/onecol-en.pdf` | content | done | done |  |  | false | да | 11.9 |
| `files/twocol-en.pdf` | content | done | done |  |  | false | да | 11.9 |
| `files/cyrillic-ru.pdf` | content | done | done |  |  | false | да | 11.8 |
| `files/cjk-zh.pdf` | content | done | done |  |  | false | да | 11.8 |
| `files/book-500.pdf` | content | done | done |  |  | false | да | 11.7 |
| `files/scan.pdf` | scan | done+ocr | done |  |  | true | да | 11.6 |
| `hostile/zip-bomb.docx` | hostile | done+truncated: max_ratio / failed:parse_error | done |  | truncated: max_ratio | false | да | 11.5 |
| `hostile/xxe.docx` | hostile | failed:parse_error: xml | failed | parse_error: xml (word/document.xml) | parse_error: xml (word/document.xml) | false | да | 10.6 |
| `hostile/billion-laughs.xlsx` | hostile | failed:parse_error: xml | failed | parse_error: xml (xl/sharedStrings.xml) | parse_error: xml (xl/sharedStrings.xml) | false | да | 6.8 |
| `hostile/deep-nesting.docx` | hostile | done+truncated: max_xml_depth / failed:parse_error | done |  | truncated: max_ratio; truncated: max_xml_depth | false | да | 6.5 |
| `hostile/broken.pdf` | hostile | failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 6.3 |
| `hostile/truncated.pdf` | hostile | done / failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 6.3 |
| `hostile/js-action.pdf` | hostile | done | done |  |  | false | да | 6.2 |
| `hostile/deep-array.pdf` | hostile | done / failed:parse_error | done |  |  | false | да | 6.1 |
| `hostile/glyph-flood.pdf` | hostile | done+truncated: page_chars | done |  | truncated: page_chars | false | да | 7.0 |
| `hostile/huge-image.pdf` | hostile | done+ocr | done |  |  | true | да | 1.4 |
| `hostile/truncated.docx` | hostile | failed:parse_error: zip | failed | parse_error: zip | parse_error: zip | false | да | 1.3 |

Кэш извлечения `blob_extract`: 21 строк, zstd(JSON) 0.73 МБ, JSON 5.15 МБ (сжатие 7.1×).
