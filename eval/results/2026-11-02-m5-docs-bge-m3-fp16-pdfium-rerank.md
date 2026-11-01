# Eval документов: hybrid+rerank bge-m3-fp16 (cuda, PDF — pdfium)

Код — `251a72b+dirty.3fd008d5`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 19 с, индексация после него 16 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.933 [0.844–1.000] | 0.978 [0.933–1.000] | 0.881 [0.789–0.956] | 0.756 [0.622–0.867] | 0.810 [0.690–0.929] | 0.810 [0.690–0.929] |
| cross-lang | 8 | 0.750 [0.375–1.000] | 0.875 [0.625–1.000] | 0.667 [0.375–1.000] | 0.625 [0.250–1.000] | 0.833 [0.500–1.000] | 0.833 [0.500–1.000] |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.949 [0.870–1.000] | 0.913 [0.783–1.000] | 0.913 [0.783–1.000] | 0.913 [0.783–1.000] |
| paraphrase | 11 | 0.909 [0.727–1.000] | 1.000 [1.000–1.000] | 0.864 [0.682–1.000] | 0.727 [0.455–0.911] | 0.800 [0.500–1.000] | 0.800 [0.500–1.000] |
| docx | 12 | 0.917 [0.750–1.000] | 1.000 [1.000–1.000] | 0.833 [0.625–1.000] | 0.833 [0.583–1.000] | 0.909 [0.727–1.000] | 0.909 [0.727–1.000] |
| pdf | 20 | 0.900 [0.750–1.000] | 0.950 [0.850–1.000] | 0.867 [0.717–1.000] | 0.850 [0.700–1.000] | 0.944 [0.833–1.000] | 0.944 [0.833–1.000] |
| pptx | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.917 [0.750–1.000] | 0.250 [0.000–0.625] | 0.250 [0.000–0.625] | 0.250 [0.000–0.625] |

Латентность запросов набора (клиент): p50 107.3 мс, p95 139.9 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; inventory.xlsx · sheet:Склад!A4533:G4558 [vector]; ru/long/incident-disk-full-logs [vector]; ru/git/revert-vs-reset [vector,fts]; ru/devtools/uv-package-manager [vector,fts] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts];  [vector];  [vector]; ru/long/incident-disk-full-logs [vector]; en/long/redis-cluster-migration [vector] |
| d2-exact | exact | 2 | para:17 | да | да | inventory.xlsx · sheet:Поставки!A906:G925 [vector]; regulations-ru.docx · para:17 [vector,fts]; ru/research/kross-enkodery-rerank [vector]; ru/zam/hybrid-search-rrf [vector]; ru/research/hnsw-parametry [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A4533:G4558 [vector]; ru/research/rrf-sliyanie [vector]; en/long/observability-stack-design [vector]; en/research/colbert-late-interaction [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; ru/long/dev-environment-onboarding [vector]; cyrillic-ru.pdf · page:1 [vector]; ru/rust/cargo-test-parallel-flaky-proptest [vector,fts]; ru/long/incident-disk-full-logs [vector] |
| d4-cross | cross-lang | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector]; en/ops/load-testing-percentiles [vector,fts]; ru/long/checkout-load-testing-report [vector]; en/devtools/playwright-flaky-e2e [vector,fts]; book-500.pdf · page:37 [vector] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; en/long/feature-flag-platform [vector]; ru/ops/backups-restore-testing [fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/zam/qdrant-managed-process [vector,fts] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; book-500.pdf · page:273 [vector]; onecol-en.pdf · page:3 [vector]; en/zam/answer-cache-protocol [vector]; roadmap.pptx · slide:17 [vector] |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding [vector,fts]; ru/long/tls-certificate-rotation [vector,fts]; ru/windows/services-schtasks-robocopy [vector,fts]; ru/zam/config-layers-env [vector]; ru/zam/qdrant-managed-process [vector] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts];  [vector]; ru/ops/tls-expired-certificate [vector]; ru/long/tls-certificate-rotation [vector]; en/git/interactive-rebase-autosquash [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; book-500.pdf · page:441 [vector]; roadmap.pptx · slide:8 [vector]; en/long/dns-outage-postmortem [vector,fts]; en/long/api-versioning-guide [vector,fts] |
| e4-para | paraphrase | 2 | para:43 | нет | нет | onecol-en.pdf · page:9 [vector]; onboarding-en.docx · para:43 [vector]; en/long/feature-flag-platform [vector]; inventory.xlsx · sheet:Staff!A7523:G7544 [vector]; en/long/oncall-handbook [vector,fts] |
| x1-exact | exact | 1 | sheet:Поставки!A3891:G3910 | нет | нет | inventory.xlsx · sheet:Поставки!A3891:G3910 [vector]; ru/ops/postmortem-blameless [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/zam/spaces-and-human-isolation [vector]; ru/long/incident-disk-full-logs [vector] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,trigram]; ru/long/mobile-release-train [vector,fts,trigram]; ru/long/kafka-consumer-lag-analysis [vector]; en/rust/future-not-send-mutexguard-await [vector]; ru/ops/postmortem-blameless [vector,fts] |
| x2-exact | exact | 1 | sheet:Склад!A7008:G7033 | нет | нет | inventory.xlsx · sheet:Склад!A7008:G7033 [vector,trigram]; ru/long/incident-disk-full-logs [vector]; ru/long/postgres-major-upgrade-runbook [vector]; book-500.pdf · page:12 [vector]; ru/long/kafka-consumer-lag-analysis [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; book-500.pdf · page:417 [vector]; ru/ops/tls-expired-certificate [vector]; en/long/oncall-handbook [vector]; en/ops/slo-burn-rate-alerts [vector] |
| x3-cross | cross-lang | 3 | sheet:Staff!A7747:G7768 | нет | нет | cyrillic-ru.pdf · page:3 [vector,fts]; regulations-ru.docx · para:69 [vector,fts]; inventory.xlsx · sheet:Staff!A7747:G7768 [vector]; en/long/oncall-handbook [vector]; ru/ops/backups-restore-testing [fts] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram]; ru/long/checkout-load-testing-report [vector];  [vector]; ru/long/tls-certificate-rotation [vector]; ru/long/ci-pipeline-retrospective [vector] |
| x5-date | date | 1 | sheet:Staff!A8374:G8395 | нет | нет | inventory.xlsx · sheet:Staff!A8374:G8395 [vector]; ru/long/postgres-major-upgrade-runbook [vector,fts]; ru/long/tls-certificate-rotation [vector]; ru/long/checkout-load-testing-report [vector]; ru/long/personal-data-retention-policy [vector] |
| x6-date | date | 1 | sheet:Staff!A8330:G8351 | нет | нет | inventory.xlsx · sheet:Staff!A8330:G8351 [vector]; en/long/oncall-handbook [vector]; en/long/payment-webhooks-reliability [vector]; ru/long/object-storage-backup-strategy [vector]; en/linux/systemd-unit-restart [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; cjk-zh.pdf · page:3 [vector]; ru/db/postgres-zero-downtime-migrations [vector]; book-500.pdf · page:477 [vector]; en/long/oncall-handbook [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram];  [vector]; ru/long/ci-pipeline-retrospective [vector]; ru/ops/postmortem-blameless [vector]; ru/long/personal-data-retention-policy [vector] |
| p2-cross | cross-lang | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector]; en/long/kubernetes-upgrade-log [vector]; en/devtools/playwright-flaky-e2e [vector,fts]; en/long/api-versioning-guide [vector,fts]; en/long/dns-outage-postmortem [vector,fts] |
| p3-para | paraphrase | 1 | slide:17 | да | да | roadmap.pptx · slide:17 [vector]; ru/long/personal-data-retention-policy [vector]; en/long/observability-stack-design [vector,fts]; en/long/oncall-handbook [vector];  [vector] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; en/long/kubernetes-upgrade-log [vector]; ru/zam/proactive-recall-hooks [vector]; ru/ops/retries-backoff-jitter [vector]; inventory.xlsx · sheet:Поставки!A4673:G4692 [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; inventory.xlsx · sheet:Поставки!A984:G1002 [vector]; ru/long/kafka-consumer-lag-analysis [vector]; ru/research/bm25-parametry [vector]; twocol-en.pdf · page:5 [vector] |
| o2-para | paraphrase | 1 | page:9 | нет | нет | onecol-en.pdf · page:9 [vector]; en/long/observability-stack-design [vector,fts]; book-500.pdf · page:363 [vector]; twocol-en.pdf · page:1 [vector]; en/long/feature-flag-platform [vector] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; inventory.xlsx · sheet:Staff!A8284:G8306 [vector]; onboarding-en.docx · para:11 [vector]; book-500.pdf · page:202 [vector]; ru/zam/config-layers-env [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; book-500.pdf · page:261 [vector]; inventory.xlsx · sheet:Поставки!A6596:G6615 [vector]; en/db/connection-pool-exhaustion [vector]; en/linux/oom-killer-dmesg [vector] |
| t1-exact | exact | 3 | page:2 | да | да | onecol-en.pdf · page:6 [vector,trigram]; book-500.pdf · page:304 [vector]; twocol-en.pdf · page:2 [vector,fts,trigram]; en/long/redis-cluster-migration [vector]; en/devtools/gha-cache-keys [vector] |
| t2-para | paraphrase | — | — | — | — | book-500.pdf · page:223 [vector]; onboarding-en.docx · para:6 [vector]; ru/long/ci-pipeline-retrospective [vector]; en/rust/future-not-send-mutexguard-await [vector,fts]; en/long/dns-outage-postmortem [vector] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; book-500.pdf · page:177 [vector]; onecol-en.pdf · page:3 [vector]; en/git/submodules-pitfalls [vector]; inventory.xlsx · sheet:Поставки!A6943:G6962 [vector] |
| t4-cross | cross-lang | 1 | page:6 | да | да | twocol-en.pdf · page:6 [vector]; cyrillic-ru.pdf · page:2 [vector]; regulations-ru.docx · para:61 [vector]; ru/ops/postmortem-blameless [vector,fts]; ru/long/ci-pipeline-retrospective [vector,fts] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; inventory.xlsx · sheet:Поставки!A1159:G1177 [vector]; ru/zam/hybrid-search-rrf [vector]; ru/devtools/uv-package-manager [vector]; ru/research/butstrep-intervaly [vector] |
| c2-para | paraphrase | 1 | page:5 | да | да | cyrillic-ru.pdf · page:5 [vector]; onboarding-en.docx · para:40 [vector];  [vector]; regulations-ru.docx · para:69 [vector]; inventory.xlsx · sheet:Поставки!A3931:G3950 [vector] |
| c3-cross | cross-lang | — | — | — | — |  [vector]; en/long/observability-stack-design [vector,fts]; en/long/oncall-handbook [vector]; ru/long/personal-data-retention-policy [vector]; en/git/reflog-recovery [vector] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts]; cyrillic-ru.pdf · page:5 [vector]; inventory.xlsx · sheet:Поставки!A2448:G2467 [vector]; en/long/ml-model-serving-notes [vector]; ru/long/object-storage-backup-strategy [vector] |
| k1-cross | cross-lang | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector]; en/devtools/npm-install-and-lockfile [vector,fts]; book-500.pdf · page:227 [vector]; en/long/observability-stack-design [vector,fts]; roadmap.pptx · slide:8 [vector] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; inventory.xlsx · sheet:Staff!A7568:G7589 [vector]; en/long/oncall-handbook [vector]; ru/long/dev-environment-onboarding [vector]; en/long/ml-model-serving-notes [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; inventory.xlsx · sheet:Staff!A338:G359 [vector]; en/git/interactive-rebase-autosquash [vector]; en/rust/tokio-spawn-blocking [vector]; ru/git/clean-fdx [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; regulations-ru.docx · para:47 [vector]; ru/long/checkout-load-testing-report [vector]; en/rust/workspace-dependencies-and-profiles [vector]; en/linux/disk-full-but-df-shows-space [vector] |
| b2-para | paraphrase | 1 | page:251 | да | да | book-500.pdf · page:251 [vector]; ru/long/ci-pipeline-retrospective [vector]; en/long/observability-stack-design [vector]; roadmap.pptx · slide:18 [vector]; en/long/ml-model-serving-notes [vector,fts] |
| b3-cross | cross-lang | 1 | page:444 | да | да | book-500.pdf · page:444 [vector]; en/long/observability-stack-design [vector]; cyrillic-ru.pdf · page:5 [vector,fts]; ru/long/ci-pipeline-retrospective [vector];  [vector] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; onboarding-en.docx · para:32 [vector]; en/long/ml-model-serving-notes [vector]; en/research/onnx-runtime-execution-providers [vector]; en/git/sparse-checkout-partial-clone [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts];  [vector]; ru/long/object-storage-backup-strategy [vector]; ru/ops/log-rotation [vector]; en/long/oncall-handbook [vector] |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 8.2 |
| `files/onboarding-en.docx` | content | done | done |  | false | 8.0 |
| `files/inventory.xlsx` | content | done | done |  | false | 17.0 |
| `files/roadmap.pptx` | content | done | done |  | false | 7.3 |
| `files/onecol-en.pdf` | content | done | done |  | false | 7.2 |
| `files/twocol-en.pdf` | content | done | done |  | false | 7.1 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 7.0 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 6.8 |
| `files/book-500.pdf` | content | done | done |  | false | 6.6 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 6.3 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio \| failed | done |  | false | 1.6 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 1.5 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 1.4 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error \| truncated: max_xml_depth | done |  | false | 1.3 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 1.2 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) \| failed | failed | parse_error: pdf | false | 1.0 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 0.9 |
| `hostile/deep-array.pdf` | hostile | done \| failed: parse_error (не parse_crash) | done |  | false | 0.8 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 5.0 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom \| parse_crash | done |  | true | 4.5 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.73 МБ, JSON 5.15 МБ (сжатие 7.1×).
