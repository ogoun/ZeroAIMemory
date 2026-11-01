# Eval документов: fts без модели (cpu, PDF — pdfium)

Код — `251a72b+dirty.3fd008d5`, 2026-11-02; набор `eval/docs/` (сид 0xd0c5e720261102), запросов 45; заметки eval/corpus: нет. Извлечение 32 с, индексация после него 0 с. Сервер жив после набора: да.

recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).

| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |
|---|---:|---|---|---|---|---|---|
| **все** | 45 | 0.778 [0.667–0.889] | 0.778 [0.667–0.889] | 0.756 [0.644–0.867] | 0.667 [0.533–0.800] | 0.857 [0.743–0.971] | 0.857 [0.743–0.971] |
| cross-lang | 8 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | — | — |
| date | 3 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.833 [0.500–1.000] | 0.333 [0.000–1.000] | 0.333 [0.000–1.000] | 0.333 [0.000–1.000] |
| exact | 23 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] |
| paraphrase | 11 | 0.818 [0.545–1.000] | 0.818 [0.545–1.000] | 0.773 [0.545–1.000] | 0.545 [0.273–0.818] | 0.667 [0.333–1.000] | 0.667 [0.333–1.000] |
| docx | 12 | 0.750 [0.500–0.917] | 0.750 [0.500–0.917] | 0.708 [0.458–0.917] | 0.667 [0.417–0.917] | 0.889 [0.667–1.000] | 0.889 [0.667–1.000] |
| pdf | 20 | 0.750 [0.550–0.900] | 0.750 [0.550–0.900] | 0.750 [0.550–0.900] | 0.700 [0.500–0.900] | 0.933 [0.800–1.000] | 0.933 [0.800–1.000] |
| pptx | 5 | 0.800 [0.400–1.000] | 0.800 [0.400–1.000] | 0.800 [0.400–1.000] | 0.600 [0.200–1.000] | 0.750 [0.250–1.000] | 0.750 [0.250–1.000] |
| xlsx | 8 | 0.875 [0.625–1.000] | 0.875 [0.625–1.000] | 0.812 [0.562–1.000] | 0.625 [0.250–0.875] | 0.714 [0.286–1.000] | 0.714 [0.286–1.000] |

Латентность запросов набора (клиент): p50 5.0 мс, p95 34.6 мс.

## Запросы

| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |
|---|---|---:|---|---|---|---|
| d1-exact | exact | 1 | para:9 | да | да | regulations-ru.docx · para:9 [fts] |
| d1-para | paraphrase | 1 | para:9 | да | да | regulations-ru.docx · para:9 [fts] |
| d2-exact | exact | 1 | para:17 | да | да | regulations-ru.docx · para:17 [fts] |
| d3-para | paraphrase | — | — | — | — | inventory.xlsx · sheet:Склад!A3059:G3086 [fts] |
| d4-exact | exact | 1 | para:47 | да | да | regulations-ru.docx · para:47 [fts]; big-21m.docx · para:42653 [fts] |
| d4-cross | cross-lang | — | — | — | — | js-action.pdf · page:1 [fts]; roadmap.pptx · slide:4 [fts]; book-500.pdf · page:37 [fts]; onboarding-en.docx · para:2 [fts] |
| d5-para | paraphrase | 1 | para:61 | да | да | regulations-ru.docx · para:61 [fts]; big-300k.xlsx · sheet:Поставки!A186815:G186833 [fts]; inventory.xlsx · sheet:Поставки!A1120:G1138 [fts] |
| e1-exact | exact | 1 | para:6 | да | да | onboarding-en.docx · para:6 [fts] |
| e1-cross | cross-lang | — | — | — | — | big-21m.docx · para:35348 [fts]; regulations-ru.docx · para:17 [fts] |
| e2-exact | exact | 1 | para:11 | да | да | onboarding-en.docx · para:11 [fts] |
| e3-para | paraphrase | 1 | para:30 | да | да | onboarding-en.docx · para:30 [fts]; inventory.xlsx · sheet:Staff!A5016:G5037 [fts] |
| e4-para | paraphrase | 2 | para:34 | нет | нет | onecol-en.pdf · page:9 [fts]; onboarding-en.docx · para:34 [fts]; inventory.xlsx · sheet:Staff!A2185:G2206 [fts] |
| x1-exact | exact | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [fts] |
| x1-para | paraphrase | 1 | sheet:Поставки!A1218:G1236 | да | да | inventory.xlsx · sheet:Поставки!A1218:G1236 [fts,trigram]; big-21m.docx · para:3650 [fts] |
| x2-exact | exact | 1 | sheet:Склад!A7771:G7795 | да | да | inventory.xlsx · sheet:Склад!A7771:G7795 [fts] |
| x3-exact | exact | 1 | sheet:Staff!A5016:G5037 | да | да | inventory.xlsx · sheet:Staff!A5016:G5037 [fts] |
| x3-cross | cross-lang | — | — | — | — | regulations-ru.docx · para:61 [fts]; big-21m.docx · para:14383 [fts] |
| x4-date | date | 1 | sheet:Поставки!A8995:G9013 | да | да | inventory.xlsx · sheet:Поставки!A8995:G9013 [fts,trigram]; big-300k.xlsx · sheet:Поставки!A202162:G202180 [fts] |
| x5-date | date | 2 | sheet:Staff!A203:G225 | нет | нет | big-300k.xlsx · sheet:Поставки!A202046:G202065 [fts]; inventory.xlsx · sheet:Staff!A203:G225 [fts] |
| x6-date | date | 1 | sheet:Staff!A181:G202 | нет | нет | inventory.xlsx · sheet:Staff!A181:G202 [fts]; big-300k.xlsx · sheet:Поставки!A201946:G201965 [fts] |
| p1-exact | exact | 1 | slide:4 | да | да | roadmap.pptx · slide:4 [fts] |
| p2-exact | exact | 1 | slide:11 | да | да | roadmap.pptx · slide:11 [fts,trigram] |
| p2-cross | cross-lang | — | — | — | — | onboarding-en.docx · para:30 [fts]; twocol-en.pdf · page:6 [fts]; book-500.pdf · page:335 [fts]; onecol-en.pdf · page:1 [fts] |
| p3-para | paraphrase | 1 | slide:3 | нет | нет | roadmap.pptx · slide:3 [fts]; book-500.pdf · page:251 [fts]; onecol-en.pdf · page:1 [fts]; twocol-en.pdf · page:4 [fts] |
| p4-exact | exact | 1 | slide:9 | да | да | roadmap.pptx · slide:9 [fts] |
| o1-exact | exact | 1 | page:3 | да | да | onecol-en.pdf · page:3 [fts] |
| o2-para | paraphrase | 1 | page:7 | да | да | onecol-en.pdf · page:7 [fts]; book-500.pdf · page:251 [fts]; roadmap.pptx · slide:4 [fts]; onboarding-en.docx · para:35 [fts]; twocol-en.pdf · page:5 [fts] |
| o3-exact | exact | 1 | page:9 | да | да | onecol-en.pdf · page:9 [fts] |
| o4-exact | exact | 1 | page:6 | да | да | onecol-en.pdf · page:6 [fts] |
| t1-exact | exact | 1 | page:2 | да | да | twocol-en.pdf · page:2 [fts,trigram]; big-300k.xlsx · sheet:Поставки!A129994:G130012 [trigram]; onecol-en.pdf · page:6 [trigram]; inventory.xlsx · sheet:Склад!A3274:G3299 [trigram] |
| t2-para | paraphrase | 1 | page:5 | нет | нет | twocol-en.pdf · page:5 [fts]; onboarding-en.docx · para:35 [fts]; roadmap.pptx · slide:17 [fts]; book-500.pdf · page:444 [fts] |
| t3-exact | exact | 1 | page:5 | да | да | twocol-en.pdf · page:5 [fts] |
| t4-cross | cross-lang | — | — | — | — | big-21m.docx · para:35348 [fts]; regulations-ru.docx · para:17 [fts] |
| c1-exact | exact | 1 | page:2 | да | да | cyrillic-ru.pdf · page:2 [fts] |
| c2-para | paraphrase | — | — | — | — | big-21m.docx · para:9696 [fts] |
| c3-cross | cross-lang | — | — | — | — | roadmap.pptx · slide:17 [fts]; book-500.pdf · page:362 [fts] |
| k1-exact | exact | 1 | page:2 | да | да | cjk-zh.pdf · page:2 [fts] |
| k1-cross | cross-lang | — | — | — | — | onecol-en.pdf · page:9 [fts]; book-500.pdf · page:313 [fts] |
| k2-exact | exact | 1 | page:3 | да | да | cjk-zh.pdf · page:3 [fts] |
| k3-exact | exact | 1 | page:4 | да | да | cjk-zh.pdf · page:4 [fts] |
| b1-exact | exact | 1 | page:37 | да | да | book-500.pdf · page:37 [fts] |
| b2-para | paraphrase | 1 | page:251 | да | да | book-500.pdf · page:251 [fts]; onecol-en.pdf · page:1 [fts]; twocol-en.pdf · page:4 [fts] |
| b3-cross | cross-lang | — | — | — | — | big-21m.docx · para:12743 [fts] |
| b4-exact | exact | 1 | page:498 | да | да | book-500.pdf · page:498 [fts] |
| j1-exact | exact | 1 | page:1 | да | да | js-action.pdf · page:1 [fts] |

## Документы

| Файл | Роль | Ожидание | Состояние | Причина | ocr_needed | с до конца |
|---|---|---|---|---|---|---:|
| `files/regulations-ru.docx` | content | done | done |  | false | 2.6 |
| `files/onboarding-en.docx` | content | done | done |  | false | 2.5 |
| `files/inventory.xlsx` | content | done | done |  | false | 2.4 |
| `files/roadmap.pptx` | content | done | done |  | false | 2.4 |
| `files/onecol-en.pdf` | content | done | done |  | false | 2.4 |
| `files/twocol-en.pdf` | content | done | done |  | false | 2.3 |
| `files/cyrillic-ru.pdf` | content | done | done |  | false | 2.2 |
| `files/cjk-zh.pdf` | content | done | done |  | false | 1.6 |
| `files/book-500.pdf` | content | done | done |  | false | 1.4 |
| `files/scan.pdf` | scan | done, ocr_needed | done |  | true | 1.4 |
| `hostile/zip-bomb.docx` | hostile | truncated: max_ratio \| failed | done |  | false | 1.3 |
| `hostile/xxe.docx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (word/document.xml) | false | 1.3 |
| `hostile/billion-laughs.xlsx` | hostile | failed: parse_error (DTD) | failed | parse_error: xml (xl/sharedStrings.xml) | false | 1.3 |
| `hostile/deep-nesting.docx` | hostile | failed: parse_error \| truncated: max_xml_depth | done |  | false | 1.2 |
| `hostile/broken.pdf` | hostile | failed: parse_error | failed | parse_error: pdf | false | 1.1 |
| `hostile/truncated.pdf` | hostile | done (восстановление xref) \| failed | failed | parse_error: pdf | false | 1.1 |
| `hostile/js-action.pdf` | hostile | done, JS не исполняется | done |  | false | 1.1 |
| `hostile/deep-array.pdf` | hostile | done \| failed: parse_error (не parse_crash) | done |  | false | 1.0 |
| `hostile/glyph-flood.pdf` | hostile | done, truncated: page_chars | done |  | false | 0.9 |
| `hostile/huge-image.pdf` | hostile | done (текста нет); превью thumb — parse_oom \| parse_crash | done |  | true | 0.1 |
| `hostile/truncated.docx` | hostile | failed: parse_error (zip) | unsupported |  | false | 0.0 |
| `large/big-21m.docx` | large | done, truncated: max_chars | done |  | false | 13.4 |
| `large/big-300k.xlsx` | large | done, truncated: max_cells | done |  | false | 12.1 |

Кэш извлечения `blob_extract`: 22 строк, zstd(JSON) 7.19 МБ, JSON 69.41 МБ (сжатие 9.7×).

## Превью

| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |
|---|---|---:|---:|---:|---|
| `files/book-500.pdf` | thumb стр. 1, 256 px | 200 | 39 | 7 |  |
| `files/inventory.xlsx` | sheet 1, строки 1–100 | 200 | 69 | 3 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 2, строки 1–100 | 200 | 63 | 3 | total_rows 10000 |
| `files/inventory.xlsx` | sheet 3, строки 1–100 | 200 | 63 | 70 | total_rows 10000 |
| `hostile/broken.pdf` | thumb стр. 1, 256 px | 422 | 3 | 1 | превью не построено: parse_error: pdf |
| `hostile/deep-array.pdf` | thumb стр. 1, 256 px | 200 | 30 | 2 |  |
| `hostile/glyph-flood.pdf` | thumb стр. 1, 256 px | 200 | 22 | 2 |  |
| `hostile/huge-image.pdf` | thumb стр. 1, 256 px | 200 | 1069 | 2 |  |
| `hostile/js-action.pdf` | thumb стр. 1, 256 px | 200 | 3 | 1 |  |
| `hostile/truncated.pdf` | thumb стр. 1, 256 px | 422 | 2 | 1 | превью не построено: parse_error: pdf |
| `large/big-300k.xlsx` | sheet 1, строки 1–100 | 200 | 1685 | 63 | total_rows 299972 |
| `large/big-300k.xlsx` | sheet 2, строки 1–100 | 422 | 28 | 1 | превью не построено: parse_error: нет листа №2 |
| `large/big-300k.xlsx` | sheet 3, строки 1–100 | 422 | 33 | 1 | превью не построено: parse_error: нет листа №3 |
