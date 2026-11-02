# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdfium)

Код — `251a72b+dirty.50f90daf`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 18 с, индексация после него 0 с. Сервер жив после набора: да.

Переопределения: `search.attach_prefetch=60`.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|
| **все** | 45 | 0.733 [0.600–0.867] | 0.697 [0.576–0.813] | 0.622 [0.489–0.756] | 0.848 [0.727–0.970] | 0.848 [0.727–0.970] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.030 [0.000–0.069] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 0.833 [0.500–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 0.978 [0.935–1.000] | 0.957 [0.870–1.000] | 0.957 [0.870–1.000] | 0.957 [0.870–1.000] |
| paraphrase | 11 | 0.636 [0.364–0.909] | 0.557 [0.299–0.788] | 0.545 [0.273–0.818] | 0.857 [0.571–1.000] | 0.857 [0.571–1.000] |
| docx | 12 | 0.833 [0.583–1.000] | 0.792 [0.542–1.000] | 0.750 [0.500–1.000] | 0.900 [0.700–1.000] | 0.900 [0.700–1.000] |
| pdf | 20 | 0.650 [0.450–0.850] | 0.637 [0.431–0.831] | 0.650 [0.450–0.850] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 0.766 [0.531–0.938] | 0.375 [0.125–0.750] | 0.429 [0.143–0.857] | 0.429 [0.143–0.857] |

Латентность запросов набора (клиент): p50 29.4 мс, p95 39.3 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/git/revert-vs-reset [vector,fts]; ru/windows/services-schtasks-robocopy [vector]; ru/ops/retries-backoff-jitter [vector]; ru/long/kafka-consumer-lag-analysis [vector] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/research/bm25-parametry [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/windows/file-locked-running-exe [vector,fts]; ru/long/ci-pipeline-retrospective [vector] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; inventory.xlsx · sheet:Поставки!A906:G925 [vector]; ru/ops/nginx-reverse-proxy-headers [vector]; ru/zam/hybrid-search-rrf [vector]; ru/git/revert-vs-reset [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A5797:G5822 [vector]; ru/long/catalog-search-relevance-study [vector]; ru/long/dev-environment-onboarding [vector]; ru/research/butstrep-intervaly [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; ru/long/checkout-load-testing-report [vector,fts]; ru/rust/arc-mutex-deadlock [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; cyrillic-ru.pdf · page:3 [vector] |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles [vector,fts]; book-500.pdf · page:37 [vector]; en/ops/trusted-proxies [fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector]; en/long/payment-webhooks-reliability [vector,fts] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector]; ru/ops/backups-restore-testing [fts]; ru/long/kafka-consumer-lag-analysis [vector] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; en/long/oncall-handbook [vector]; book-500.pdf · page:273 [vector]; en/long/observability-stack-design [vector]; en/long/ml-model-serving-notes [vector] |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/windows/services-schtasks-robocopy [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts]; ru/zam/architecture-overview [vector] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts];  [vector]; ru/ops/tls-expired-certificate [vector]; en/long/dns-outage-postmortem [vector];  [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; en/devtools/gha-matrix-strategy [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/long/redis-cluster-migration [vector,fts] |
| e4-para | paraphrase | 2 | para:34 | нет | нет | en/long/oncall-handbook [vector,fts]; onboarding-en.docx · para:34 [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/feature-flag-platform [vector] |
| x1-exact | exact | 2 | sheet:Поставки!A7590:G7608 | нет | нет | ru/ops/postmortem-blameless [vector]; inventory.xlsx · sheet:Поставки!A7590:G7608 [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/zam/config-layers-env [vector,fts];  [vector] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; ru/long/mobile-release-train [vector,fts,trigram]; ru/long/kafka-consumer-lag-analysis [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/long/catalog-search-relevance-study [vector,fts] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/research/kross-enkodery-rerank [vector]; ru/windows/services-schtasks-robocopy [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; en/long/oncall-handbook [vector]; en/git/force-with-lease [vector]; en/long/kubernetes-upgrade-log [vector]; en/devtools/gha-pull-request-target-secrets [vector] |
| x3-cross | cross-lang | 8 | — | — | — | ru/long/ci-pipeline-retrospective [vector,fts]; cyrillic-ru.pdf · page:3 [vector,fts]; regulations-ru.docx · para:69 [vector,fts]; ru/windows/services-schtasks-robocopy [vector]; ru/long/checkout-load-testing-report [vector] |
| x4-date | date | 1 | sheet:Поставки!A2780:G2798 | нет | нет | inventory.xlsx · sheet:Поставки!A2780:G2798 [vector,trigram]; ru/ops/timeouts [vector,fts]; ru/long/checkout-load-testing-report [vector]; ru/long/tls-certificate-rotation [vector];  [vector] |
| x5-date | date | 2 | sheet:Склад!A9694:G9720 | нет | нет | ru/long/postgres-major-upgrade-runbook [vector,fts]; inventory.xlsx · sheet:Склад!A9694:G9720 [vector]; cyrillic-ru.pdf · page:2 [vector,fts]; ru/long/personal-data-retention-policy [vector]; regulations-ru.docx · para:17 [vector,fts] |
| x6-date | date | 1 | sheet:Staff!A6272:G6294 | нет | нет | inventory.xlsx · sheet:Staff!A6272:G6294 [vector,fts]; en/long/oncall-handbook [vector]; en/long/observability-stack-design [vector]; en/long/feature-flag-platform [vector]; en/linux/systemd-unit-restart [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; en/long/redis-cluster-migration [vector]; en/ops/dns-ttl-migration [vector]; book-500.pdf · page:263 [vector]; en/long/oncall-handbook [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/ops/postmortem-blameless [vector]; ru/long/personal-data-retention-policy [vector] |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide [vector,fts]; en/devtools/playwright-flaky-e2e [vector,fts]; en/long/redis-cluster-migration [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; onboarding-en.docx · para:30 [vector,fts] |
| p3-para | paraphrase | — | — | — | — | en/long/api-versioning-guide [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/observability-stack-design [vector,fts]; en/research/chunking-strategies [vector,fts]; book-500.pdf · page:203 [vector] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; ru/ops/retries-backoff-jitter [vector]; ru/long/mobile-release-train [vector]; ru/long/dev-environment-onboarding [vector]; en/long/kubernetes-upgrade-log [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; en/zam/reranker-license [vector]; en/long/ml-model-serving-notes [vector]; twocol-en.pdf · page:5 [vector]; en/research/colbert-late-interaction [vector] |
| o2-para | paraphrase | 8 | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/postgres-index-tuning-notes [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/feature-flag-platform [vector]; en/long/dns-outage-postmortem [vector] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; en/long/api-versioning-guide [vector]; en/research/colbert-late-interaction [vector]; en/research/learned-sparse-splade [vector]; en/rust/e0502-mutable-borrow-while-shared [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; en/db/connection-pool-exhaustion [vector]; book-500.pdf · page:261 [vector]; en/long/kubernetes-upgrade-log [vector]; en/windows/powershell-pipeline-objects [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; onecol-en.pdf · page:6 [vector,trigram]; en/windows/wsl2-localhost-forwarding [vector,trigram]; en/db/connection-pool-exhaustion [vector,trigram]; en/devtools/npm-install-and-lockfile [vector,trigram] |
| t2-para | paraphrase | — | — | — | — | en/rust/future-not-send-mutexguard-await [vector,fts]; en/long/oncall-handbook [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/rust/e0499-two-mutable-borrows [vector,fts]; en/ops/slo-burn-rate-alerts [vector] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; en/git/submodules-pitfalls [vector]; ru/devtools/local-build-cache [vector]; en/research/onnx-runtime-execution-providers [vector];  [vector] |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective [vector,fts]; ru/long/incident-disk-full-logs [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts]; regulations-ru.docx · para:69 [vector]; ru/research/fts5-tokenizatory [vector] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; ru/long/incident-disk-full-logs [vector]; ru/zam/hybrid-search-rrf [vector]; ru/zam/markdown-chunking [vector]; inventory.xlsx · sheet:Поставки!A3971:G3989 [vector] |
| c2-para | paraphrase | 3 | page:5 | да | да | ru/long/checkout-load-testing-report [vector,fts]; ru/long/kafka-consumer-lag-analysis [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/tls-certificate-rotation [vector] |
| c3-cross | cross-lang | — | — | — | — | en/long/observability-stack-design [vector,fts]; en/long/kubernetes-upgrade-log [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/ops/trusted-proxies [fts]; book-500.pdf · page:217 [vector] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts]; ru/long/object-storage-backup-strategy [vector]; en/long/observability-stack-design [vector]; ru/long/checkout-load-testing-report [vector];  [vector] |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile [vector,fts]; en/long/observability-stack-design [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts]; en/long/feature-flag-platform [vector,fts]; en/long/postgres-index-tuning-notes [vector] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; en/long/dns-outage-postmortem [vector]; en/long/ml-model-serving-notes [vector]; en/long/kubernetes-upgrade-log [vector]; ru/long/ci-pipeline-retrospective [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; ru/git/clean-fdx [vector]; en/git/interactive-rebase-autosquash [vector]; en/long/oncall-handbook [vector]; regulations-ru.docx · para:6 [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; en/linux/disk-full-but-df-shows-space [vector]; en/linux/oom-killer-dmesg [vector]; ru/ops/retries-backoff-jitter [vector]; en/docker/multistage-build-layer-cache [vector] |
| b2-para | paraphrase | 6 | — | — | — | en/long/postgres-index-tuning-notes [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/ml-model-serving-notes [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/zam/windows-data-dir-path-limit [vector,fts] |
| b3-cross | cross-lang | 9 | — | — | — | cyrillic-ru.pdf · page:1 [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/research/bm25-parametry [vector,fts]; regulations-ru.docx · para:15 [vector,fts]; ru/ops/nginx-reverse-proxy-headers [vector,fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; en/long/kubernetes-upgrade-log [vector]; en/long/feature-flag-platform [vector]; en/long/redis-cluster-migration [vector];  [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; en/long/oncall-handbook [vector]; inventory.xlsx · sheet:Staff!A9067:G9089 [vector]; ru/long/object-storage-backup-strategy [vector]; ru/long/tls-certificate-rotation [vector] |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 7.4 |
| `files/onboarding-en.docx` | content | done | done |  | false | 7.2 |
| `files/inventory.xlsx` | content | done | done |  | false | 16.6 |
| `files/roadmap.pptx` | content | done | done |  | false | 6.8 |
| `files/onecol-en.pdf` | content | done | done |  | false | 6.7 |
| `files/twocol-en.pdf` | content | done | done |  | false | 6.6 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 6.5 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 6.3 |
| `files/book-500.pdf` | content | done | done |  | false | 7.1 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 6.9 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio \| failed | done |  | false | 2.5 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 2.4 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 2.3 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error \| truncated: max_xml_depth | done |  | false | 2.2 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 2.1 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) \| failed | failed | parse_error: pdf | false | 2.1 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.9 |
| `hostile/deep-array.pdf` | hostile | done \| failed: parse_error (не parse_crash) | done |  | false | 1.8 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 2.7 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom \| parse_crash | done |  | true | 2.3 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.72 МБ, JSON 5.15 МБ (сжатие 7.1×).
