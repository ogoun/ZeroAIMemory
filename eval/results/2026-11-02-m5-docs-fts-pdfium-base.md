# Eval документов: fts без модели (cpu, PDF — pdfium)

Код — `251a72b+dirty.84586688`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: да. Извлечение 4 с, индексация после него 0 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|
| **все** | 45 | 0.644 [0.511–0.778] | 0.618 [0.478–0.751] | 0.578 [0.422–0.711] | 0.897 [0.759–1.000] | 0.897 [0.759–1.000] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.273 [0.000–0.545] | 0.164 [0.018–0.355] | 0.273 [0.000–0.545] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| docx | 12 | 0.667 [0.417–0.917] | 0.567 [0.308–0.808] | 0.667 [0.417–0.917] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pdf | 20 | 0.600 [0.400–0.800] | 0.600 [0.400–0.800] | 0.600 [0.400–0.800] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| pptx | 5 | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 0.600 [0.200–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| xlsx | 8 | 0.750 [0.375–1.000] | 0.750 [0.375–1.000] | 0.375 [0.000–0.750] | 0.500 [0.167–0.833] | 0.500 [0.167–0.833] |

Латентность запросов набора (клиент): p50 10.7 мс, p95 21.4 мс.
Основной набор (326 запросов, второй проход): без вложений p50 10.2 / p95 15.8 мс, с вложениями p50 12.6 / p95 19.2 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9; ru/research/kross-enkodery-rerank; ru/devtools/uv-package-manager; ru/git/revert-vs-reset; ru/long/object-storage-backup-strategy |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9; ru/windows/powershell-encoding; ru/ops/nginx-reverse-proxy-headers; ru/git/cherry-pick-range; ru/long/personal-data-retention-policy |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 |
| d3-para | paraphrase | — | — | — | — | ru/devtools/code-review-checklist; ru/long/ci-pipeline-retrospective; ru/long/checkout-load-testing-report; ru/long/tls-certificate-rotation; inventory.xlsx · sheet:Склад!A8233:G8262 |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47; ru/long/object-storage-backup-strategy; ru/linux/ulimit-open-files-emfile; ru/rust/rwlock-vs-mutex; ru/long/personal-data-retention-policy |
| d4-cross | cross-lang | — | — | — | — | en/ops/load-testing-percentiles; en/db/sqlite-fts5-search-index; en/long/api-versioning-guide; en/long/payment-webhooks-reliability; en/long/redis-cluster-migration |
| d5-para | paraphrase | 5 | para:61 | да | да | ru/zam/architecture-overview; ru/windows/powershell-encoding; ru/ops/backups-restore-testing; ru/zam/qdrant-managed-process; regulations-ru.docx · para:61 |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 |
| e1-cross | cross-lang | — | — | — | — | ru/long/dev-environment-onboarding; ru/long/object-storage-backup-strategy; ru/windows/services-schtasks-robocopy; ru/long/incident-disk-full-logs; ru/long/ci-pipeline-retrospective |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 |
| e3-para | paraphrase | 2 | para:30 | да | да | en/long/dns-outage-postmortem; onboarding-en.docx · para:30; en/devtools/gha-matrix-strategy; ru/ops/nginx-reverse-proxy-headers; en/long/payment-webhooks-reliability |
| e4-para | paraphrase | 10 | — | — | — | en/long/oncall-handbook; en/long/feature-flag-platform; en/git/signing-commits-ssh-gpg; en/long/api-versioning-guide; en/git/bisect-run |
| x1-exact | exact | 1 | sheet:Поставки!A1231:G1250 | да | да | inventory.xlsx · sheet:Поставки!A1231:G1250; ru/zam/config-layers-env |
| x1-para | paraphrase | — | — | — | — | ru/research/metriki-ir; ru/git/clean-fdx; ru/long/tls-certificate-rotation; ru/long/object-storage-backup-strategy; ru/windows/git-autocrlf-gitattributes |
| x2-exact | exact | 1 | sheet:Склад!A7775:G7801 | да | да | inventory.xlsx · sheet:Склад!A7775:G7801 |
| x3-exact | exact | 1 | sheet:Staff!A5009:G5032 | да | да | inventory.xlsx · sheet:Staff!A5009:G5032 |
| x3-cross | cross-lang | — | — | — | — | cyrillic-ru.pdf · page:4; regulations-ru.docx · para:65; ru/devtools/flaky-tests-quarantine; ru/long/incident-disk-full-logs; ru/ops/backups-restore-testing |
| x4-date | date | 1 | sheet:Поставки!A8832:G8852 | нет | нет | inventory.xlsx · sheet:Поставки!A8832:G8852; en/linux/systemd-timers-vs-cron; ru/devtools/semver-and-changelog; en/research/learned-sparse-splade; ru/long/tls-certificate-rotation |
| x5-date | date | 1 | sheet:Staff!A193:G216 | нет | нет | inventory.xlsx · sheet:Staff!A193:G216; en/linux/journalctl-filters; cyrillic-ru.pdf · page:2; regulations-ru.docx · para:69; en/research/colbert-late-interaction |
| x6-date | date | 1 | sheet:Staff!A193:G216 | нет | нет | inventory.xlsx · sheet:Staff!A193:G216; ru/ops/postmortem-blameless; ru/windows/services-schtasks-robocopy; ru/research/butstrep-intervaly; en/linux/systemd-timers-vs-cron |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 |
| p2-cross | cross-lang | — | — | — | — | en/long/api-versioning-guide; en/long/redis-cluster-migration; en/long/payment-webhooks-reliability; onboarding-en.docx · para:30; en/research/learned-sparse-splade |
| p3-para | paraphrase | — | — | — | — | en/long/api-versioning-guide; en/linux/systemd-timers-vs-cron; en/long/ml-model-serving-notes; en/rust/thiserror-vs-anyhow; en/long/oncall-handbook |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 |
| o2-para | paraphrase | — | — | — | — | en/long/feature-flag-platform; en/long/observability-stack-design; en/long/oncall-handbook; en/long/ml-model-serving-notes; en/long/frontend-performance-audit |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2; en/windows/wsl2-localhost-forwarding; onecol-en.pdf · page:6; en/devtools/npm-install-and-lockfile; en/db/connection-pool-exhaustion |
| t2-para | paraphrase | — | — | — | — | en/long/dns-outage-postmortem; en/rust/e0499-two-mutable-borrows; en/windows/wsl2-localhost-forwarding; en/long/oncall-handbook; en/research/onnx-runtime-execution-providers |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 |
| t4-cross | cross-lang | — | — | — | — | ru/long/ci-pipeline-retrospective; ru/long/incident-disk-full-logs; ru/ops/postmortem-blameless; ru/devtools/code-review-checklist; ru/long/postgres-major-upgrade-runbook |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 |
| c2-para | paraphrase | — | — | — | — | ru/docker/rootless-podman; ru/long/checkout-load-testing-report; ru/long/catalog-search-relevance-study; ; ru/long/dev-environment-onboarding |
| c3-cross | cross-lang | — | — | — | — | en/ops/trusted-proxies; en/long/kubernetes-upgrade-log; en/long/observability-stack-design; en/long/api-versioning-guide; en/git/submodules-pitfalls |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 |
| k1-cross | cross-lang | — | — | — | — | en/devtools/npm-install-and-lockfile; en/zam/windows-data-dir-path-limit; en/long/feature-flag-platform; en/linux/oom-killer-dmesg; en/db/sqlite-fts5-search-index |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 |
| b2-para | paraphrase | — | — | — | — | en/long/oncall-handbook; en/long/payment-webhooks-reliability; en/long/dns-outage-postmortem; en/git/submodules-pitfalls; en/long/api-versioning-guide |
| b3-cross | cross-lang | — | — | — | — | ru/rust/rwlock-vs-mutex; ru/research/bm25-parametry; ru/ops/nginx-reverse-proxy-headers; ru/devtools/code-review-checklist; ru/ops/postmortem-blameless |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 4.1 |
| `files/onboarding-en.docx` | content | done | done |  | false | 4.0 |
| `files/inventory.xlsx` | content | done | done |  | false | 3.9 |
| `files/roadmap.pptx` | content | done | done |  | false | 3.9 |
| `files/onecol-en.pdf` | content | done | done |  | false | 3.8 |
| `files/twocol-en.pdf` | content | done | done |  | false | 3.7 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 2.8 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 2.8 |
| `files/book-500.pdf` | content | done | done |  | false | 2.6 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 2.6 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio | failed | done |  | false | 2.5 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 2.4 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 2.4 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error | truncated: max_xml_depth | done |  | false | 2.4 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 2.3 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) | failed | failed | parse_error: pdf | false | 1.7 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.6 |
| `hostile/deep-array.pdf` | hostile | done | failed: parse_error (не parse_crash) | done |  | false | 0.9 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 0.8 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom | parse_crash | done |  | true | 0.7 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.71 МБ, JSON 4.97 МБ (сжатие 7.0×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 50 | 9 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 72 | 11 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 73 | 10 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 71 | 11 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 6 | 2 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 39 | 9 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 31 | 10 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1205 | 7 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 5 | 7 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
