// Сквозные сценарии вложений и просмотрщиков документов (M5, задача 9): переход из выдачи
// поиска к месту документа (страница PDF, абзац DOCX, диапазон листа, слайд), изоляция
// недоверенного DOCX и PDF (Review Focus 5), список вложений и открепление, CSP главной
// страницы. Данные — seed-docs.ts (global-setup).
import { expect, test, type Frame, type Page } from '@playwright/test';
import { ADMIN_STATE } from './session';
import { DOCX_SECTION, FILES, WORDS, XLSX_ROW } from './seed-docs';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });

/** CSP главной страницы — эталон: просмотрщики не должны его ослаблять. */
const MAIN_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; " +
  "img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'";
/**
 * CSP только ответа `/viewer/pdf`: главная + `'wasm-unsafe-eval'`, воркер, встраивание в WebUI и
 * `sandbox` без allow-same-origin (непрозрачный источник документа).
 */
const VIEWER_PDF_CSP =
  "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; " +
  "img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; frame-ancestors 'self'; " +
  "base-uri 'self'; form-action 'none'; sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox";

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы и её фреймов. */
function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    const t = m.text();
    // отказ песочницы фрейма DOCX исполнять скрипт — так и должно быть (скриптов в документе нет:
    // это проверяется отдельно; сообщения дают и вызовы Playwright внутри фрейма)
    const sandboxed = t.startsWith("Blocked script execution in 'about:srcdoc' because the document's frame is sandboxed");
    if (m.type() === 'error' && !t.startsWith('Failed to load resource') && !sandboxed) problems.push(t);
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected = r.status() === 404 && path.endsWith('/favicon.ico');
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  page.on('dialog', (d) => {
    problems.push(`диалог: ${d.message()}`);
    void d.dismiss();
  });
  return problems;
}

/** Метка исполнения враждебной вставки документа в окне WebUI. */
const pwned = (page: Page) => page.evaluate(() => (window as unknown as { __pwned?: string }).__pwned ?? null);

async function login(page: Page): Promise<string> {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  return decodeURIComponent(/#\/u\/([^/]+)$/.exec(page.url())![1]);
}

/** Ctrl+K → запрос → карточка вложения `file` (с подписью места). */
async function searchCard(page: Page, word: string, file: string) {
  await page.keyboard.press('Control+k');
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('combobox', { name: 'Поиск по заметкам' }).fill(word);
  const card = dialog.getByRole('option').filter({ hasText: file });
  await expect(card).toBeVisible({ timeout: 15_000 });
  return card;
}

function frameOf(page: Page, part: string): Frame {
  const f = page.frames().find((x) => x.url().includes(part));
  if (!f) throw new Error(`нет фрейма ${part}`);
  return f;
}

test('pdf_opens_at_locator_page', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const card = await searchCard(page, WORDS.pdf, FILES.pdf);
  await expect(card).toContainText('стр. 2');
  await card.click();
  await expect(page).toHaveURL(/\/a\/[0-9a-f]{64}\/[^/]+\/page%3A2$/);
  await expect(page.getByRole('heading', { level: 1, name: FILES.pdf })).toBeVisible();

  // отдельный документ /viewer/pdf с непрозрачным источником (проверка поведения — ниже)
  const viewer = page.frameLocator('iframe[title="Просмотр PDF"]');
  await expect(viewer.getByLabel('Страница', { exact: true })).toHaveValue('2', { timeout: 20_000 });
  await expect(viewer.getByText('из 3')).toBeVisible();
  await expect(viewer.locator('.page[data-page-number="2"] .textLayer')).toContainText(WORDS.pdf);
  const top = await frameOf(page, 'viewer/pdf').evaluate(() => {
    const el = document.querySelector('.page[data-page-number="2"]')!;
    const box = document.getElementById('viewerContainer')!;
    return el.getBoundingClientRect().top - box.getBoundingClientRect().top;
  });
  expect(Math.abs(top)).toBeLessThan(40);

  // поиск по тексту средствами pdf.js: переход к странице с совпадением
  const find = viewer.getByLabel('Поиск в документе');
  await find.fill('closing');
  await find.press('Enter');
  await expect(viewer.getByLabel('Страница', { exact: true })).toHaveValue('3');
  await expect(viewer.locator('.textLayer .highlight').first()).toBeVisible();
  await expect(viewer.locator('#find-status')).toContainText('1 из 1');

  // JavaScript PDF (OpenAction app.alert) не исполнен: диалогов нет (watch), метки нет
  expect(await pwned(page)).toBeNull();
  expect(problems).toEqual([]);

  // изоляция от WebUI (Review Focus 5, раунд правок 1, I-1): DOM WebUI недоступен, запрос к API
  // с cookie сессии не проходит (межсайтовый без CORS), cookie документа не читаются
  const isolation = await frameOf(page, 'viewer/pdf').evaluate(async () => {
    const out: Record<string, string> = { origin: String(self.origin) };
    try {
      out.parent = `ACCESS ${parent.document.title}`;
    } catch (e) {
      out.parent = (e as Error).name;
    }
    try {
      const r = await fetch('api/v1/spaces', { credentials: 'include' });
      out.api = `HTTP ${r.status}`;
    } catch (e) {
      out.api = (e as Error).name;
    }
    try {
      out.cookie = `READ ${document.cookie}`;
    } catch (e) {
      out.cookie = (e as Error).name;
    }
    return out;
  });
  expect(isolation).toEqual({ origin: 'null', parent: 'SecurityError', api: 'TypeError', cookie: 'SecurityError' });

  // CSP документа просмотрщика: строковый код (eval-подобный таймер) и встроенный скрипт не
  // исполняются (CDP-вызов evaluate сам CSP обходит, поэтому проверка — кодом страницы)
  const viewerFrame = frameOf(page, 'viewer/pdf');
  await viewerFrame.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    (setTimeout as unknown as (code: string, ms: number) => void)('window.__evalRan = 1', 0);
    const s = document.createElement('script');
    s.textContent = 'window.__inlineRan = 1';
    document.head.append(s);
    w.__probe = 1;
  });
  await page.waitForTimeout(300);
  expect(
    await viewerFrame.evaluate(() => {
      const w = window as unknown as Record<string, unknown>;
      return [w.__evalRan ?? null, w.__inlineRan ?? null, w.__probe];
    }),
  ).toEqual([null, null, 1]);

  // переход назад — к заметке-владельцу
  await page.getByRole('link', { name: /Документы/ }).first().click();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Документы/ })).toBeVisible();
});

test('docx_renders_in_sandbox_no_script', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const card = await searchCard(page, WORDS.docx, FILES.docx);
  await expect(card).toContainText(/абз\. \d+/);
  await card.click();
  await expect(page).toHaveURL(/\/a\/[0-9a-f]{64}\/[^/]+\/para%3A\d+$/);

  // sandbox-iframe без allow-scripts и allow-same-origin: непрозрачный источник, скриптов нет
  const iframe = page.locator('iframe[title="Просмотр DOCX"]');
  await expect(iframe).toHaveAttribute('sandbox', '');
  expect(await iframe.getAttribute('srcdoc')).toContain("default-src 'none'");
  const doc = page.frameLocator('iframe[title="Просмотр DOCX"]');
  await expect(doc.getByText(DOCX_SECTION, { exact: true })).toBeVisible({ timeout: 20_000 });
  await expect(doc.getByText(WORDS.docx, { exact: false })).toBeVisible();

  // абзац из выдачи: отмечен и прокручен в видимую часть фрейма
  const frame = frameOf(page, 'about:srcdoc');
  const target = await frame.evaluate(() => {
    const el = document.getElementById('zam-target');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { text: el.textContent ?? '', top: r.top, h: innerHeight, scrolled: document.scrollingElement!.scrollTop };
  });
  expect(target).not.toBeNull();
  expect(target!.text).toContain(DOCX_SECTION);
  expect(target!.scrolled).toBeGreaterThan(0);
  expect(target!.top).toBeGreaterThanOrEqual(0);
  expect(target!.top).toBeLessThan(target!.h);

  // вставка altChunk со скриптом не отрисована, скриптов, фреймов и форм в документе нет
  expect(await doc.locator('script, iframe, object, embed, form').count()).toBe(0);
  await expect(doc.getByText('altchunk text')).toHaveCount(0);
  // ссылки документа не открываются: внешних href нет, javascript: — тоже
  expect(await doc.locator('a[href]:not([href^="#"])').count()).toBe(0);
  await doc.getByText('Опасная ссылка').click();
  await doc.getByText('Внешний сайт').click();
  await expect(page).toHaveURL(/para%3A\d+$/);
  expect(page.context().pages()).toHaveLength(1);
  // документ не видит cookie WebUI (непрозрачный источник)
  const cookie = await frame.evaluate(() => {
    try {
      return document.cookie;
    } catch (e) {
      return (e as Error).name;
    }
  });
  expect(cookie).toBe('SecurityError');
  expect(await pwned(page)).toBeNull();
  expect(problems).toEqual([]);
});

test('xlsx_sheet_switch_and_range', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const card = await searchCard(page, WORDS.xlsx, FILES.xlsx);
  await expect(card).toContainText('лист «Данные»');
  await card.click();
  await expect(page).toHaveURL(/\/a\/[0-9a-f]{64}\/[^/]+\/sheet%3A/);

  const tabs = page.getByRole('tablist', { name: 'Листы' });
  await expect(tabs.getByRole('tab', { name: 'Данные' })).toHaveAttribute('aria-selected', 'true');
  const grid = page.getByRole('grid', { name: /Данные/ });
  await expect(grid).toHaveAttribute('aria-rowcount', '400');
  // закреплённая шапка листа
  await expect(grid.getByRole('columnheader', { name: 'Описание' })).toBeVisible();
  // диапазон из выдачи: подсвечен, первая его строка — в видимой части
  const range = page.getByTestId('sheet-range');
  await expect(range).toContainText(/A\d+:C\d+/);
  const first = Number(/A(\d+):/.exec((await range.textContent()) ?? '')![1]);
  expect(first).toBeLessThanOrEqual(XLSX_ROW);
  await expect(grid.locator(`[role="row"][aria-rowindex="${first}"]`)).toBeInViewport();
  expect(await grid.locator('.in-range').count()).toBeGreaterThan(0);
  // строка со словом — внутри диапазона: прокрутка к ней (в DOM только видимые строки)
  await grid.evaluate((el, d) => (el.scrollTop += d * 28), XLSX_ROW - first);
  const hit = grid.getByText(WORDS.xlsx, { exact: false });
  await expect(hit).toBeInViewport();
  await expect(grid.locator(`[role="row"][aria-rowindex="${XLSX_ROW}"] .in-range`).first()).toBeVisible();

  // виртуальная прокрутка: последние строки подгружаются, шапка остаётся на месте
  await grid.evaluate((el) => (el.scrollTop = el.scrollHeight));
  await expect(grid.getByText('R400', { exact: true })).toBeVisible();
  await expect(grid.getByRole('columnheader', { name: 'Описание' })).toBeInViewport();

  // переключение листа мышью и клавиатурой
  await tabs.getByRole('tab', { name: 'Итоги' }).click();
  await expect(page.getByRole('grid', { name: /Итоги/ }).getByText('Выручка')).toBeVisible();
  await tabs.getByRole('tab', { name: 'Итоги' }).press('ArrowRight');
  await expect(tabs.getByRole('tab', { name: 'Данные' })).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.getByRole('tab', { name: 'Данные' })).toBeFocused();
  // возврат на лист из выдачи — снова к его диапазону
  await expect(page.getByRole('grid', { name: /Данные/ }).locator(`[role="row"][aria-rowindex="${first}"]`)).toBeInViewport();
  expect(problems).toEqual([]);
});

test('pptx_slide_text', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const card = await searchCard(page, WORDS.pptx, FILES.pptx);
  await expect(card).toContainText('слайд 4');
  await card.click();
  await expect(page).toHaveURL(/\/a\/[0-9a-f]{64}\/[^/]+\/slide%3A4$/);

  const slide = page.getByRole('region', { name: /^Слайд/ });
  await expect(page.getByTestId('slide-pos')).toHaveText('Слайд 4 из 5');
  await expect(slide).toContainText(WORDS.pptx);
  await expect(slide.getByRole('heading', { name: 'Часть вторая' })).toBeVisible();
  // навигация: клавиши и список слайдов
  await slide.focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.getByTestId('slide-pos')).toHaveText('Слайд 5 из 5');
  await expect(slide).toContainText('Спасибо за внимание');
  await page.getByRole('button', { name: 'Следующий слайд' }).isDisabled();
  await page.getByRole('navigation', { name: 'Слайды' }).getByRole('button', { name: /План/ }).click();
  await expect(page.getByTestId('slide-pos')).toHaveText('Слайд 2 из 5');
  await page.getByRole('button', { name: 'Предыдущий слайд' }).click();
  await expect(page.getByTestId('slide-pos')).toHaveText('Слайд 1 из 5');
  expect(problems).toEqual([]);
});

test('attachments_list_detach', async ({ page }) => {
  const problems = watch(page);
  const me = await login(page);
  const note = env('E2E_NOTE_ATTACH');
  await page.goto(`${base()}#/u/${me}/s/knowledge/n/${note}`);
  const tab = page.getByRole('tab', { name: /^Вложения/ });
  await expect(tab).toContainText('5');
  await tab.click();
  const list = page.getByRole('list', { name: 'Вложения заметки' });
  const row = (name: string) => list.getByRole('listitem').filter({ has: page.getByRole('link', { name, exact: true }) });
  await expect(list.getByRole('listitem')).toHaveCount(5);
  // состояние извлечения: неудача — по коду причины (не путь), «нужно распознавание», размер
  await expect(row(FILES.broken)).toContainText('текст не извлечён: файл повреждён или не соответствует формату');
  await expect(row(FILES.scan)).toContainText('нужно распознавание');
  await expect(row(FILES.svg)).toContainText('без извлечения текста');
  await expect(row(FILES.text)).toContainText('текст извлечён');
  await expect(row(FILES.text)).toContainText(/\d+ Б/);

  // SVG — только через <img>: его скрипты не исполняются
  await row(FILES.svg).getByRole('link', { name: FILES.svg, exact: true }).click();
  const img = page.getByRole('img', { name: FILES.svg });
  await expect(img).toBeVisible();
  expect(await img.evaluate((el) => (el as HTMLImageElement).naturalWidth)).toBe(120);
  expect(await page.locator('main svg rect').count()).toBe(0);
  expect(await pwned(page)).toBeNull();
  await page.goBack();

  // открепление — с подтверждением
  await page.getByRole('tab', { name: /^Вложения/ }).click();
  await row(FILES.extra).getByRole('button', { name: `Открепить ${FILES.extra}` }).click();
  const confirm = page.getByRole('dialog');
  await expect(confirm).toContainText(FILES.extra);
  await confirm.getByRole('button', { name: 'Отмена' }).click();
  await expect(row(FILES.extra)).toHaveCount(1);
  await row(FILES.extra).getByRole('button', { name: `Открепить ${FILES.extra}` }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Открепить' }).click();
  await expect(row(FILES.extra)).toHaveCount(0);
  await expect(page.getByRole('tab', { name: /^Вложения/ })).toContainText('4');
  const left = (await (await page.request.get(`${base()}api/v1/nodes/${note}/attachments`)).json()) as { filename: string }[];
  expect(left.map((a) => a.filename)).not.toContain(FILES.extra);

  // участник с правом чтения: кнопки открепления нет
  await page.route('**/api/v1/spaces', async (route) => {
    const r = await route.fetch();
    const body = await r.json();
    for (const s of body.spaces) if (s.kind === 'knowledge') Object.assign(s, { role: 'member', access: 'ro' });
    await route.fulfill({ response: r, json: body });
  });
  await page.reload();
  await page.getByRole('tab', { name: /^Вложения/ }).click();
  await expect(list.getByRole('listitem')).toHaveCount(4);
  await expect(list.getByRole('button', { name: /^Открепить/ })).toHaveCount(0);
  expect(problems).toEqual([]);
});

test('sheet_no_locator_without_secret', async ({ page }) => {
  // `sheet:#1` — первый лист с единицами (как в `/read`), а не первый в книге: перед ним пустой
  const problems = watch(page);
  const me = await login(page);
  const note = env('E2E_NOTE_DOCS');
  const list = (await (await page.request.get(`${base()}api/v1/nodes/${note}/attachments`)).json()) as {
    sha256: string;
    filename: string;
  }[];
  const nums = list.find((a) => a.filename === FILES.nums)!;
  await page.goto(
    `${base()}#/u/${me}/s/knowledge/n/${note}/a/${nums.sha256}/${encodeURIComponent(nums.filename)}/${encodeURIComponent('sheet:#1!A2:B2')}`,
  );
  const tabs = page.getByRole('tablist', { name: 'Листы' });
  await expect(tabs.getByRole('tab', { name: 'Цифры' })).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('grid', { name: /Цифры/ }).getByText('один')).toBeVisible();
  await expect(page.getByTestId('sheet-range')).toContainText('A2:B2');
  expect(problems).toEqual([]);
});

test('docx_zip_bomb_refused', async ({ page }) => {
  // 320 МиБ распаковки при заявленных 100 Б: отказ по фактическому объёму, вкладка жива
  const problems = watch(page);
  const me = await login(page);
  const note = env('E2E_NOTE_BOMB');
  await page.goto(`${base()}#/u/${me}/s/knowledge/n/${note}`);
  await page.getByRole('tab', { name: /^Вложения/ }).click();
  await page.getByRole('link', { name: FILES.bomb, exact: true }).click();
  // какой отказ — зависит от итога сервера: too_large/parse_oom — без разбора в браузере,
  // иначе — потоковая проверка распаковки в браузере (главный случай I-2)
  const docs = (await (await page.request.get(`${base()}api/v1/nodes/${note}/attachments`)).json()) as {
    extract_error: string | null;
  }[];
  const code = (docs[0].extract_error ?? '').split(':')[0];
  const heavy = code === 'too_large' || code === 'parse_oom';
  console.log(`бомба: сервер ${docs[0].extract_error ?? 'без ошибки'}, отказ ${heavy ? 'по итогу сервера' : 'в браузере'}`);
  await expect(page.getByRole('alert')).toContainText(
    heavy ? 'не смог его разобрать' : 'слишком велик для просмотра в браузере',
    { timeout: 30_000 },
  );
  await expect(page.locator('iframe[title="Просмотр DOCX"]')).toHaveCount(0);
  expect(await page.evaluate(() => 1 + 1)).toBe(2);
  const heap = await page.evaluate(
    () => (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? 0,
  );
  expect(heap).toBeLessThan(150 * 1024 * 1024);
  expect(problems).toEqual([]);
});

test('main_csp_unchanged', async ({ page, request }) => {
  const main = await request.get(base());
  expect(main.headers()['content-security-policy']).toBe(MAIN_CSP);
  const viewer = await request.get(`${base()}viewer/pdf`);
  expect(viewer.status()).toBe(200);
  expect(viewer.headers()['content-security-policy']).toBe(VIEWER_PDF_CSP);
  // главная страница по глубокому адресу (SPA) — та же политика
  const deep = await request.get(`${base()}notes/deep`);
  expect(deep.headers()['content-security-policy']).toBe(MAIN_CSP);
  // в браузере: главный документ не допускает wasm-eval и встраивание в чужие страницы
  await page.goto(base());
  const resp = await page.request.get(base());
  expect(resp.headers()['content-security-policy']).not.toContain('wasm-unsafe-eval');
});
