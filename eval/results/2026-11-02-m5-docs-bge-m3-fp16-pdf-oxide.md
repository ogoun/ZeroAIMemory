# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdf_oxide)

Код — `3403134+dirty.ee3bb400`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 19 с, индексация после него 44 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.756 [0.622–0.867] | 0.978 [0.933–1.000] | 0.715 [0.598–0.836] | 0.667 [0.533–0.800] | 0.882 [0.765–0.971] | 0.882 [0.765–0.971] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.875 [0.625–1.000] | 0.042 [0.000–0.084] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.833 [0.500–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.727 [0.455–1.000] | 1.000 [1.000–1.000] | 0.578 [0.336–0.803] | 0.636 [0.364–0.909] | 0.875 [0.625–1.000] | 0.875 [0.625–1.000] |
| docx | 12 | 0.833 [0.583–1.000] | 1.000 [1.000–1.000] | 0.800 [0.575–1.000] | 0.750 [0.500–1.000] | 0.900 [0.700–1.000] | 0.900 [0.700–1.000] |
| pdf | 20 | 0.700 [0.500–0.900] | 0.950 [0.850–1.000] | 0.643 [0.440–0.838] | 0.700 [0.500–0.900] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 0.622 [0.222–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 1.000 [1.000–1.000] | 0.828 [0.594–1.000] | 0.500 [0.125–0.875] | 0.571 [0.143–1.000] | 0.571 [0.143–1.000] |

Латентность запросов набора (клиент): p50 31.2 мс, p95 37.8 мс.
Основной набор (326 запросов, второй проход): без вложений p50 12.6 / p95 18.7 мс, с вложениями p50 14.5 / p95 21.9 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/git/revert-vs-reset [vector,fts]; ru/windows/services-schtasks-robocopy [vector]; ru/ops/retries-backoff-jitter [vector]; ru/long/kafka-consumer-lag-analysis [vector] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/long/ci-pipeline-retrospective [vector]; ru/long/incident-disk-full-logs [vector] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; inventory.xlsx · sheet:Поставки!A906:G925 [vector]; ru/ops/nginx-reverse-proxy-headers [vector]; ru/zam/hybrid-search-rrf [vector]; ru/git/revert-vs-reset [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A5797:G5822 [vector]; ru/long/catalog-search-relevance-study [vector]; ru/long/dev-environment-onboarding [vector]; ru/research/butstrep-intervaly [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/long/checkout-load-testing-report [vector,fts]; ru/rust/arc-mutex-deadlock [vector,fts]; cyrillic-ru.pdf · page:3 [vector] |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles [vector,fts]; book-500.pdf · page:37 [vector]; en/long/payment-webhooks-reliability [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts]; en/ops/trusted-proxies [fts] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector]; ru/ops/backups-restore-testing [fts]; ru/long/kafka-consumer-lag-analysis [vector] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; en/long/oncall-handbook [vector]; book-500.pdf · page:273 [vector]; en/long/observability-stack-design [vector]; en/long/ml-model-serving-notes [vector] |
| e1-cross | cross-lang | 10 | — | — | — | ru/long/dev-environment-onboarding [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/windows/services-schtasks-robocopy [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/zam/architecture-overview [vector] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts];  [vector]; ru/ops/tls-expired-certificate [vector]; en/long/dns-outage-postmortem [vector];  [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; en/devtools/gha-matrix-strategy [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/long/redis-cluster-migration [vector,fts] |
| e4-para | paraphrase | 2 | para:34 | нет | нет | en/long/oncall-handbook [vector,fts]; onboarding-en.docx · para:34 [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts]; inventory.xlsx · sheet:Staff!A7523:G7544 [vector] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,fts]; ru/ops/postmortem-blameless [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/zam/config-layers-env [vector,fts];  [vector] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; ru/long/mobile-release-train [vector,fts,trigram]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/long/catalog-search-relevance-study [vector,fts] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/research/kross-enkodery-rerank [vector]; ru/windows/services-schtasks-robocopy [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; en/long/oncall-handbook [vector]; en/git/force-with-lease [vector]; en/long/kubernetes-upgrade-log [vector]; en/devtools/gha-pull-request-target-secrets [vector] |
| x3-cross | cross-lang | 8 | — | — | — | cyrillic-ru.pdf · page:3 [vector,fts]; ru/long/ci-pipeline-retrospective [vector,fts]; regulations-ru.docx · para:69 [vector,fts]; ru/windows/services-schtasks-robocopy [vector]; ru/long/checkout-load-testing-report [vector] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram]; ru/ops/timeouts [vector,fts]; ru/long/checkout-load-testing-report [vector]; ru/long/tls-certificate-rotation [vector];  [vector] |
| x5-date | date | 1 | sheet:Staff!A3558:G3579 | нет | нет | inventory.xlsx · sheet:Staff!A3558:G3579 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector,fts]; ru/long/personal-data-retention-policy [vector]; en/git/sparse-checkout-partial-clone [vector]; ru/long/checkout-load-testing-report [vector] |
| x6-date | date | 2 | sheet:Staff!A8330:G8351 | нет | нет | en/long/oncall-handbook [vector]; inventory.xlsx · sheet:Staff!A8330:G8351 [vector]; en/long/observability-stack-design [vector]; en/long/feature-flag-platform [vector]; en/linux/systemd-unit-restart [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; en/long/redis-cluster-migration [vector]; en/ops/dns-ttl-migration [vector]; en/long/oncall-handbook [vector]; book-500.pdf · page:273 [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/ops/postmortem-blameless [vector]; ru/long/personal-data-retention-policy [vector] |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts]; en/long/redis-cluster-migration [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; onboarding-en.docx · para:30 [vector,fts] |
| p3-para | paraphrase | 9 | — | — | — | en/long/api-versioning-guide [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/observability-stack-design [vector,fts]; en/research/chunking-strategies [vector,fts]; book-500.pdf · page:203 [vector] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; ru/ops/retries-backoff-jitter [vector]; ru/long/mobile-release-train [vector]; ru/long/dev-environment-onboarding [vector]; en/long/kubernetes-upgrade-log [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; en/zam/reranker-license [vector]; twocol-en.pdf · page:5 [vector]; en/long/ml-model-serving-notes [vector]; en/research/colbert-late-interaction [vector] |
| o2-para | paraphrase | 4 | page:7 | да | да | en/long/observability-stack-design [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/postgres-index-tuning-notes [vector,fts]; onecol-en.pdf · page:7 [vector]; en/long/frontend-performance-audit [vector,fts] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; en/long/api-versioning-guide [vector]; en/research/colbert-late-interaction [vector]; en/research/learned-sparse-splade [vector]; en/rust/e0502-mutable-borrow-while-shared [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; en/db/connection-pool-exhaustion [vector]; book-500.pdf · page:261 [vector]; en/long/kubernetes-upgrade-log [vector]; en/windows/powershell-pipeline-objects [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; onecol-en.pdf · page:6 [vector,trigram]; en/windows/wsl2-localhost-forwarding [vector,trigram]; en/db/connection-pool-exhaustion [vector,trigram]; en/devtools/npm-install-and-lockfile [vector,trigram] |
| t2-para | paraphrase | — | — | — | — | en/rust/future-not-send-mutexguard-await [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/ops/slo-burn-rate-alerts [vector]; en/rust/e0499-two-mutable-borrows [vector,fts] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; en/git/submodules-pitfalls [vector]; ru/devtools/local-build-cache [vector]; en/research/onnx-runtime-execution-providers [vector];  [vector] |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective [vector,fts]; ru/long/incident-disk-full-logs [vector,fts]; regulations-ru.docx · para:69 [vector]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/research/fts5-tokenizatory [vector] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; ru/long/incident-disk-full-logs [vector]; ru/zam/hybrid-search-rrf [vector]; ru/zam/markdown-chunking [vector]; ru/rust/ffi-cstring-repr-c [vector] |
| c2-para | paraphrase | 3 | page:5 | да | да | ru/long/checkout-load-testing-report [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/tls-certificate-rotation [vector] |
| c3-cross | cross-lang | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/kubernetes-upgrade-log [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/ops/trusted-proxies [fts]; book-500.pdf · page:217 [vector] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts]; ru/long/object-storage-backup-strategy [vector]; en/long/observability-stack-design [vector]; ru/long/checkout-load-testing-report [vector]; ru/zam/l0-l1-levels [vector] |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile [vector,fts]; en/long/observability-stack-design [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/postgres-index-tuning-notes [vector] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; en/long/dns-outage-postmortem [vector]; en/long/ml-model-serving-notes [vector]; en/long/kubernetes-upgrade-log [vector]; ru/long/ci-pipeline-retrospective [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; ru/git/clean-fdx [vector]; inventory.xlsx · sheet:Staff!A338:G359 [vector]; en/long/oncall-handbook [vector]; en/git/interactive-rebase-autosquash [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; en/linux/disk-full-but-df-shows-space [vector]; en/linux/oom-killer-dmesg [vector]; ru/ops/retries-backoff-jitter [vector]; en/docker/multistage-build-layer-cache [vector] |
| b2-para | paraphrase | 6 | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/postgres-index-tuning-notes [vector,fts]; en/long/ml-model-serving-notes [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts] |
| b3-cross | cross-lang | 9 | — | — | — | cyrillic-ru.pdf · page:5 [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/research/bm25-parametry [vector,fts]; regulations-ru.docx · para:15 [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector,fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; en/long/kubernetes-upgrade-log [vector]; en/long/feature-flag-platform [vector]; en/long/redis-cluster-migration [vector];  [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; en/long/oncall-handbook [vector]; ru/long/object-storage-backup-strategy [vector]; ru/long/tls-certificate-rotation [vector]; inventory.xlsx · sheet:Staff!A9067:G9089 [vector] |

## Документы

Сверка с допустимыми исходами манифеста: **20 из 21**.

| Файл | Роль | Допустимо | Состояние | Причина | Предупреждения | ocr_needed | Сверка | с до конца |
|---|---|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  |  | false | да | 3.8 |
| `files/onboarding-en.docx` | content | done | done |  |  | false | да | 3.8 |
| `files/inventory.xlsx` | content | done | done |  |  | false | да | 15.7 |
| `files/roadmap.pptx` | content | done | done |  |  | false | да | 3.6 |
| `files/onecol-en.pdf` | content | done | done |  |  | false | да | 3.6 |
| `files/twocol-en.pdf` | content | done | done |  |  | false | да | 3.5 |
| `files/cyrillic-ru.pdf` | content | done | done |  |  | false | да | 3.5 |
| `files/cjk-zh.pdf` | content | done | done |  |  | false | да | 3.2 |
| `files/book-500.pdf` | content | done | done |  |  | false | да | 2.9 |
| `files/scan.pdf` | scan | done+ocr | done |  |  | false | **нет** | 2.9 |
| `hostile/zip-bomb.docx` | hostile | done+truncated: max_ratio / failed:parse_error | done |  | truncated: max_ratio | false | да | 2.6 |
| `hostile/xxe.docx` | hostile | failed:parse_error: xml | failed | parse_error: xml (word/document.xml) | parse_error: xml (word/document.xml) | false | да | 2.5 |
| `hostile/billion-laughs.xlsx` | hostile | failed:parse_error: xml | failed | parse_error: xml (xl/sharedStrings.xml) | parse_error: xml (xl/sharedStrings.xml) | false | да | 2.4 |
| `hostile/deep-nesting.docx` | hostile | done+truncated: max_xml_depth / failed:parse_error | done |  | truncated: max_ratio; truncated: max_xml_depth | false | да | 2.3 |
| `hostile/broken.pdf` | hostile | failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 2.2 |
| `hostile/truncated.pdf` | hostile | done / failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 2.1 |
| `hostile/js-action.pdf` | hostile | done | done |  |  | false | да | 2.0 |
| `hostile/deep-array.pdf` | hostile | done / failed:parse_error | done |  |  | false | да | 1.8 |
| `hostile/glyph-flood.pdf` | hostile | done+truncated: page_chars | done |  | truncated: page_chars | false | да | 2.9 |
| `hostile/huge-image.pdf` | hostile | done+ocr | done |  |  | true | да | 1.6 |
| `hostile/truncated.docx` | hostile | failed:parse_error: zip | failed | parse_error: zip | parse_error: zip | false | да | 1.3 |

Кэш извлечения `blob_extract`: 21 строк, zstd(JSON) 0.72 МБ, JSON 5.13 МБ (сжатие 7.1×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 41 | 6 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 67 | 6 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 59 | 5 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 66 | 7 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 29 | 51 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 22 | 1 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1054 | 2 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 4 | 1 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 2 | 1 | превью не построено: parse_error: pdf |
