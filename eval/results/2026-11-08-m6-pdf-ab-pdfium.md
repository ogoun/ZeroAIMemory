# A/B извлечения PDF: pdfium (2026-11-08)

Код — `a4dd79c`, набор `eval/docs/` (сид 0xd0c5e720261102). Каждый файл — в отдельном процессе `xtask docs-extract`; время — release, медиана повторов.

| Раскладка | Страниц | Доля строк | Доля верного порядка | Целые строки таблиц |
|---|---:|---|---|---|
| all | 79 | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] | 0 из 24 |
| cjk | 4 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| one-column | 68 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| table | 2 | 1.000 [1.000–1.000] | 0.867 [0.818–0.917] | 0 из 24 |
| two-column | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |

Факты на своей странице: **19 из 19**. Книга на 500 страниц: 872 мс (574 стр./с).

| Файл | Роль | Исход | мс | Пик, МиБ | Стр. с текстом | ocr_needed | Предупреждения |
|---|---|---|---:|---:|---:|---|---|
| `files/regulations-ru.docx` | content | ok | 1 | 14 | 19/19 | false |  |
| `files/onboarding-en.docx` | content | ok | 1 | 14 | 13/13 | false |  |
| `files/inventory.xlsx` | content | ok | 1948 | 26 | 1338/1338 | false |  |
| `files/roadmap.pptx` | content | ok | 5 | 14 | 30/30 | false |  |
| `files/onecol-en.pdf` | content | ok | 20 | 20 | 10/10 | false |  |
| `files/twocol-en.pdf` | content | ok | 16 | 20 | 6/6 | false |  |
| `files/cyrillic-ru.pdf` | content | ok | 11 | 19 | 6/6 | false |  |
| `files/cjk-zh.pdf` | content | ok | 4 | 19 | 4/4 | false |  |
| `files/book-500.pdf` | content | ok | 872 | 36 | 500/500 | false |  |
| `files/scan.pdf` | scan | ok | 0 | 17 | 0/3 | true |  |
| `hostile/broken.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 17 | 0/0 | false |  |
| `hostile/truncated.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 17 | 0/0 | false |  |
| `hostile/js-action.pdf` | hostile | ok | 3 | 20 | 1/1 | false |  |
| `hostile/deep-array.pdf` | hostile | ok | 25 | 28 | 1/1 | false |  |
| `hostile/glyph-flood.pdf` | hostile | ok | 178 | 66 | 1/1 | false | truncated: page_chars |
| `hostile/huge-image.pdf` | hostile | ok | 1 | 22 | 0/1 | true |  |
