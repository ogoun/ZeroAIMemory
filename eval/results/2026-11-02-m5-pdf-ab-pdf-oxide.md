# A/B извлечения PDF: pdf_oxide (2026-11-02)

Код — `251a72b+dirty.5d8f33dc`, набор `eval/docs/` (сид 0xd0c5e720261102). Каждый файл — в отдельном процессе `xtask docs-extract`; время — release, медиана повторов.

| Раскладка | Страниц | Доля строк | Доля верного порядка | Целые строки таблиц |
|---|---:|---|---|---|
| all | 79 | 0.996 [0.993–0.998] | 0.999 [0.997–1.000] | 11 из 24 |
| cjk | 4 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |
| one-column | 68 | 0.995 [0.992–0.997] | 1.000 [1.000–1.000] | — |
| table | 2 | 1.000 [1.000–1.000] | 0.958 [0.917–1.000] | 11 из 24 |
| two-column | 5 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | — |

Факты на своей странице: **19 из 19**. Книга на 500 страниц: 458 мс (1091 стр./с).

| Файл | Роль | Исход | мс | Пик, МиБ | Стр. с текстом | ocr_needed | Предупреждения |
|---|---|---|---:|---:|---:|---|---|
| `files/onecol-en.pdf` | content | ok | 11 | 16 | 10/10 | false |  |
| `files/twocol-en.pdf` | content | ok | 11 | 15 | 6/6 | false |  |
| `files/cyrillic-ru.pdf` | content | ok | 9 | 14 | 6/6 | false |  |
| `files/cjk-zh.pdf` | content | ok | 4 | 11 | 4/4 | false |  |
| `files/book-500.pdf` | content | ok | 458 | 36 | 500/500 | false |  |
| `files/scan.pdf` | scan | ok | 1 | 9 | 0/3 | false |  |
| `hostile/broken.pdf` | hostile | error (Invalid cross-reference table) | — | 9 | 0/0 | false |  |
| `hostile/truncated.pdf` | hostile | ok | 3 | 10 | 2/4 | false | skipped: 1 pages |
| `hostile/js-action.pdf` | hostile | ok | 1 | 9 | 1/1 | false |  |
| `hostile/deep-array.pdf` | hostile | crash (exit code: 0xc00000fd) | — | — | 0/0 | false |  |
| `hostile/glyph-flood.pdf` | hostile | ok | 93 | 84 | 1/1 | false |  |
| `hostile/huge-image.pdf` | hostile | ok | 4 | 23 | 0/1 | false |  |
