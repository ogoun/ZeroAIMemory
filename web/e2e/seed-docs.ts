// Заметки с вложениями для E2E просмотрщиков (M5): документы собираются docs-fixtures.ts,
// загружаются по первому токену администратора; global-setup ждёт извлечения текста (поиск
// по вложениям — полнотекстовый, pdfium — из target/deps).
import { makeBrokenDocx, makeDocx, makePdf, makePptx, makeXlsx, makeZipBombDocx } from './docs-fixtures';

/** Уникальные слова документов: поиск находит фрагмент вложения с `locator`. */
export const WORDS = {
  pdf: 'pdfzebra',
  docx: 'docxkiwi',
  xlsx: 'xlsxmango',
  pptx: 'pptxlemon',
} as const;

/** Строка листа «Данные» со словом `WORDS.xlsx` (номер строки листа). */
export const XLSX_ROW = 300;
/** Заголовок раздела DOCX, с которого начинается фрагмент со словом `WORDS.docx`. */
export const DOCX_SECTION = 'Раздел киви';

export const FILES = {
  pdf: 'отчёт.pdf',
  docx: 'договор.docx',
  xlsx: 'таблица.xlsx',
  pptx: 'доклад.pptx',
  /** Книга без секретов с пустым первым листом: `sheet:#1` — второй лист книги. */
  nums: 'номера.xlsx',
  bomb: 'бомба.docx',
  svg: 'схема.svg',
  broken: 'сломанный.docx',
  scan: 'скан.pdf',
  text: 'заметки.txt',
  extra: 'лишний.txt',
} as const;

/** Враждебные вставки: исполнение любой из них оставит метку `__pwned` в окне WebUI. */
const HOSTILE_HTML =
  '<script>parent.__pwned = "altchunk-script"</script>' +
  '<img src="x" onerror="parent.__pwned = \'altchunk-img\'"><p>altchunk text</p>';
const HOSTILE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60" onload="parent.__pwned=\'svg-onload\'">' +
  '<script>parent.__pwned = "svg-script"</script><rect width="120" height="60" fill="#2f6fed"/></svg>';

const pad = (i: number) => `Абзац ${i}: ${'обычный текст договора без особых слов. '.repeat(2)}`;

async function api<T>(base: string, token: string, method: string, path: string, init: RequestInit = {}): Promise<T> {
  const r = await fetch(`${base}api/v1/${path}`, {
    ...init,
    method,
    headers: { Authorization: `Bearer ${token}`, ...(init.headers ?? {}) },
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (r.status === 204 ? undefined : await r.json()) as T;
}

async function note(base: string, token: string, path: string, content: string): Promise<string> {
  const n = await api<{ id: string }>(base, token, 'POST', 'nodes', {
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ space: 'knowledge', path, kind: 'note', content }),
  });
  return n.id;
}

async function upload(base: string, token: string, node: string, name: string, data: Buffer | string, type: string) {
  const form = new FormData();
  form.append('node', node);
  const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : new Uint8Array(data);
  form.append('file', new Blob([bytes], { type }), name);
  await api(base, token, 'POST', 'blobs', { body: form });
}

interface Doc {
  filename: string;
  extract_state: string;
}

/** Ждёт конца извлечения текста всех вложений заметок (не `pending`). */
async function waitExtracted(base: string, token: string, nodes: string[]) {
  const t0 = Date.now();
  for (;;) {
    const all = (await Promise.all(nodes.map((n) => api<Doc[]>(base, token, 'GET', `nodes/${n}/attachments`)))).flat();
    const pending = all.filter((d) => d.extract_state === 'pending');
    if (!pending.length) return;
    if (Date.now() - t0 > 120_000) {
      throw new Error(`извлечение не закончилось за 120 с: ${pending.map((d) => d.filename).join(', ')}`);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
}

export interface DocsSeed {
  /** Заметка с документами для просмотрщиков. */
  docs: string;
  /** Заметка для списка вложений и открепления. */
  attach: string;
  /** Заметка с zip-бомбой под видом DOCX. */
  bomb: string;
}

export async function seedDocs(base: string, token: string): Promise<DocsSeed> {
  const docs = await note(base, token, 'E2E/Документы', '# Документы\n\nВложения для просмотрщиков.\n');
  const attach = await note(base, token, 'E2E/Вложения', '# Вложения\n\nСписок вложений заметки.\n');
  const bomb = await note(base, token, 'E2E/Бомба', '# Бомба\n\nВраждебный DOCX.\n');

  const pdf = makePdf(
    [
      { lines: ['Report intro page one', 'Nothing special here'] },
      { lines: ['Second page of the report', `This page mentions ${WORDS.pdf} quartz`] },
      { lines: ['Third page closing words'] },
    ],
    "app.alert('pdf-js-pwned')",
  );
  await upload(base, token, docs, FILES.pdf, pdf, 'application/pdf');

  const paras = [
    { heading: 'Введение' },
    { text: 'Опасная ссылка', link: 'javascript:parent.__pwned="docx-link"' },
    { text: 'Внешний сайт', link: 'https://example.test/docx' },
    ...Array.from({ length: 30 }, (_, i) => pad(i + 4)),
    { heading: DOCX_SECTION },
    `Здесь встречается ${WORDS.docx} в тексте раздела.`,
    ...Array.from({ length: 10 }, (_, i) => `Хвост ${i + 1}.`),
  ];
  await upload(
    base,
    token,
    docs,
    FILES.docx,
    await makeDocx(paras, HOSTILE_HTML),
    'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  );

  const data: string[][] = [['Код', 'Описание', 'Флаг']];
  for (let r = 2; r <= 400; r++) {
    data.push([`R${r}`, r === XLSX_ROW ? `строка с ${WORDS.xlsx}` : `значение ${r}`, r % 2 ? 'нет' : 'да']);
  }
  await upload(
    base,
    token,
    docs,
    FILES.xlsx,
    await makeXlsx([
      ['Итоги', [['Показатель', 'Сумма'], ['Выручка', '100'], ['Расход', '40']]],
      ['Данные', data],
    ]),
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  );

  await upload(
    base,
    token,
    docs,
    FILES.pptx,
    await makePptx([
      ['Титульный слайд', 'Доклад о памяти'],
      ['План', 'Три части'],
      ['Часть первая', 'Устройство хранилища'],
      ['Часть вторая', `Слово ${WORDS.pptx} на четвёртом слайде`],
      ['Итоги', 'Спасибо за внимание'],
    ]),
    'application/vnd.openxmlformats-officedocument.presentationml.presentation',
  );

  await upload(
    base,
    token,
    docs,
    FILES.nums,
    await makeXlsx([
      ['Пусто', []],
      ['Цифры', [['Код', 'Число'], ['N1', 'один'], ['N2', 'два']]],
    ]),
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  );
  // 320 МиБ распаковки при заявленных 100 Б (предел просмотра — 256 МиБ)
  await upload(base, token, bomb, FILES.bomb, makeZipBombDocx(320 * 1024 * 1024), 'application/octet-stream');

  await upload(base, token, attach, FILES.svg, HOSTILE_SVG, 'image/svg+xml');
  // настоящий DOCX с битым XML тела: разбор — failed parse_error
  await upload(base, token, attach, FILES.broken, await makeBrokenDocx(), 'application/octet-stream');
  await upload(base, token, attach, FILES.scan, makePdf([{ image: true }]), 'application/pdf');
  await upload(base, token, attach, FILES.text, 'Обычные текстовые заметки.\n', 'text/plain');
  await upload(base, token, attach, FILES.extra, 'Это вложение открепляется в тесте.\n', 'text/plain');

  await waitExtracted(base, token, [docs, attach, bomb]);
  return { docs, attach, bomb };
}
