# Eval документов: fts без модели (cpu, PDF — pdfium)

Код — `3403134+dirty.ee3bb400`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 4 с, индексация после него 0 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.622 [0.489–0.756] | 0.800 [0.689–0.911] | 0.597 [0.458–0.730] | 0.600 [0.467–0.733] | 0.964 [0.893–1.000] | 0.964 [0.893–1.000] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 0.667 [0.000–1.000] | 1.000 [1.000–1.000] | 0.722 [0.167–1.000] | 0.333 [0.000–1.000] | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.273 [0.000–0.545] | 0.909 [0.727–1.000] | 0.155 [0.000–0.355] | 0.273 [0.000–0.545] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| docx | 12 | 0.667 [0.417–0.917] | 0.750 [0.500–0.917] | 0.558 [0.292–0.808] | 0.667 [0.417–0.917] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pdf | 20 | 0.600 [0.400–0.800] | 0.800 [0.600–0.950] | 0.600 [0.400–0.800] | 0.600 [0.400–0.800] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 0.800 [0.400–1.000] | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.625 [0.250–0.875] | 0.875 [0.625–1.000] | 0.646 [0.292–0.896] | 0.500 [0.125–0.875] | 0.800 [0.400–1.000] | 0.800 [0.400–1.000] |

Латентность запросов набора (клиент): p50 8.1 мс, p95 16.0 мс.
Основной набор (326 запросов, второй проход): без вложений p50 8.0 / p95 14.0 мс, с вложениями p50 10.5 / p95 17.0 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [fts]; ru/research/kross-enkodery-rerank [fts]; ru/devtools/uv-package-manager [fts]; ru/git/revert-vs-reset [fts]; ru/long/object-storage-backup-strategy [fts] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [fts]; ru/windows/powershell-encoding [fts]; ru/ops/nginx-reverse-proxy-headers [fts]; ru/git/cherry-pick-range [fts]; ru/long/kafka-consumer-lag-analysis [fts] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [fts] |
| d3-para | paraphrase | — | — | — | — | ru/devtools/code-review-checklist [fts]; ru/long/ci-pipeline-retrospective [fts]; ru/long/checkout-load-testing-report [fts]; ru/long/tls-certificate-rotation [fts]; inventory.xlsx · sheet:Склад!A3059:G3086 [fts] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [fts]; ru/long/object-storage-backup-strategy [fts]; ru/linux/ulimit-open-files-emfile [fts]; ru/rust/rwlock-vs-mutex [fts]; ru/long/personal-data-retention-policy [fts] |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles [fts]; en/db/sqlite-fts5-search-index [fts]; en/long/api-versioning-guide [fts]; en/long/payment-webhooks-reliability [fts]; en/long/redis-cluster-migration [fts] |
| d5-para | paraphrase | 5 | para:61 | да | да | ru/zam/architecture-overview [fts]; ru/windows/powershell-encoding [fts]; ru/ops/backups-restore-testing [fts]; ru/zam/qdrant-managed-process [fts]; regulations-ru.docx · para:61 [fts] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [fts] |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding [fts]; ru/long/object-storage-backup-strategy [fts]; ru/windows/services-schtasks-robocopy [fts]; ru/long/incident-disk-full-logs [fts]; ru/long/ci-pipeline-retrospective [fts] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [fts] |
| e3-para | paraphrase | 2 | para:30 | да | да | en/long/dns-outage-postmortem [fts]; onboarding-en.docx · para:30 [fts]; en/devtools/gha-matrix-strategy [fts]; ru/ops/nginx-reverse-proxy-headers [fts]; en/long/payment-webhooks-reliability [fts] |
| e4-para | paraphrase | — | — | — | — | en/long/oncall-handbook [fts]; en/long/feature-flag-platform [fts]; en/git/signing-commits-ssh-gpg [fts]; en/long/api-versioning-guide [fts]; en/git/bisect-run [fts] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [fts]; ru/zam/config-layers-env [fts] |
| x1-para | paraphrase | — | — | — | — | ru/research/metriki-ir [fts,trigram]; ru/git/clean-fdx [fts,trigram]; ru/long/tls-certificate-rotation [fts,trigram]; ru/long/object-storage-backup-strategy [fts,trigram]; ru/windows/git-autocrlf-gitattributes [fts,trigram] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [fts] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [fts] |
| x3-cross | cross-lang | — | — | — | — | cyrillic-ru.pdf · page:4 [fts]; regulations-ru.docx · para:65 [fts]; ru/devtools/flaky-tests-quarantine [fts]; ru/long/incident-disk-full-logs [fts]; ru/ops/backups-restore-testing [fts] |
| x4-date | date | 1 | sheet:Поставки!A8995:G9013 | да | да | inventory.xlsx · sheet:Поставки!A8995:G9013 [fts,trigram] |
| x5-date | date | 6 | — | — | — | cyrillic-ru.pdf · page:2 [fts]; regulations-ru.docx · para:69 [fts]; ru/long/tls-certificate-rotation [fts]; roadmap.pptx · slide:11 [fts]; ru/long/postgres-major-upgrade-runbook [fts] |
| x6-date | date | 1 | sheet:Staff!A181:G202 | нет | нет | inventory.xlsx · sheet:Staff!A181:G202 [fts] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [fts] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [fts,trigram] |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide [fts]; en/long/redis-cluster-migration [fts]; en/long/payment-webhooks-reliability [fts]; onboarding-en.docx · para:30 [fts]; en/research/learned-sparse-splade [fts] |
| p3-para | paraphrase | — | — | — | — | en/long/api-versioning-guide [fts]; en/linux/systemd-timers-vs-cron [fts]; en/long/ml-model-serving-notes [fts]; en/rust/thiserror-vs-anyhow [fts]; en/long/oncall-handbook [fts] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [fts] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [fts] |
| o2-para | paraphrase | — | — | — | — | en/long/feature-flag-platform [fts]; en/long/observability-stack-design [fts]; en/long/oncall-handbook [fts]; en/long/ml-model-serving-notes [fts]; en/long/frontend-performance-audit [fts] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [fts] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [fts] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [fts,trigram]; en/windows/wsl2-localhost-forwarding [trigram]; onecol-en.pdf · page:6 [trigram]; en/devtools/npm-install-and-lockfile [trigram]; en/db/connection-pool-exhaustion [trigram] |
| t2-para | paraphrase | — | — | — | — | en/long/dns-outage-postmortem [fts]; en/rust/e0499-two-mutable-borrows [fts]; en/windows/wsl2-localhost-forwarding [fts]; en/long/oncall-handbook [fts]; en/rust/tokio-spawn-blocking [fts] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [fts] |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective [fts]; ru/long/incident-disk-full-logs [fts]; ru/ops/postmortem-blameless [fts]; ru/devtools/code-review-checklist [fts]; ru/long/postgres-major-upgrade-runbook [fts] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [fts] |
| c2-para | paraphrase | — | — | — | — | ru/docker/rootless-podman [fts]; ru/long/checkout-load-testing-report [fts]; ru/long/catalog-search-relevance-study [fts];  [fts]; ru/long/dev-environment-onboarding [fts] |
| c3-cross | cross-lang | — | — | — | — | en/ops/trusted-proxies [fts]; en/long/kubernetes-upgrade-log [fts]; en/long/observability-stack-design [fts]; en/long/api-versioning-guide [fts]; en/git/submodules-pitfalls [fts] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [fts] |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile [fts]; en/zam/windows-data-dir-path-limit [fts]; en/long/feature-flag-platform [fts]; en/linux/oom-killer-dmesg [fts]; en/db/sqlite-fts5-search-index [fts] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [fts] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [fts] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [fts] |
| b2-para | paraphrase | — | — | — | — | en/long/oncall-handbook [fts]; en/long/payment-webhooks-reliability [fts]; en/long/dns-outage-postmortem [fts]; en/git/submodules-pitfalls [fts]; en/long/api-versioning-guide [fts] |
| b3-cross | cross-lang | — | — | — | — | ru/rust/rwlock-vs-mutex [fts]; ru/research/bm25-parametry [fts]; ru/ops/nginx-reverse-proxy-headers [fts]; ru/devtools/code-review-checklist [fts]; ru/ops/postmortem-blameless [fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [fts] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [fts] |

## Документы

Сверка с допустимыми исходами манифеста: **21 из 21**.

| Файл | Роль | Допустимо | Состояние | Причина | Предупреждения | ocr_needed | Сверка | с до конца |
|---|---|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  |  | false | да | 4.0 |
| `files/onboarding-en.docx` | content | done | done |  |  | false | да | 3.9 |
| `files/inventory.xlsx` | content | done | done |  |  | false | да | 3.8 |
| `files/roadmap.pptx` | content | done | done |  |  | false | да | 3.8 |
| `files/onecol-en.pdf` | content | done | done |  |  | false | да | 3.8 |
| `files/twocol-en.pdf` | content | done | done |  |  | false | да | 3.7 |
| `files/cyrillic-ru.pdf` | content | done | done |  |  | false | да | 3.7 |
| `files/cjk-zh.pdf` | content | done | done |  |  | false | да | 3.6 |
| `files/book-500.pdf` | content | done | done |  |  | false | да | 2.7 |
| `files/scan.pdf` | scan | done+ocr | done |  |  | true | да | 2.7 |
| `hostile/zip-bomb.docx` | hostile | done+truncated: max_ratio / failed:parse_error | done |  | truncated: max_ratio | false | да | 2.5 |
| `hostile/xxe.docx` | hostile | failed:parse_error: xml | failed | parse_error: xml (word/document.xml) | parse_error: xml (word/document.xml) | false | да | 2.5 |
| `hostile/billion-laughs.xlsx` | hostile | failed:parse_error: xml | failed | parse_error: xml (xl/sharedStrings.xml) | parse_error: xml (xl/sharedStrings.xml) | false | да | 2.5 |
| `hostile/deep-nesting.docx` | hostile | done+truncated: max_xml_depth / failed:parse_error | done |  | truncated: max_ratio; truncated: max_xml_depth | false | да | 2.4 |
| `hostile/broken.pdf` | hostile | failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 2.4 |
| `hostile/truncated.pdf` | hostile | done / failed:parse_error: pdf | failed | parse_error: pdf | parse_error: pdf | false | да | 2.3 |
| `hostile/js-action.pdf` | hostile | done | done |  |  | false | да | 2.3 |
| `hostile/deep-array.pdf` | hostile | done / failed:parse_error | done |  |  | false | да | 2.2 |
| `hostile/glyph-flood.pdf` | hostile | done+truncated: page_chars | done |  | truncated: page_chars | false | да | 2.2 |
| `hostile/huge-image.pdf` | hostile | done+ocr | done |  |  | true | да | 0.1 |
| `hostile/truncated.docx` | hostile | failed:parse_error: zip | failed | parse_error: zip | parse_error: zip | false | да | 0.0 |

Кэш извлечения `blob_extract`: 21 строк, zstd(JSON) 0.73 МБ, JSON 5.15 МБ (сжатие 7.1×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 45 | 8 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 70 | 8 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 65 | 8 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 70 | 9 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 29 | 54 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 22 | 1 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1372 | 2 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 4 | 1 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 2 | 1 | превью не построено: parse_error: pdf |
