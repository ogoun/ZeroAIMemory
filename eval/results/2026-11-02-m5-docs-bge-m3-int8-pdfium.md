# Eval документов: hybrid bge-m3-int8 (cpu, PDF — pdfium)

Код — `251a72b+dirty.3fd008d5`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 18 с, индексация после него 1348 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.733 [0.600–0.867] | 0.978 [0.933–1.000] | 0.691 [0.568–0.814] | 0.644 [0.489–0.778] | 0.879 [0.758–0.970] | 0.879 [0.758–0.970] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.875 [0.625–1.000] | 0.039 [0.013–0.078] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.583 [0.250–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.636 [0.364–0.909] | 1.000 [1.000–1.000] | 0.547 [0.285–0.788] | 0.545 [0.273–0.818] | 0.857 [0.571–1.000] | 0.857 [0.571–1.000] |
| docx | 12 | 0.833 [0.583–1.000] | 1.000 [1.000–1.000] | 0.771 [0.521–1.000] | 0.750 [0.500–1.000] | 0.900 [0.700–1.000] | 0.900 [0.700–1.000] |
| pdf | 20 | 0.650 [0.450–0.850] | 0.950 [0.850–1.000] | 0.644 [0.441–0.833] | 0.650 [0.450–0.850] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 0.620 [0.220–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 1.000 [1.000–1.000] | 0.731 [0.475–0.938] | 0.500 [0.125–0.875] | 0.571 [0.143–1.000] | 0.571 [0.143–1.000] |

Латентность запросов набора (клиент): p50 49.7 мс, p95 60.1 мс.
Основной набор (326 запросов, второй проход): без вложений p50 12.3 / p95 18.0 мс, с вложениями p50 15.6 / p95 24.1 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/git/revert-vs-reset [vector,fts]; ru/windows/services-schtasks-robocopy [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/ops/retries-backoff-jitter [vector] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/long/ci-pipeline-retrospective [vector]; ru/long/incident-disk-full-logs [vector] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector]; ru/zam/hybrid-search-rrf [vector]; inventory.xlsx · sheet:Поставки!A906:G925 [vector]; ru/git/revert-vs-reset [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A5797:G5822 [vector]; ru/long/catalog-search-relevance-study [vector]; ru/long/dev-environment-onboarding [vector]; ru/research/butstrep-intervaly [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; ru/long/checkout-load-testing-report [vector,fts]; ru/rust/arc-mutex-deadlock [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; cyrillic-ru.pdf · page:3 [vector] |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles [vector,fts]; book-500.pdf · page:37 [vector]; en/long/payment-webhooks-reliability [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts]; en/ops/trusted-proxies [fts] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; ru/zam/qdrant-managed-process [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector]; ru/ops/backups-restore-testing [fts] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; en/long/oncall-handbook [vector]; book-500.pdf · page:175 [vector]; en/zam/answer-cache-protocol [vector]; en/long/ml-model-serving-notes [vector] |
| e1-cross | cross-lang | — | — | — | — | ru/long/tls-certificate-rotation [vector,fts]; ru/long/dev-environment-onboarding [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; ru/zam/architecture-overview [vector] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts];  [vector]; ru/ops/tls-expired-certificate [vector]; en/ops/dns-ttl-migration [vector]; en/long/dns-outage-postmortem [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; en/devtools/gha-matrix-strategy [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/long/redis-cluster-migration [vector,fts]; en/long/oncall-handbook [vector,fts] |
| e4-para | paraphrase | 4 | para:34 | нет | нет | en/long/oncall-handbook [vector,fts]; en/long/observability-stack-design [vector,fts]; inventory.xlsx · sheet:Staff!A9472:G9494 [vector]; onboarding-en.docx · para:34 [vector,fts]; en/long/api-versioning-guide [vector,fts] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,fts]; ru/ops/postmortem-blameless [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/long/checkout-load-testing-report [vector] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; ru/long/tls-certificate-rotation [vector,fts]; ru/long/mobile-release-train [vector,fts,trigram]; ru/long/ci-pipeline-retrospective [vector,fts]; ru/long/incident-disk-full-logs [vector,fts] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector]; ru/research/kross-enkodery-rerank [vector]; ru/windows/services-schtasks-robocopy [vector]; ru/long/kafka-consumer-lag-analysis [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; en/long/oncall-handbook [vector]; en/git/force-with-lease [vector]; en/long/kubernetes-upgrade-log [vector]; en/devtools/gha-pull-request-target-secrets [vector] |
| x3-cross | cross-lang | 10 | — | — | — | regulations-ru.docx · para:69 [vector,fts]; cyrillic-ru.pdf · page:3 [vector,fts]; ru/long/ci-pipeline-retrospective [vector,fts]; ru/devtools/flaky-tests-quarantine [vector,fts]; ru/windows/services-schtasks-robocopy [vector] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram]; ru/ops/timeouts [vector,fts]; ru/long/checkout-load-testing-report [vector]; ru/long/personal-data-retention-policy [vector];  [vector] |
| x5-date | date | 4 | sheet:Staff!A540:G562 | нет | нет | ru/long/postgres-major-upgrade-runbook [vector,fts]; ru/long/object-storage-backup-strategy [vector]; ru/long/personal-data-retention-policy [vector]; inventory.xlsx · sheet:Staff!A540:G562 [vector]; en/git/sparse-checkout-partial-clone [vector] |
| x6-date | date | 2 | sheet:Staff!A8284:G8306 | нет | нет | en/long/oncall-handbook [vector]; inventory.xlsx · sheet:Staff!A8284:G8306 [vector]; en/long/observability-stack-design [vector]; en/long/feature-flag-platform [vector];  [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; en/long/redis-cluster-migration [vector]; en/ops/dns-ttl-migration [vector]; book-500.pdf · page:363 [vector]; en/long/oncall-handbook [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/ops/postmortem-blameless [vector]; ru/long/incident-disk-full-logs [vector] |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide [vector,fts]; en/long/redis-cluster-migration [vector,fts]; onboarding-en.docx · para:30 [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts]; en/long/ml-model-serving-notes [vector] |
| p3-para | paraphrase | 10 | — | — | — | en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/ml-model-serving-notes [vector,fts]; en/research/chunking-strategies [vector,fts] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; ru/ops/retries-backoff-jitter [vector]; ru/long/mobile-release-train [vector]; ru/long/incident-disk-full-logs [vector]; ru/long/dev-environment-onboarding [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; en/zam/reranker-license [vector]; en/long/ml-model-serving-notes [vector];  [vector]; twocol-en.pdf · page:5 [vector] |
| o2-para | paraphrase | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/git/bisect-run [vector,fts]; en/long/frontend-performance-audit [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/postgres-index-tuning-notes [vector,fts] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; en/research/colbert-late-interaction [vector]; en/long/api-versioning-guide [vector]; en/research/learned-sparse-splade [vector]; onboarding-en.docx · para:11 [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; en/db/connection-pool-exhaustion [vector]; book-500.pdf · page:261 [vector]; en/windows/powershell-pipeline-objects [vector]; en/long/kubernetes-upgrade-log [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; onecol-en.pdf · page:6 [vector,trigram]; en/windows/wsl2-localhost-forwarding [vector,trigram]; en/db/connection-pool-exhaustion [vector,trigram]; en/devtools/npm-install-and-lockfile [vector,trigram] |
| t2-para | paraphrase | — | — | — | — | en/rust/future-not-send-mutexguard-await [vector,fts]; en/long/payment-webhooks-reliability [vector,fts]; en/ops/slo-burn-rate-alerts [vector]; en/rust/e0499-two-mutable-borrows [vector,fts]; en/long/dns-outage-postmortem [vector,fts] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; en/git/submodules-pitfalls [vector]; ru/devtools/local-build-cache [vector];  [vector]; en/research/onnx-runtime-execution-providers [vector] |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective [vector,fts]; ru/ops/postmortem-blameless [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/research/fts5-tokenizatory [vector]; ru/git/clean-fdx [vector,fts] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; inventory.xlsx · sheet:Поставки!A6616:G6634 [vector]; ru/zam/hybrid-search-rrf [vector]; ru/long/incident-disk-full-logs [vector];  [vector] |
| c2-para | paraphrase | 2 | page:5 | да | да | ru/long/checkout-load-testing-report [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/tls-certificate-rotation [vector]; inventory.xlsx · sheet:Поставки!A7649:G7668 [vector] |
| c3-cross | cross-lang | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/kubernetes-upgrade-log [vector,fts]; en/ops/trusted-proxies [fts]; en/git/reflog-recovery [vector]; book-500.pdf · page:273 [vector] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts];  [vector]; ru/long/checkout-load-testing-report [vector]; en/ops/slo-burn-rate-alerts [vector]; ru/long/object-storage-backup-strategy [vector] |
| k1-cross | cross-lang | 10 | — | — | — | en/devtools/npm-install-and-lockfile [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/postgres-index-tuning-notes [vector] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; en/ops/slo-burn-rate-alerts [vector]; en/long/dns-outage-postmortem [vector]; ru/long/personal-data-retention-policy [vector]; ru/long/object-storage-backup-strategy [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; ru/git/clean-fdx [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/windows/services-schtasks-robocopy [vector]; ru/long/personal-data-retention-policy [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; en/linux/disk-full-but-df-shows-space [vector]; twocol-en.pdf · page:4 [vector]; en/linux/oom-killer-dmesg [vector]; ru/ops/retries-backoff-jitter [vector] |
| b2-para | paraphrase | 6 | — | — | — | en/long/postgres-index-tuning-notes [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts]; en/long/payment-webhooks-reliability [vector,fts] |
| b3-cross | cross-lang | 9 | — | — | — | ru/long/tls-certificate-rotation [vector,fts]; cyrillic-ru.pdf · page:1 [vector,fts]; regulations-ru.docx · para:17 [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector,fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; en/long/kubernetes-upgrade-log [vector]; en/long/feature-flag-platform [vector]; roadmap.pptx · slide:17 [vector];  [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; en/long/oncall-handbook [vector]; ru/long/object-storage-backup-strategy [vector]; ru/long/tls-certificate-rotation [vector]; ru/ops/log-rotation [vector] |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 2.1 |
| `files/onboarding-en.docx` | content | done | done |  | false | 2.0 |
| `files/inventory.xlsx` | content | done | done |  | false | 17.5 |
| `files/roadmap.pptx` | content | done | done |  | false | 1.9 |
| `files/onecol-en.pdf` | content | done | done |  | false | 1.8 |
| `files/twocol-en.pdf` | content | done | done |  | false | 1.8 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 1.7 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 1.7 |
| `files/book-500.pdf` | content | done | done |  | false | 2.6 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 2.5 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio \| failed | done |  | false | 2.3 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 1.9 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 1.9 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error \| truncated: max_xml_depth | done |  | false | 1.8 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 1.7 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) \| failed | failed | parse_error: pdf | false | 1.6 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.6 |
| `hostile/deep-array.pdf` | hostile | done \| failed: parse_error (не parse_crash) | done |  | false | 1.8 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 3.0 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom \| parse_crash | done |  | true | 2.8 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.73 МБ, JSON 5.15 МБ (сжатие 7.1×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 56 | 6 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 66 | 5 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 61 | 3 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 64 | 7 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 31 | 30 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 22 | 1 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1079 | 2 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 4 | 1 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 2 | 1 | превью не построено: parse_error: pdf |
