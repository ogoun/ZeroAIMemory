# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdfium)

Код — `251a72b+dirty.c354a441`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 19 с, индексация после него 53 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|
| **все** | 45 | 0.733 [0.600–0.867] | 0.691 [0.570–0.815] | 0.644 [0.489–0.778] | 0.879 [0.758–0.970] | 0.879 [0.758–0.970] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.028 [0.000–0.063] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 0.583 [0.250–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.636 [0.364–0.909] | 0.555 [0.297–0.788] | 0.545 [0.273–0.818] | 0.857 [0.571–1.000] | 0.857 [0.571–1.000] |
| docx | 12 | 0.833 [0.583–1.000] | 0.792 [0.542–1.000] | 0.750 [0.500–1.000] | 0.900 [0.700–1.000] | 0.900 [0.700–1.000] |
| pdf | 20 | 0.650 [0.450–0.850] | 0.630 [0.425–0.825] | 0.650 [0.450–0.850] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 0.620 [0.220–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 0.734 [0.484–0.938] | 0.500 [0.125–0.875] | 0.571 [0.143–1.000] | 0.571 [0.143–1.000] |

Латентность запросов набора (клиент): p50 40.3 мс, p95 44.7 мс.
Основной набор (326 запросов, второй проход): без вложений p50 18.4 / p95 26.0 мс, с вложениями p50 19.3 / p95 27.1 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9; ru/git/revert-vs-reset; ru/windows/services-schtasks-robocopy; ru/ops/retries-backoff-jitter; ru/long/kafka-consumer-lag-analysis |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9; ru/research/bm25-parametry; ru/long/tls-certificate-rotation; ru/windows/file-locked-running-exe; ru/long/ci-pipeline-retrospective |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17; ru/ops/nginx-reverse-proxy-headers; ru/zam/hybrid-search-rrf; ru/git/revert-vs-reset; inventory.xlsx · sheet:Поставки!A4235:G4254 |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43; ru/long/catalog-search-relevance-study; ru/long/dev-environment-onboarding; ru/research/butstrep-intervaly; inventory.xlsx · sheet:Склад!A1112:G1140 |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47; ru/long/checkout-load-testing-report; ru/rust/arc-mutex-deadlock; ru/long/object-storage-backup-strategy; cyrillic-ru.pdf · page:3 |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles; book-500.pdf · page:37; en/long/payment-webhooks-reliability; en/devtools/playwright-flaky-e2e; en/long/api-versioning-guide |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61; ru/long/object-storage-backup-strategy; inventory.xlsx · sheet:Поставки!A5126:G5146; ru/rust/cargo-test-parallel-flaky-proptest; ru/ops/backups-restore-testing |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6; en/long/oncall-handbook; book-500.pdf · page:273; en/long/observability-stack-design; en/long/ml-model-serving-notes |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding; ru/long/tls-certificate-rotation; ru/windows/services-schtasks-robocopy; ru/long/object-storage-backup-strategy; ru/zam/architecture-overview |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11; ; ru/ops/tls-expired-certificate; en/long/dns-outage-postmortem; en/zam/answer-cache-protocol |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30; en/devtools/gha-matrix-strategy; en/long/feature-flag-platform; en/long/dns-outage-postmortem; en/long/redis-cluster-migration |
| e4-para | paraphrase | 2 | para:34 | нет | нет | en/long/oncall-handbook; onboarding-en.docx · para:34; en/long/api-versioning-guide; en/long/observability-stack-design; inventory.xlsx · sheet:Staff!A7029:G7052 |
| x1-exact | exact | 1 | sheet:Поставки!A1231:G1250 | да | да | inventory.xlsx · sheet:Поставки!A1231:G1250; ru/ops/postmortem-blameless; ru/long/kafka-consumer-lag-analysis; ru/zam/config-layers-env;  |
| x1-para | paraphrase | 1 | sheet:Поставки!A1231:G1250 | да | да | inventory.xlsx · sheet:Поставки!A1231:G1250; ru/long/mobile-release-train; ru/long/kafka-consumer-lag-analysis; ru/long/tls-certificate-rotation; ru/long/catalog-search-relevance-study |
| x2-exact | exact | 1 | sheet:Склад!A7775:G7801 | да | да | inventory.xlsx · sheet:Склад!A7775:G7801; ru/long/postgres-major-upgrade-runbook; ru/long/kafka-consumer-lag-analysis; ru/research/kross-enkodery-rerank; ru/windows/services-schtasks-robocopy |
| x3-exact | exact | 1 | sheet:Staff!A5009:G5032 | да | да | inventory.xlsx · sheet:Staff!A5009:G5032; en/long/oncall-handbook; en/git/force-with-lease; en/long/kubernetes-upgrade-log; en/devtools/gha-pull-request-target-secrets |
| x3-cross | cross-lang | 8 | — | — | — | ru/long/ci-pipeline-retrospective; cyrillic-ru.pdf · page:3; regulations-ru.docx · para:69; ru/windows/services-schtasks-robocopy; ru/long/checkout-load-testing-report |
| x4-date | date | 1 | sheet:Поставки!A6866:G6885 | нет | нет | inventory.xlsx · sheet:Поставки!A6866:G6885; ru/ops/timeouts; en/rust/cargo-feature-unification-resolver; ru/long/checkout-load-testing-report; ru/long/tls-certificate-rotation |
| x5-date | date | 4 | sheet:Склад!A8775:G8803 | нет | нет | cyrillic-ru.pdf · page:2; ru/long/postgres-major-upgrade-runbook; ru/long/tls-certificate-rotation; inventory.xlsx · sheet:Склад!A8775:G8803; ru/long/personal-data-retention-policy |
| x6-date | date | 2 | sheet:Staff!A8730:G8752 | нет | нет | en/long/oncall-handbook; inventory.xlsx · sheet:Staff!A8730:G8752; en/long/observability-stack-design; en/long/feature-flag-platform; en/linux/systemd-unit-restart |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4; en/long/redis-cluster-migration; en/ops/dns-ttl-migration; en/long/oncall-handbook; book-500.pdf · page:273 |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11; ; ru/long/ci-pipeline-retrospective; ru/ops/postmortem-blameless; ru/long/personal-data-retention-policy |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide; en/devtools/playwright-flaky-e2e; en/long/redis-cluster-migration; en/long/dns-outage-postmortem; onboarding-en.docx · para:30 |
| p3-para | paraphrase | 10 | — | — | — | en/long/api-versioning-guide; en/long/oncall-handbook; en/long/observability-stack-design; en/research/chunking-strategies; book-500.pdf · page:203 |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9; ru/ops/retries-backoff-jitter; ru/long/mobile-release-train; ru/long/dev-environment-onboarding; en/long/kubernetes-upgrade-log |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3; en/zam/reranker-license; en/long/ml-model-serving-notes; en/research/colbert-late-interaction;  |
| o2-para | paraphrase | — | — | — | — | en/long/observability-stack-design; en/long/oncall-handbook; en/long/postgres-index-tuning-notes; en/long/frontend-performance-audit; en/long/feature-flag-platform |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9; en/long/api-versioning-guide; en/research/colbert-late-interaction; en/research/learned-sparse-splade; en/rust/e0502-mutable-borrow-while-shared |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6; en/db/connection-pool-exhaustion; book-500.pdf · page:261; en/long/kubernetes-upgrade-log; en/windows/powershell-pipeline-objects |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2; onecol-en.pdf · page:6; en/windows/wsl2-localhost-forwarding; en/db/connection-pool-exhaustion; en/devtools/npm-install-and-lockfile |
| t2-para | paraphrase | — | — | — | — | en/rust/future-not-send-mutexguard-await; en/long/oncall-handbook; en/ops/slo-burn-rate-alerts; en/long/dns-outage-postmortem; en/rust/e0499-two-mutable-borrows |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5; en/git/submodules-pitfalls; ru/devtools/local-build-cache; en/research/onnx-runtime-execution-providers;  |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective; ru/long/incident-disk-full-logs; ru/long/kafka-consumer-lag-analysis; ru/research/fts5-tokenizatory; regulations-ru.docx · para:69 |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2; ru/long/incident-disk-full-logs; ru/zam/hybrid-search-rrf; ru/zam/markdown-chunking; ru/rust/ffi-cstring-repr-c |
| c2-para | paraphrase | 3 | page:5 | да | да | ru/long/checkout-load-testing-report; ru/long/kafka-consumer-lag-analysis; cyrillic-ru.pdf · page:5; ru/long/postgres-major-upgrade-runbook; ru/long/tls-certificate-rotation |
| c3-cross | cross-lang | — | — | — | — | en/long/observability-stack-design; en/long/kubernetes-upgrade-log; en/long/dns-outage-postmortem; en/ops/trusted-proxies; book-500.pdf · page:217 |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2; ru/long/object-storage-backup-strategy; en/long/observability-stack-design; ru/long/checkout-load-testing-report;  |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile; en/long/observability-stack-design; en/zam/windows-data-dir-path-limit; en/long/postgres-index-tuning-notes; en/long/feature-flag-platform |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3; en/long/dns-outage-postmortem; en/long/ml-model-serving-notes; en/long/kubernetes-upgrade-log; ru/long/ci-pipeline-retrospective |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4; ru/git/clean-fdx; regulations-ru.docx · para:6; en/git/interactive-rebase-autosquash; en/long/oncall-handbook |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37; en/linux/disk-full-but-df-shows-space; en/linux/oom-killer-dmesg; ru/ops/retries-backoff-jitter; en/docker/multistage-build-layer-cache |
| b2-para | paraphrase | 6 | — | — | — | en/long/observability-stack-design; en/long/postgres-index-tuning-notes; en/long/ml-model-serving-notes; en/long/dns-outage-postmortem; en/zam/windows-data-dir-path-limit |
| b3-cross | cross-lang | 10 | — | — | — | cyrillic-ru.pdf · page:1; ru/long/tls-certificate-rotation; regulations-ru.docx · para:15; ru/research/bm25-parametry;  |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498; en/long/kubernetes-upgrade-log; en/long/feature-flag-platform; en/long/redis-cluster-migration;  |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1; en/long/oncall-handbook; ru/long/object-storage-backup-strategy; ru/long/tls-certificate-rotation; ru/ops/log-rotation |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 2.9 |
| `files/onboarding-en.docx` | content | done | done |  | false | 2.7 |
| `files/inventory.xlsx` | content | done | done |  | false | 18.4 |
| `files/roadmap.pptx` | content | done | done |  | false | 2.5 |
| `files/onecol-en.pdf` | content | done | done |  | false | 2.5 |
| `files/twocol-en.pdf` | content | done | done |  | false | 2.4 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 2.3 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 2.3 |
| `files/book-500.pdf` | content | done | done |  | false | 3.2 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 2.9 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio | failed | done |  | false | 2.6 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 2.1 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 2.1 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error | truncated: max_xml_depth | done |  | false | 2.0 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 1.9 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) | failed | failed | parse_error: pdf | false | 1.7 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.6 |
| `hostile/deep-array.pdf` | hostile | done | failed: parse_error (не parse_crash) | done |  | false | 1.5 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 4.6 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom | parse_crash | done |  | true | 4.5 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.71 МБ, JSON 4.97 МБ (сжатие 7.0×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 53 | 9 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 73 | 5 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 78 | 5 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 61 | 5 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 4 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 34 | 3 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 27 | 2 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1397 | 2 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 4 | 3 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
