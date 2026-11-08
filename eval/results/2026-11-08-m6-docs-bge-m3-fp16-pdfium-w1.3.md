# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdfium)

Код — `6eaf2ba`, 2026-11-08; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 14 с, индексация после него 5 с. Сервер жив после набора: да.

Переопределения: `search.recency_half_life_days=0`, `search.w_attach=1.3`.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.867 [0.756–0.956] | 0.978 [0.933–1.000] | 0.800 [0.696–0.896] | 0.733 [0.600–0.844] | 0.846 [0.743–0.949] | 0.846 [0.743–0.949] |
| cross-lang | 8 | 0.500 [0.125–0.875] | 0.875 [0.625–1.000] | 0.188 [0.112–0.247] | 0.375 [0.125–0.750] | 0.750 [0.250–1.000] | 0.750 [0.250–1.000] |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.818 [0.545–1.000] | 1.000 [1.000–1.000] | 0.773 [0.530–1.000] | 0.636 [0.364–0.909] | 0.778 [0.444–1.000] | 0.778 [0.444–1.000] |
| docx | 12 | 0.917 [0.750–1.000] | 1.000 [1.000–1.000] | 0.863 [0.664–1.000] | 0.833 [0.583–1.000] | 0.909 [0.727–1.000] | 0.909 [0.727–1.000] |
| pdf | 20 | 0.850 [0.700–1.000] | 0.950 [0.850–1.000] | 0.754 [0.579–0.917] | 0.800 [0.600–0.950] | 0.941 [0.824–1.000] | 0.941 [0.824–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 0.662 [0.319–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.906 [0.719–1.000] | 0.500 [0.125–0.875] | 0.500 [0.125–0.875] | 0.500 [0.125–0.875] |

Латентность запросов набора (клиент): p50 36.3 мс, p95 60.7 мс.
Основной набор с вложениями (326 запросов): nDCG@10 0.776 [0.737–0.814], recall@5 0.833 [0.792–0.872].

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; ru/git/revert-vs-reset [vector,fts]; inventory.xlsx · sheet:Склад!A4533:G4558 [vector];  [vector]; ru/windows/services-schtasks-robocopy [vector] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; inventory.xlsx · sheet:Поставки!A4381:G4400 [vector]; ru/api/rate-limiting-token-bucket [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/research/bm25-parametry [vector,fts] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; inventory.xlsx · sheet:Поставки!A906:G925 [vector]; ru/ops/nginx-reverse-proxy-headers [vector]; ru/zam/hybrid-search-rrf [vector]; ru/git/revert-vs-reset [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A5797:G5822 [vector]; book-500.pdf · page:49 [vector]; ru/long/catalog-search-relevance-study [vector]; ru/long/dev-environment-onboarding [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; cyrillic-ru.pdf · page:3 [vector]; ru/long/checkout-load-testing-report [vector,fts]; ru/rust/arc-mutex-deadlock [vector,fts]; ru/long/object-storage-backup-strategy [vector,fts] |
| d4-cross | cross-lang | 4 | para:47 | да | да | book-500.pdf · page:37 [vector]; en/ops/load-testing-percentiles [vector,fts];  [vector,fts]; regulations-ru.docx · para:47 [vector]; en/infra/load-balancer-health-checks [vector,fts] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; onecol-en.pdf · page:6 [vector]; inventory.xlsx · sheet:Поставки!A1100:G1119 [vector]; ru/long/object-storage-backup-strategy [vector,fts]; ru/rust/cargo-test-parallel-flaky-proptest [vector] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; book-500.pdf · page:273 [vector]; onecol-en.pdf · page:3 [vector]; roadmap.pptx · slide:17 [vector]; en/long/oncall-handbook [vector] |
| e1-cross | cross-lang | 9 | — | — | — | ru/long/dev-environment-onboarding [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; regulations-ru.docx · para:43 [vector]; ru/windows/services-schtasks-robocopy [vector,fts] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts]; inventory.xlsx · sheet:Staff!A9023:G9044 [vector]; book-500.pdf · page:90 [vector]; en/web/token-refresh-tabs [vector];  [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; roadmap.pptx · slide:8 [vector]; book-500.pdf · page:391 [vector]; en/devtools/gha-matrix-strategy [vector,fts]; en/long/feature-flag-platform [vector,fts] |
| e4-para | paraphrase | 1 | para:34 | нет | нет | onboarding-en.docx · para:34 [vector,fts]; inventory.xlsx · sheet:Staff!A7523:G7544 [vector]; en/long/oncall-handbook [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,fts]; ru/ops/postmortem-blameless [vector];  [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/zam/config-layers-env [vector,fts] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; ru/long/mobile-release-train [vector,fts,trigram]; ru/long/kafka-consumer-lag-analysis [vector,fts]; regulations-ru.docx · para:45 [vector]; ru/long/catalog-search-relevance-study [vector,fts] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/research/kross-enkodery-rerank [vector]; ru/windows/services-schtasks-robocopy [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; en/long/oncall-handbook [vector];  [vector]; book-500.pdf · page:417 [vector]; en/git/force-with-lease [vector] |
| x3-cross | cross-lang | 4 | sheet:Staff!A9360:G9381 | нет | нет | cyrillic-ru.pdf · page:3 [vector,fts]; regulations-ru.docx · para:69 [vector,fts];  [vector,fts]; inventory.xlsx · sheet:Staff!A9360:G9381 [vector]; ru/long/ci-pipeline-retrospective [vector,fts] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram];  [vector];  [vector]; ru/long/checkout-load-testing-report [vector];  [vector] |
| x5-date | date | 1 | sheet:Staff!A3558:G3579 | нет | нет | inventory.xlsx · sheet:Staff!A3558:G3579 [vector,fts]; ru/long/postgres-major-upgrade-runbook [vector,fts]; ru/web/date-formatting-intl [vector,fts]; ru/long/personal-data-retention-policy [vector]; ru/long/checkout-load-testing-report [vector] |
| x6-date | date | 1 | sheet:Staff!A8330:G8351 | нет | нет | inventory.xlsx · sheet:Staff!A8330:G8351 [vector]; en/long/oncall-handbook [vector];  [vector]; en/long/observability-stack-design [vector];  [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; book-500.pdf · page:273 [vector]; en/long/redis-cluster-migration [vector]; en/ops/dns-ttl-migration [vector]; cjk-zh.pdf · page:3 [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; inventory.xlsx · sheet:Staff!A159:G180 [vector]; ru/infra/proxy-timeouts-long-requests [vector]; ru/long/ci-pipeline-retrospective [vector] |
| p2-cross | cross-lang | 7 | — | — | — | onboarding-en.docx · para:30 [vector,fts]; twocol-en.pdf · page:6 [vector,fts]; en/long/api-versioning-guide [vector,fts];  [vector,fts]; en/api/database-statement-timeout [vector,fts] |
| p3-para | paraphrase | 6 | — | — | — | en/long/oncall-handbook [vector,fts]; book-500.pdf · page:203 [vector]; en/api/database-statement-timeout [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/observability-stack-design [vector,fts] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; cjk-zh.pdf · page:1 [vector]; regulations-ru.docx · para:43 [vector]; onboarding-en.docx · para:35 [vector]; inventory.xlsx · sheet:Поставки!A4673:G4692 [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; twocol-en.pdf · page:5 [vector]; inventory.xlsx · sheet:Поставки!A4830:G4849 [vector]; onboarding-en.docx · para:30 [vector]; en/zam/reranker-license [vector] |
| o2-para | paraphrase | 3 | page:6 | нет | нет | en/long/observability-stack-design [vector,fts]; book-500.pdf · page:363 [vector]; onecol-en.pdf · page:6 [vector]; en/long/dns-outage-postmortem [vector];  [vector] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; inventory.xlsx · sheet:Staff!A249:G270 [vector]; book-500.pdf · page:202 [vector]; onboarding-en.docx · para:11 [vector]; en/long/api-versioning-guide [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; book-500.pdf · page:261 [vector]; inventory.xlsx · sheet:Поставки!A4206:G4225 [vector];  [vector];  [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; onecol-en.pdf · page:6 [vector,trigram]; en/windows/wsl2-localhost-forwarding [vector,trigram]; en/db/connection-pool-exhaustion [vector,trigram]; en/devtools/npm-install-and-lockfile [vector,trigram] |
| t2-para | paraphrase | — | — | — | — | en/long/dns-outage-postmortem [vector,fts]; en/rust/future-not-send-mutexguard-await [vector,fts]; book-500.pdf · page:9 [vector]; onboarding-en.docx · para:6 [vector]; en/long/oncall-handbook [vector,fts] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; inventory.xlsx · sheet:Поставки!A6943:G6962 [vector]; onecol-en.pdf · page:3 [vector]; book-500.pdf · page:177 [vector]; en/git/submodules-pitfalls [vector] |
| t4-cross | cross-lang | 3 | page:6 | да | да | regulations-ru.docx · para:69 [vector]; ru/long/ci-pipeline-retrospective [vector,fts]; twocol-en.pdf · page:6 [vector]; ru/long/incident-disk-full-logs [vector,fts]; cyrillic-ru.pdf · page:5 [vector] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; inventory.xlsx · sheet:Поставки!A3655:G3674 [vector]; regulations-ru.docx · para:61 [vector]; ru/long/incident-disk-full-logs [vector]; ru/zam/hybrid-search-rrf [vector] |
| c2-para | paraphrase | 1 | page:5 | да | да | cyrillic-ru.pdf · page:5 [vector]; ru/long/checkout-load-testing-report [vector,fts]; inventory.xlsx · sheet:Поставки!A3931:G3950 [vector]; onboarding-en.docx · para:40 [vector]; ru/long/kafka-consumer-lag-analysis [vector,fts] |
| c3-cross | cross-lang | — | — | — | — | book-500.pdf · page:217 [vector]; en/long/observability-stack-design [vector,fts]; en/long/kubernetes-upgrade-log [vector,fts]; en/long/dns-outage-postmortem [vector,fts]; en/ops/trusted-proxies [fts] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; inventory.xlsx · sheet:Поставки!A2448:G2467 [vector]; ru/long/object-storage-backup-strategy [vector]; ru/long/checkout-load-testing-report [vector] |
| k1-cross | cross-lang | 6 | — | — | — | book-500.pdf · page:204 [vector]; en/devtools/npm-install-and-lockfile [vector,fts];  [vector,fts]; en/long/observability-stack-design [vector,fts]; en/infra/nginx-limit-req [vector,fts] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; inventory.xlsx · sheet:Staff!A7568:G7589 [vector]; en/long/dns-outage-postmortem [vector]; en/long/ml-model-serving-notes [vector]; en/long/kubernetes-upgrade-log [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; inventory.xlsx · sheet:Staff!A338:G359 [vector]; regulations-ru.docx · para:6 [vector];  [vector]; ru/git/clean-fdx [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; regulations-ru.docx · para:47 [vector]; en/linux/disk-full-but-df-shows-space [vector]; en/linux/oom-killer-dmesg [vector]; ru/ops/retries-backoff-jitter [vector] |
| b2-para | paraphrase | 1 | page:251 | да | да | book-500.pdf · page:251 [vector]; roadmap.pptx · slide:18 [vector]; en/long/postgres-index-tuning-notes [vector,fts]; en/long/observability-stack-design [vector,fts]; en/long/ml-model-serving-notes [vector,fts] |
| b3-cross | cross-lang | 4 | page:444 | да | да | cyrillic-ru.pdf · page:1 [vector,fts]; regulations-ru.docx · para:15 [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; book-500.pdf · page:444 [vector]; ru/api/rate-limiting-token-bucket [vector,fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; onboarding-en.docx · para:32 [vector]; en/long/kubernetes-upgrade-log [vector]; en/long/feature-flag-platform [vector]; en/long/redis-cluster-migration [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; inventory.xlsx · sheet:Staff!A9067:G9089 [vector]; cjk-zh.pdf · page:4 [vector]; onboarding-en.docx · para:6 [vector]; roadmap.pptx · slide:3 [vector] |

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
