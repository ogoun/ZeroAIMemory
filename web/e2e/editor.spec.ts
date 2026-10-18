// Сквозные сценарии правки в WebUI: редактор (CodeMirror) с предпросмотром, сохранение
// по If-Match, конфликт, вложения, история и откат, поиск Ctrl+K, переводы строк CRLF,
// навигация по якорям и стартовый экран. Данные — seed.ts и заметки, созданные здесь.
import { expect, request as pwRequest, test, type APIRequestContext, type Page } from '@playwright/test';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });

/**
 * Ошибки JS, CSP и неожиданные HTTP-ответы страницы; `allow` — ожидаемые в сценарии
 * ответы вида `409 PUT` (статус и метод).
 */
function watch(page: Page, allow: string[] = []): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) ||
      (r.status() === 404 && path.endsWith('/favicon.ico')) ||
      allow.includes(`${r.status()} ${r.request().method()}`);
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

/** Открывает приложение с общей сессией (вход — один раз в global-setup); id своего пользователя. */
async function login(page: Page): Promise<string> {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  return /#\/u\/([0-9A-Za-z]+)$/.exec(page.url())![1];
}

/** REST API по токену администратора — правки «с другой машины». */
async function api(): Promise<APIRequestContext> {
  return pwRequest.newContext({
    baseURL: `${base()}api/v1/`,
    extraHTTPHeaders: { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` },
  });
}

interface Node {
  id: string;
  current_rev: number;
}

async function createNote(a: APIRequestContext, path: string, content: string): Promise<Node> {
  const r = await a.post('nodes', { data: { space: 'knowledge', path, kind: 'note', content } });
  expect(r.status(), await r.text()).toBe(201);
  return (await r.json()) as Node;
}

async function putNote(a: APIRequestContext, id: string, rev: number, content: string): Promise<Node> {
  const r = await a.put(`nodes/${id}`, { data: { content }, headers: { 'If-Match': String(rev) } });
  expect(r.status(), await r.text()).toBe(200);
  return (await r.json()) as Node;
}

/** Байты текущей ревизии как сохранены (format=md). */
async function rawText(a: APIRequestContext, id: string): Promise<string> {
  const r = await a.get(`nodes/${id}?format=md`);
  expect(r.status()).toBe(200);
  return Buffer.from(await r.body()).toString('utf8');
}

/** Уникальное слово из букв (токенайзер FTS не дробит его). */
function uniqueWord(): string {
  return `зэм${[...String(Date.now())].map((d) => 'абвгдежзик'[Number(d)]).join('')}`;
}

const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');
const editorContent = (page: Page) => page.locator('.editor .cm-content');

/** «Править» → редактор загружен (ревизия в строке состояния). */
async function openEditor(page: Page, rev: number) {
  await page.getByRole('button', { name: 'Править' }).click();
  await expect(editorContent(page)).toBeVisible();
  await expect(page.locator('.editor .state')).toHaveText(`Ревизия ${rev}`);
}

/** Курсор в конец текста и ввод строк (Enter между ними). */
async function typeAtEnd(page: Page, ...lines: string[]) {
  await editorContent(page).click();
  await page.keyboard.press('Control+End');
  for (const l of lines) {
    if (l === '\n') await page.keyboard.press('Enter');
    else await page.keyboard.type(l);
  }
}

test('правка: создание, ввод, Ctrl+S, поиск Ctrl+K, история, дифф, откат', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const t = tree(page);
  const name = `Редактор ${Date.now()}`;
  const word = uniqueWord();

  // создать заметку из контекстного меню пространства
  await t.getByRole('button', { name: 'Действия: Knowledge' }).click();
  await page.getByRole('menuitem', { name: 'Новая заметка' }).click();
  const dlg = page.getByRole('dialog');
  await dlg.getByLabel('Имя').fill(name);
  await dlg.getByRole('button', { name: 'Создать' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: new RegExp(`^${name}`) })).toBeVisible();
  const url = page.url();

  // напечатать текст: предпросмотр обновляется, Ctrl+S сохраняет ревизию 2
  await openEditor(page, 1);
  await typeAtEnd(page, '\n', `Строка со словом ${word}.`);
  await expect(page.locator('.editor .state')).toHaveText('Есть несохранённые изменения');
  await expect(page.locator('.editor .preview .doc')).toContainText(`Строка со словом ${word}.`);
  await page.keyboard.press('Control+s');
  await expect(page.locator('.editor .state')).toHaveText('Сохранено: ревизия 2');
  await expect(page.locator('.doc-head .meta')).toContainText('ревизия 2');
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(page.locator('.doc')).toContainText(word);

  // Ctrl+K находит заметку по слову; Enter — переход к разделу
  await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await expect(search).toBeFocused();
  await search.fill(word);
  const option = page.getByRole('option').filter({ hasText: name });
  await expect(option).toBeVisible();
  await expect(option.locator('mark')).toHaveText(word);
  await expect(option).toContainText(`Knowledge / ${name} › ${name}`);
  await search.press('Enter');
  await expect(page.getByRole('dialog')).toBeHidden();
  await expect(page).toHaveURL(new RegExp(`${url.split('#/')[1].replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}#h-`));
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: new RegExp(`^${name}`) })).toBeInViewport();

  // история: 2 ревизии, дифф содержит добавленную строку
  await page.getByRole('tab', { name: 'История' }).click();
  const rows = page.locator('table[aria-label="Ревизии"] tbody tr');
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toContainText('человек');
  await expect(page.locator('.diff .add').filter({ hasText: word })).toHaveText(`+Строка со словом ${word}.`);

  // откат к ревизии 1 возвращает текст
  await rows.nth(1).getByRole('button', { name: 'Откатить к этой' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Откатить' }).click();
  await expect(page.getByRole('dialog')).toBeHidden();
  await expect(rows).toHaveCount(3);
  await expect(page.locator('.doc-head .meta')).toContainText('ревизия 3');
  await page.getByRole('tab', { name: 'Документ' }).click();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: new RegExp(`^${name}`) })).toBeVisible();
  await expect(page.locator('.doc')).not.toContainText(word);

  expect(problems, problems.join('\n')).toEqual([]);
});

test('конфликт: правка через API между открытием редактора и сохранением', async ({ page, context }) => {
  const problems = watch(page, ['409 PUT']);
  const uid = await login(page);
  const a = await api();
  const n = await createNote(a, `E2E/Конфликт ${Date.now()}`, '# Конфликт\n\nисходный текст\n');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${n.id}`);
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Конфликт/ })).toBeVisible();

  await openEditor(page, 1);
  const other = await putNote(a, n.id, 1, '# Конфликт\n\nисходный текст\nчужая строка\n');
  await typeAtEnd(page, 'моя строка');
  await page.keyboard.press('Control+s');

  const dlg = page.getByRole('dialog', { name: 'Заметка изменена в другом месте' });
  await expect(dlg).toBeVisible();
  await expect(dlg.locator('.diff .add')).toHaveText('+чужая строка');

  // «Перезаписать поверх» — PUT с текущей ревизией
  await dlg.getByRole('button', { name: 'Перезаписать поверх' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.locator('.editor .state')).toHaveText(`Сохранено: ревизия ${other.current_rev + 1}`);
  expect(await rawText(a, n.id)).toBe('# Конфликт\n\nисходный текст\nмоя строка');

  // второй конфликт: «Скопировать мой текст и открыть текущую»
  await putNote(a, n.id, other.current_rev + 1, '# Конфликт\n\nверсия с другой машины\n');
  await typeAtEnd(page, ' и ещё');
  await page.keyboard.press('Control+s');
  await expect(dlg).toBeVisible();
  await dlg.getByRole('button', { name: 'Скопировать мой текст и открыть текущую' }).click();
  await expect(dlg).toBeHidden();
  await expect(editorContent(page)).toContainText('версия с другой машины');
  await expect(editorContent(page)).not.toContainText('моя строка');
  await expect(page.locator('.editor .state')).toContainText('открыта текущая ревизия 4');
  const port = new URL(base()).port;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(`http://localhost:${port}/`);
  const copied = await probe.evaluate(() => navigator.clipboard.readText());
  expect(copied.replace(/\r\n/g, '\n')).toBe('# Конфликт\n\nисходный текст\nмоя строка и ещё');
  await probe.close();

  await a.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('CRLF: сохранение не меняет переводы строк', async ({ page }) => {
  const problems = watch(page);
  const uid = await login(page);
  const a = await api();
  const n = await createNote(a, `E2E/CRLF ${Date.now()}`, '# CRLF\r\n\r\nстрока один\r\n');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${n.id}`);
  await openEditor(page, 1);
  await typeAtEnd(page, 'строка два', '\n', '- пункт', '\n', 'второй');
  await page.keyboard.press('Control+s');
  await expect(page.locator('.editor .state')).toHaveText('Сохранено: ревизия 2');
  // Enter в списке продолжает разметку — тем же переводом строки документа
  expect(await rawText(a, n.id)).toBe('# CRLF\r\n\r\nстрока один\r\nстрока два\r\n- пункт\r\n- второй');
  await a.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('вложение: «Прикрепить файл», уход с несохранёнными правками', async ({ page }) => {
  const problems = watch(page);
  const uid = await login(page);
  const a = await api();
  const n = await createNote(a, `E2E/Вложения ${Date.now()}`, '# Вложения\n\n');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${n.id}`);
  await openEditor(page, 1);
  await typeAtEnd(page, '');

  // PNG 1×1 → встраивание ![…](…) в позицию курсора, картинка в предпросмотре
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
    'base64',
  );
  const input = page.locator('.editor input[type="file"]');
  await input.setInputFiles({ name: 'точка e2e.png', mimeType: 'image/png', buffer: png });
  await expect(page.locator('.editor .state')).toHaveText('Файл «точка e2e.png» прикреплён');
  await expect(editorContent(page)).toContainText('![точка e2e.png](_attachments/');
  const img = page.locator('.editor .preview img[alt="точка e2e.png"]');
  await expect
    .poll(() => img.evaluate((i: HTMLImageElement) => (i.complete ? i.naturalWidth : 0)))
    .toBe(1);
  // не медиа — обычная ссылка
  await page.keyboard.press('Enter');
  await input.setInputFiles({ name: 'заметки.txt', mimeType: 'text/plain', buffer: Buffer.from('текст') });
  await expect(page.locator('.editor .state')).toHaveText('Файл «заметки.txt» прикреплён');
  await expect(editorContent(page)).toContainText('[заметки.txt](_attachments/');
  await expect(editorContent(page)).not.toContainText('![заметки.txt]');

  // перетаскивание двух файлов: загрузка по очереди, по ссылке на строку
  await page.keyboard.press('Enter');
  const files = (names: { name: string; type: string; text?: string }[]) =>
    page.evaluateHandle(
      ({ names, b64 }) => {
        const dt = new DataTransfer();
        for (const f of names) {
          const body = f.text ?? Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
          dt.items.add(new File([body], f.name, { type: f.type }));
        }
        return dt;
      },
      { names, b64: png.toString('base64') },
    );
  const dropped = await files([
    { name: 'сброс.png', type: 'image/png' },
    { name: 'сброс.txt', type: 'text/plain', text: 'abc' },
  ]);
  // сброс — на последнюю (пустую) строку: вставка по координатам, а не по курсору
  await editorContent(page).click({ position: { x: 5, y: 5 } });
  const box = (await page.locator('.editor .cm-line').last().boundingBox())!;
  await editorContent(page).dispatchEvent('drop', {
    dataTransfer: dropped,
    clientX: box.x + 4,
    clientY: box.y + box.height / 2,
  });
  await expect(page.locator('.editor .state')).toHaveText('Файл «сброс.txt» прикреплён');
  const lines = page.locator('.editor .cm-line');
  await expect(lines.filter({ hasText: /^!\[сброс\.png\]\(_attachments\/[0-9a-f]{16}\/сброс\.png\)$/ })).toHaveCount(1);
  await expect(lines.filter({ hasText: /^\[сброс\.txt\]\(_attachments\/[0-9a-f]{16}\/сброс\.txt\)$/ })).toHaveCount(1);
  const droppedImg = page.locator('.editor .preview img[alt="сброс.png"]');
  await expect
    .poll(() => droppedImg.evaluate((i: HTMLImageElement) => (i.complete ? i.naturalWidth : 0)))
    .toBe(1);

  // вставка файла из буфера — в позицию курсора
  await page.keyboard.press('Control+End');
  await page.keyboard.press('Enter');
  const pasted = await files([{ name: 'вставка.png', type: 'image/png' }]);
  await editorContent(page).evaluate((el, dt) => {
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  }, pasted);
  await expect(page.locator('.editor .state')).toHaveText('Файл «вставка.png» прикреплён');
  await expect(lines.last()).toHaveText(/^!\[вставка\.png\]\(_attachments\/[0-9a-f]{16}\/вставка\.png\)$/);

  // уход по дереву с несохранёнными правками: «остаться», затем «уйти»
  const t = tree(page);
  const other = t.getByRole('link', { name: 'Вторая заметка' });
  const here = page.url();
  page.once('dialog', (d) => void d.dismiss());
  await other.click();
  await expect(page).toHaveURL(here);
  await expect(editorContent(page)).toBeVisible();
  page.once('dialog', (d) => void d.accept());
  await other.click();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Вторая заметка/ })).toBeVisible();
  await expect(page.locator('.editor')).toHaveCount(0);
  // правки не сохранены: ревизия прежняя
  expect(await rawText(a, n.id)).toBe('# Вложения\n\n');

  await a.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ошибка перечитывания после сохранения не закрывает редактор; дифф строки ---', async ({ page }) => {
  const problems = watch(page);
  const uid = await login(page);
  const a = await api();
  const n = await createNote(a, `E2E/Линия ${Date.now()}`, '# Линия\n\n---\n\nтекст\n');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${n.id}`);
  await openEditor(page, 1);

  // перечитывание документа после сохранения обрывается
  await page.route('**/api/v1/nodes/*?format=html', (r) => r.abort());
  await editorContent(page).click();
  await page.keyboard.press('Control+a');
  await page.keyboard.type('# Линия\n\nтекст\n');
  await page.keyboard.press('Control+s');
  await expect(page.locator('.editor .state')).toHaveText('Сохранено: ревизия 2');
  await expect(page.locator('.docview > .error')).toBeVisible();
  await typeAtEnd(page, 'несохранённое');
  await expect(editorContent(page)).toContainText('несохранённое');
  await expect(page.locator('.editor')).toBeVisible();
  await page.unroute('**/api/v1/nodes/*?format=html');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('.editor .state')).toHaveText('Сохранено: ревизия 3');
  expect(await rawText(a, n.id)).toBe('# Линия\n\nтекст\nнесохранённое');

  // удалённая строка «---» — строка диффа «----», а не заголовок
  await page.getByRole('tab', { name: 'История' }).click();
  const rows = page.locator('table[aria-label="Ревизии"] tbody tr');
  await expect(rows).toHaveCount(3);
  await rows.nth(2).getByLabel('А: ревизия 1').check();
  await rows.nth(1).getByLabel('Б: ревизия 2').check();
  await expect(page.locator('.diff .del').filter({ hasText: /^----$/ })).toHaveCount(1);
  await expect(page.locator('.diff .meta').filter({ hasText: /^--- rev 1$/ })).toHaveCount(1);

  await a.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('поиск: Enter до конца debounce открывает результат нового запроса', async ({ page }) => {
  const problems = watch(page);
  const uid = await login(page);
  const a = await api();
  const w1 = uniqueWord();
  const first = await createNote(a, `E2E/Поиск один ${Date.now()}`, `# Один\n\n${w1}\n`);
  const w2 = uniqueWord() + 'й';
  const second = await createNote(a, `E2E/Поиск два ${Date.now()}`, `# Два\n\n${w2}\n`);
  await page.goto(`${base()}#/u/${uid}`);
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await search.fill(w1);
  await expect(page.getByRole('option')).toHaveCount(1);
  // новый запрос и сразу Enter: выдача по нему, а не по прежнему
  await search.fill(w2);
  await search.press('Enter');
  await expect(page).toHaveURL(new RegExp(`/n/${second.id}`));
  expect(page.url()).not.toContain(first.id);
  await a.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('стартовый экран: приветствие по пустому адресу', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  await expect(page.locator('.welcome h1')).toHaveText('admin');
  // повторное открытие пустого адреса при живой сессии
  await page.goto(base());
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  await expect(page.locator('.welcome h1')).toHaveText('admin');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('якоря: оглавление и ссылка в тексте прокручивают каждый раз', async ({ page }) => {
  const problems = watch(page);
  // невысокое окно: оба раздела заведомо уходят из видимой области
  await page.setViewportSize({ width: 1280, height: 480 });
  const uid = await login(page);
  const media = env('E2E_NOTE_MEDIA');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${media}`);
  const doc = page.locator('.doc');
  await expect(doc.locator('.mermaid-diagram svg')).toBeVisible({ timeout: 20_000 });
  const target = doc.getByRole('heading', { level: 2, name: /^Картинка/ });

  await page.locator('details.toc summary').click();
  const item = page.locator('details.toc').getByRole('link', { name: 'Картинка' });
  for (let i = 0; i < 2; i++) {
    await page.evaluate(() => window.scrollTo(0, 0));
    await expect(target).not.toBeInViewport();
    await item.click();
    await expect(target).toBeInViewport();
    await expect(page).toHaveURL(/#h-/);
  }

  // якорь заголовка в тексте: адрес обновляется, повторный клик снова прокручивает
  const code = doc.locator('h2#h-код');
  const anchor = code.locator('a.heading-anchor');
  for (let i = 0; i < 2; i++) {
    await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
    await expect(code).not.toBeInViewport();
    await code.hover();
    await anchor.click();
    await expect(code).toBeInViewport();
    await expect(page).toHaveURL(new RegExp(`/n/${media}#${encodeURIComponent('h-код')}$`));
  }
  expect(problems, problems.join('\n')).toEqual([]);
});
