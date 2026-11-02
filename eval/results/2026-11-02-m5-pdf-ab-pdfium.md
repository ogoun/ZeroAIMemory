# A/B извлечения PDF: pdfium (2026-11-02)

Код — `3403134+dirty.ee3bb400`, набор `eval/docs/` (сид 0xd0c5e720261102). Каждый файл — в отдельном процессе `xtask docs-extract`; время — release, медиана повторов.

| Раскладка | Страниц | Доля строк | Доля верного порядка | Целые строки таблиц |
|---|---:|---|---|---|
| all | 79 | 1.000 [1.000–1.000] | 0.997 [0.991–1.000] | 0 из 24 |
| cjk | 4 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| one-column | 68 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| table | 2 | 1.000 [1.000–1.000] | 0.867 [0.818–0.917] | 0 из 24 |
| two-column | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |

Факты на своей странице: **19 из 19**. Книга на 500 страниц: 517 мс (967 стр./с).

| Файл | Роль | Исход | мс | Пик, МиБ | Стр. с текстом | ocr_needed | Предупреждения |
|---|---|---|---:|---:|---:|---|---|
| `files/regulations-ru.docx` | content | ok | 0 | 8 | 19/19 | false |  |
| `files/onboarding-en.docx` | content | ok | 0 | 8 | 13/13 | false |  |
| `files/inventory.xlsx` | content | ok | 167 | 20 | 1338/1338 | false |  |
| `files/roadmap.pptx` | content | ok | 2 | 8 | 30/30 | false |  |
| `files/onecol-en.pdf` | content | ok | 14 | 14 | 10/10 | false |  |
| `files/twocol-en.pdf` | content | ok | 12 | 14 | 6/6 | false |  |
| `files/cyrillic-ru.pdf` | content | ok | 8 | 12 | 6/6 | false |  |
| `files/cjk-zh.pdf` | content | ok | 4 | 12 | 4/4 | false |  |
| `files/book-500.pdf` | content | ok | 517 | 30 | 500/500 | false |  |
| `files/scan.pdf` | scan | ok | 0 | 11 | 0/3 | true |  |
| `hostile/broken.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 10 | 0/0 | false |  |
| `hostile/truncated.pdf` | hostile | error (документ повреждён: pdf: PdfiumLibraryInternalError(FormatEr) | — | 10 | 0/0 | false |  |
| `hostile/js-action.pdf` | hostile | ok | 3 | 13 | 1/1 | false |  |
| `hostile/deep-array.pdf` | hostile | ok | 27 | 22 | 1/1 | false |  |
| `hostile/glyph-flood.pdf` | hostile | ok | 131 | 60 | 1/1 | false | truncated: page_chars |
| `hostile/huge-image.pdf` | hostile | ok | 1 | 16 | 0/1 | true |  |
