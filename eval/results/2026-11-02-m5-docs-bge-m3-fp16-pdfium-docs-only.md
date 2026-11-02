# Eval документов: hybrid bge-m3-fp16 (cuda, PDF — pdfium)

Код — `251a72b+dirty.3fd008d5`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: нет. Извлечение 13 с, индексация после него 48 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.911 [0.822–0.978] | 0.978 [0.933–1.000] | 0.815 [0.714–0.907] | 0.778 [0.644–0.889] | 0.854 [0.732–0.951] | 0.854 [0.732–0.951] |
| cross-lang | 8 | 0.625 [0.250–1.000] | 0.875 [0.625–1.000] | 0.226 [0.115–0.341] | 0.625 [0.250–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.909 [0.727–1.000] | 1.000 [1.000–1.000] | 0.806 [0.598–1.000] | 0.636 [0.364–0.909] | 0.700 [0.400–1.000] | 0.700 [0.400–1.000] |
| docx | 12 | 0.917 [0.750–1.000] | 1.000 [1.000–1.000] | 0.866 [0.670–1.000] | 0.833 [0.583–1.000] | 0.909 [0.727–1.000] | 0.909 [0.727–1.000] |
| pdf | 20 | 0.900 [0.750–1.000] | 0.950 [0.850–1.000] | 0.785 [0.625–0.931] | 0.850 [0.700–1.000] | 0.944 [0.833–1.000] | 0.944 [0.833–1.000] |
| pptx | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.717 [0.417–1.000] | 0.800 [0.400–1.000] | 0.800 [0.400–1.000] | 0.800 [0.400–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 1.000 [1.000–1.000] | 0.875 [0.625–1.000] | 0.500 [0.125–0.875] | 0.571 [0.143–1.000] | 0.571 [0.143–1.000] |

Латентность запросов набора (клиент): p50 23.9 мс, p95 32.9 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; eval-docs/files-regulations-ru-docx [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-inventory-xlsx [vector]; eval-docs/hostile-zip-bomb-docx [vector] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [vector,fts]; eval-docs/files-regulations-ru-docx [vector]; eval-docs/files-inventory-xlsx [vector]; eval-docs/hostile-deep-nesting-docx [vector]; inventory.xlsx · sheet:Поставки!A4381:G4400 [vector] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [vector,fts]; inventory.xlsx · sheet:Поставки!A906:G925 [vector]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-glyph-flood-pdf [vector]; eval-docs/hostile-deep-array-pdf [vector] |
| d3-para | paraphrase | 1 | para:43 | да | да | regulations-ru.docx · para:43 [vector]; inventory.xlsx · sheet:Склад!A5797:G5822 [vector]; eval-docs/files-regulations-ru-docx [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-cyrillic-ru-pdf [vector] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [vector,fts]; cyrillic-ru.pdf · page:3 [vector,fts]; eval-docs/files-cjk-zh-pdf [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/files-cyrillic-ru-pdf [vector] |
| d4-cross | cross-lang | 4 | para:47 | да | да | book-500.pdf · page:37 [vector,fts]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-deep-nesting-docx [vector]; regulations-ru.docx · para:47 [vector]; eval-docs/hostile-deep-array-pdf [vector] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [vector,fts]; inventory.xlsx · sheet:Поставки!A6885:G6903 [vector,fts]; onecol-en.pdf · page:6 [vector]; eval-docs/hostile-deep-array-pdf [vector]; eval-docs/hostile-broken-pdf [vector] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [vector,fts]; eval-docs/hostile-huge-image-pdf [vector]; eval-docs/files-onboarding-en-docx [vector]; book-500.pdf · page:273 [vector]; eval-docs/hostile-broken-pdf [vector] |
| e1-cross | cross-lang | 7 | — | — | — | regulations-ru.docx · para:43 [vector,fts]; cyrillic-ru.pdf · page:5 [vector,fts]; eval-docs/files-onboarding-en-docx [vector]; eval-docs/hostile-xxe-docx [vector]; cjk-zh.pdf · page:3 [vector] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [vector,fts]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-broken-pdf [vector]; inventory.xlsx · sheet:Staff!A9023:G9044 [vector]; book-500.pdf · page:90 [vector] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [vector,fts]; roadmap.pptx · slide:8 [vector]; book-500.pdf · page:391 [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-truncated-docx [vector] |
| e4-para | paraphrase | 1 | para:34 | нет | нет | onboarding-en.docx · para:34 [vector,fts]; onecol-en.pdf · page:9 [vector,fts]; inventory.xlsx · sheet:Staff!A7523:G7544 [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-huge-image-pdf [vector] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,fts]; eval-docs/hostile-broken-pdf [vector]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-truncated-docx [vector] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [vector,fts,trigram]; regulations-ru.docx · para:2 [vector,fts]; cyrillic-ru.pdf · page:1 [vector,fts]; eval-docs/hostile-truncated-docx [vector]; eval-docs/hostile-zip-bomb-docx [vector] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [vector,fts]; eval-docs/hostile-deep-nesting-docx [vector]; eval-docs/files-cyrillic-ru-pdf [vector]; eval-docs/hostile-huge-image-pdf [vector]; eval-docs/files-book-500-pdf [vector] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [vector,fts]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-deep-array-pdf [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/hostile-broken-pdf [vector] |
| x3-cross | cross-lang | — | — | — | — | eval-docs/files-onboarding-en-docx [vector]; eval-docs/files-regulations-ru-docx [vector]; cyrillic-ru.pdf · page:3 [vector,fts]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-truncated-docx [vector] |
| x4-date | date | 1 | sheet:Поставки!A5938:G5956 | нет | нет | inventory.xlsx · sheet:Поставки!A5938:G5956 [vector,trigram]; eval-docs/files-cjk-zh-pdf [vector]; eval-docs/hostile-js-action-pdf [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-huge-image-pdf [vector] |
| x5-date | date | 1 | sheet:Staff!A3558:G3579 | нет | нет | inventory.xlsx · sheet:Staff!A3558:G3579 [vector,fts]; eval-docs/files-inventory-xlsx [vector]; eval-docs/files-cjk-zh-pdf [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-glyph-flood-pdf [vector] |
| x6-date | date | 1 | sheet:Staff!A8330:G8351 | нет | нет | inventory.xlsx · sheet:Staff!A8330:G8351 [vector]; eval-docs/hostile-js-action-pdf [vector]; eval-docs/hostile-xxe-docx [vector]; eval-docs/hostile-broken-pdf [vector]; eval-docs/hostile-truncated-pdf [vector] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [vector,fts]; book-500.pdf · page:273 [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-deep-nesting-docx [vector]; eval-docs/files-inventory-xlsx [vector] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [vector,fts,trigram]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-js-action-pdf [vector]; eval-docs/files-onboarding-en-docx [vector] |
| p2-cross | cross-lang | 3 | slide:11 | да | да | onboarding-en.docx · para:30 [vector,fts]; twocol-en.pdf · page:6 [vector,fts]; roadmap.pptx · slide:11 [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; book-500.pdf · page:146 [vector] |
| p3-para | paraphrase | 4 | slide:3 | нет | нет | eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-onboarding-en-docx [vector]; eval-docs/hostile-truncated-docx [vector]; roadmap.pptx · slide:3 [vector,fts]; eval-docs/files-scan-pdf [vector] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [vector,fts]; eval-docs/files-roadmap-pptx [vector]; eval-docs/files-regulations-ru-docx [vector]; cjk-zh.pdf · page:1 [vector]; eval-docs/files-onboarding-en-docx [vector] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [vector,fts]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-twocol-en-pdf [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/files-onboarding-en-docx [vector] |
| o2-para | paraphrase | 2 | page:6 | нет | нет | eval-docs/files-scan-pdf [vector]; onecol-en.pdf · page:6 [vector,fts]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-cjk-zh-pdf [vector] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [vector,fts]; eval-docs/hostile-truncated-pdf [vector]; inventory.xlsx · sheet:Staff!A249:G270 [vector]; book-500.pdf · page:202 [vector]; eval-docs/hostile-truncated-docx [vector] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [vector,fts]; book-500.pdf · page:261 [vector]; eval-docs/hostile-glyph-flood-pdf [vector]; eval-docs/hostile-zip-bomb-docx [vector]; inventory.xlsx · sheet:Поставки!A4206:G4225 [vector] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [vector,fts,trigram]; onecol-en.pdf · page:6 [vector,trigram]; eval-docs/files-inventory-xlsx [vector]; eval-docs/files-scan-pdf [vector]; eval-docs/hostile-billion-laughs-xlsx [vector] |
| t2-para | paraphrase | 9 | — | — | — | eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-zip-bomb-docx [vector]; onboarding-en.docx · para:6 [vector,fts]; eval-docs/files-onboarding-en-docx [vector]; eval-docs/hostile-truncated-docx [vector] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [vector,fts]; eval-docs/hostile-js-action-pdf [vector]; eval-docs/hostile-xxe-docx [vector]; eval-docs/hostile-huge-image-pdf [vector]; eval-docs/hostile-deep-array-pdf [vector] |
| t4-cross | cross-lang | 3 | page:6 | да | да | regulations-ru.docx · para:61 [vector,fts]; cyrillic-ru.pdf · page:5 [vector,fts]; twocol-en.pdf · page:6 [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/hostile-truncated-pdf [vector] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [vector,fts]; inventory.xlsx · sheet:Поставки!A3655:G3674 [vector]; eval-docs/files-scan-pdf [vector]; eval-docs/hostile-truncated-pdf [vector]; regulations-ru.docx · para:61 [vector] |
| c2-para | paraphrase | 1 | page:5 | да | да | cyrillic-ru.pdf · page:5 [vector,fts]; eval-docs/files-onboarding-en-docx [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-xxe-docx [vector]; eval-docs/hostile-truncated-docx [vector] |
| c3-cross | cross-lang | — | — | — | — | book-500.pdf · page:88 [vector,fts]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/hostile-deep-nesting-docx [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/hostile-deep-array-pdf [vector] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [vector,fts]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-deep-nesting-docx [vector]; cyrillic-ru.pdf · page:5 [vector]; inventory.xlsx · sheet:Поставки!A2448:G2467 [vector] |
| k1-cross | cross-lang | 2 | page:2 | да | да | book-500.pdf · page:204 [vector,fts]; cjk-zh.pdf · page:2 [vector]; roadmap.pptx · slide:8 [vector]; eval-docs/hostile-deep-nesting-docx [vector]; eval-docs/hostile-billion-laughs-xlsx [vector] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [vector,fts]; eval-docs/files-regulations-ru-docx [vector]; inventory.xlsx · sheet:Staff!A7568:G7589 [vector]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-broken-pdf [vector] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [vector,fts]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-billion-laughs-xlsx [vector]; inventory.xlsx · sheet:Staff!A338:G359 [vector]; eval-docs/hostile-broken-pdf [vector] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [vector,fts]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/files-inventory-xlsx [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/files-book-500-pdf [vector] |
| b2-para | paraphrase | 1 | page:251 | да | да | book-500.pdf · page:251 [vector,fts]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/files-onboarding-en-docx [vector]; eval-docs/files-twocol-en-pdf [vector] |
| b3-cross | cross-lang | 4 | page:444 | да | да | cyrillic-ru.pdf · page:1 [vector,fts]; regulations-ru.docx · para:15 [vector,fts]; eval-docs/hostile-deep-array-pdf [vector]; book-500.pdf · page:444 [vector]; eval-docs/files-scan-pdf [vector] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [vector,fts]; eval-docs/hostile-truncated-pdf [vector]; eval-docs/hostile-broken-pdf [vector]; onboarding-en.docx · para:32 [vector]; eval-docs/hostile-truncated-docx [vector] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [vector,fts]; eval-docs/hostile-billion-laughs-xlsx [vector]; eval-docs/hostile-truncated-docx [vector]; eval-docs/hostile-zip-bomb-docx [vector]; eval-docs/files-cjk-zh-pdf [vector] |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 2.8 |
| `files/onboarding-en.docx` | content | done | done |  | false | 2.7 |
| `files/inventory.xlsx` | content | done | done |  | false | 12.2 |
| `files/roadmap.pptx` | content | done | done |  | false | 2.4 |
| `files/onecol-en.pdf` | content | done | done |  | false | 2.1 |
| `files/twocol-en.pdf` | content | done | done |  | false | 2.0 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 1.9 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 1.6 |
| `files/book-500.pdf` | content | done | done |  | false | 1.9 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 1.8 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio \| failed | done |  | false | 1.6 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 1.5 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 1.5 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error \| truncated: max_xml_depth | done |  | false | 1.4 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 1.3 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) \| failed | failed | parse_error: pdf | false | 1.2 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.2 |
| `hostile/deep-array.pdf` | hostile | done \| failed: parse_error (не parse_crash) | done |  | false | 1.1 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 2.4 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom \| parse_crash | done |  | true | 2.2 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |

Кэш извлечения `blob_extract`: 20 строк, zstd(JSON) 0.73 МБ, JSON 5.15 МБ (сжатие 7.1×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 42 | 6 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 69 | 105 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 60 | 5 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 63 | 4 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 4 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 28 | 88 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 22 | 2 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1074 | 1 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 3 | 1 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 2 | 1 | превью не построено: parse_error: pdf |
