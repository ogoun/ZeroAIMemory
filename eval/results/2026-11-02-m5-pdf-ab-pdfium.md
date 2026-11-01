# A/B извлечения PDF: pdfium (2026-11-02)

Код — `251a72b+dirty.3fd008d5`, набор `eval/docs/` (сид 0xd0c5e720261102). Каждый файл — в отдельном процессе `xtask docs-extract`; время — release, медиана повторов.

| Раскладка | Страниц | Доля строк | Доля верного порядка | Целые строки таблиц |
|---|---:|---|---|---|
| all | 79 | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] | 0 из 24 |
| cjk | 4 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| one-column | 68 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| table | 2 | 1.000 [1.000–1.000] | 0.867 [0.818–0.917] | 0 из 24 |
| two-column | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |

Факты на своей странице: **19 из 19**. Книга на 500 страниц: 495 мс (1009 стр./с).

| Файл | Роль | Исход | мс | Пик, МиБ | Стр. с текстом | ocr_needed | Предупреждения |
|---|---|---|---:|---:|---:|---|---|
| `files/regulations-ru.docx` | content | ok | 0 | 8 | 19/19 | false |  |
| `files/onboarding-en.docx` | content | ok | 0 | 8 | 13/13 | false |  |
| `files/inventory.xlsx` | content | ok | 170 | 20 | 1338/1338 | false |  |
| `files/roadmap.pptx` | content | ok | 2 | 8 | 30/30 | false |  |
| `files/onecol-en.pdf` | content | ok | 14 | 13 | 10/10 | false |  |
| `files/twocol-en.pdf` | content | ok | 11 | 14 | 6/6 | false |  |
| `files/cyrillic-ru.pdf` | content | ok | 7 | 12 | 6/6 | false |  |
| `files/cjk-zh.pdf` | content | ok | 4 | 12 | 4/4 | false |  |
| `files/book-500.pdf` | content | ok | 495 | 30 | 500/500 | false |  |
| `files/scan.pdf` | scan | ok | 0 | 10 | 0/3 | true |  |
| `hostile/broken.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 10 | 0/0 | false |  |
| `hostile/truncated.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 10 | 0/0 | false |  |
| `hostile/js-action.pdf` | hostile | ok | 3 | 13 | 1/1 | false |  |
| `hostile/deep-array.pdf` | hostile | ok | 26 | 21 | 1/1 | false |  |
| `hostile/glyph-flood.pdf` | hostile | ok | 129 | 60 | 1/1 | false | truncated: page_chars |
| `hostile/huge-image.pdf` | hostile | ok | 1 | 15 | 0/1 | true |  |
| `large/big-21m.docx` | large | ok | 148 | 103 | 14967/14967 | false | truncated: max_chars |
| `large/big-300k.xlsx` | large | ok | 1293 | 152 | 10599/10599 | false | truncated: max_chars |
