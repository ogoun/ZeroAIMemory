// Сквозные сценарии журнала «Отчёты» (M5.5): журнал в дереве решения, лента (дата, автор,
// теги, миниатюра, курсор), фильтры и полнотекстовый поиск, страница отчёта (KaTeX, Mermaid,
// картинки, таблица, код), SVG без исполнения скриптов, прочие файлы на скачивание, ссылки
// `data.refs`, Ctrl+K, отсутствие правки, удаление и восстановление только владельцем.
// Данные — seedReports (global-setup, отчёты агента по токену). Снимки — в .tmp/e2e/screens/.
import { expect, test, type Browser, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  LINK_FILE,
  LINK_FILE_AUTO,
  LINK_FILE_PATH,
  LINK_HTTPS,
  LINK_MAILTO,
  LINK_VSCODE,
  LINK_VSCODE_PATH,
  REPORT_BULK,
  REPORT_COMMIT,
  REPORT_DUP_WORD,
  REPORT_MACHINE,
  REPORT_SESSION,
  REPORT_TITLE,
  REPORT_WORD,
  SOLUTION_GUID,
  SOLUTION_NAME,
  SVG_FLAG,
  makePng,
} from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));
const PASS = 'e2e-report-pass-1';
const PASS2 = 'e2e-report-pass-2';

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы. */
function watch(page: Page, allow: (status: number, path: string) => boolean = () => false): string[] {
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
      allow(r.status(), path);
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

let adminId = '';
async function admin(page: Page): Promise<string> {
  if (!adminId) {
    const r = await page.request.get(`${base()}api/v1/auth/me`);
    expect(r.ok()).toBe(true);
    adminId = ((await r.json()) as { user_id: string }).user_id;
  }
  return adminId;
}

const reportsUrl = (user: string, id = '') =>
  `${base()}#/u/${user}/s/${SOLUTION_GUID}/j/reports${id ? `/${id}` : ''}`;

async function openFeed(page: Page) {
  await page.goto(reportsUrl(await admin(page)));
  await expect(page.getByRole('heading', { level: 1, name: 'Отчёты' })).toBeVisible();
  const feed = page.getByRole('feed', { name: 'Лента отчётов' });
  await expect(feed.locator('article').first()).toBeVisible();
  return feed;
}

async function openReport(page: Page, id: string) {
  await page.goto(reportsUrl(await admin(page), id));
  await expect(page.locator(`article.report[data-report="${id}"] .doc`)).toBeVisible();
}

test('reports_journal_pinned_in_solution_tree', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  const t = page.locator('nav[aria-label="Дерево заметок"]');
  await t.getByRole('button', { name: /^Solutions/ }).first().click();
  await t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).first().click();
  const journals = t.getByRole('list', { name: `Журналы: ${SOLUTION_NAME}` });
  await expect(journals.getByRole('link')).toHaveText([
    'Changelog', 'Releases', 'Отчёты', 'Код', 'Insights', 'Словарь', 'Задачи', 'Идеи', 'Вопросы', 'Vault',
  ]);
  await journals.getByRole('link', { name: 'Отчёты', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/reports$`));
  await expect(page.getByRole('heading', { level: 1, name: 'Отчёты' })).toBeVisible();
  // у Knowledge журнала отчётов нет
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  const kj = t.getByRole('list', { name: 'Журналы: Knowledge' });
  await expect(kj.getByRole('link', { name: 'Insights' })).toBeVisible();
  await expect(kj.getByRole('link', { name: 'Отчёты' })).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('reports_feed_date_author_tags_thumb_cursor', async ({ page }) => {
  const problems = watch(page);
  // запросы картинок ленты: серверные миниатюры, а не исходные файлы (M6, задача 15a)
  const images: { path: string; type: string }[] = [];
  page.on('response', (r) => {
    const u = new URL(r.url());
    if (/\/api\/v1\/(previews|entries\/[^/]+\/attachments)\//.test(u.pathname)) {
      images.push({ path: u.pathname + u.search, type: r.headers()['content-type'] ?? '' });
    }
  });
  const feed = await openFeed(page);
  const cards = feed.locator('article');
  // страница — 20, новые первыми: полный отчёт создан последним
  await expect(cards).toHaveCount(20);
  const first = cards.first();
  await expect(first.getByRole('heading', { level: 3 })).toHaveText(REPORT_TITLE);
  await expect(first.locator('time')).toHaveAttribute('datetime', /^\d{4}-\d{2}-\d{2}T/);
  await expect(first).toContainText(`агент, сессия ${REPORT_SESSION}, ${REPORT_MACHINE}`);
  await expect(first).toContainText('#ml');
  await expect(first).toContainText('train');
  // миниатюра — серверное превью первой картинки (`kind=thumb`), лениво, с object-fit;
  // исходник 160×90 меньше предела 256 — размер тот же
  const thumb = first.locator('img.thumb');
  await expect(thumb).toHaveAttribute('loading', 'lazy');
  await expect(thumb).toHaveAttribute('src', /api\/v1\/previews\/[0-9a-f]{64}\?kind=thumb&px=256$/);
  await expect.poll(() => thumb.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth)).toBe(160);
  expect(await thumb.evaluate((i) => getComputedStyle(i).objectFit)).toBe('cover');
  // у отчёта без картинок — заглушка
  await expect(cards.nth(1).locator('img')).toHaveCount(0);
  await page.screenshot({ path: `${screens}reports-feed.png`, fullPage: true });
  // лента не тянет тела отчётов (их читает страница)
  const more = page.waitForResponse((r) => r.url().includes('kind=report') && r.url().includes('cursor='));
  await page.getByRole('button', { name: 'Показать ещё' }).click();
  const resp = await more;
  const body = (await resp.json()) as { items: Record<string, unknown>[] };
  expect(body.items.every((x) => !('body' in x))).toBe(true);
  // + полный, «perf», корзина, «два раздела», печатный (reports-print.spec), из `zam report`
  await expect(cards).toHaveCount(REPORT_BULK + 6);
  await expect(page.getByRole('button', { name: 'Показать ещё' })).toHaveCount(0);
  // печатный отчёт (первая картинка 320×160): превью ужато до 256 — грузится PNG превью
  const big = feed.locator(`article[data-report="${env('E2E_REPORT_PRINT')}"] img.thumb`);
  await big.scrollIntoViewIfNeeded();
  await expect(big).toHaveAttribute('src', /api\/v1\/previews\/[0-9a-f]{64}\?kind=thumb&px=256$/);
  await expect.poll(() => big.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth)).toBe(256);
  expect(await big.evaluate((i: HTMLImageElement) => i.naturalHeight)).toBe(128);
  // отчёт из `zam report` (первая картинка 300×150) — тоже превью
  const cli = feed.locator(`article[data-report="${env('E2E_REPORT_CLI')}"] img.thumb`);
  await cli.scrollIntoViewIfNeeded();
  await expect.poll(() => cli.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth)).toBe(256);
  // лента не запросила ни одного исходного файла: только превью, все — PNG
  expect(images.length).toBeGreaterThanOrEqual(3);
  for (const i of images) {
    expect(i.path, i.path).toMatch(/^\/api\/v1\/previews\/[0-9a-f]{64}\?kind=thumb&px=256$/);
    expect(i.type, i.path).toBe('image/png');
  }
  expect(problems, problems.join('\n')).toEqual([]);
});

test('reports_feed_filter_tag_area_and_fts', async ({ page }) => {
  const problems = watch(page);
  const feed = await openFeed(page);
  const cards = feed.locator('article');
  const apply = page.getByRole('button', { name: 'Применить' });

  const byTag = page.waitForRequest((r) => r.url().includes('kind=report') && r.url().includes('tag=perf'));
  await page.getByLabel('Тег').fill('perf');
  await apply.click();
  await byTag;
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText('производительность индекса');

  await page.getByRole('button', { name: 'Сбросить' }).click();
  await expect(cards).toHaveCount(20);
  const byArea = page.waitForRequest((r) => r.url().includes('area=bulk'));
  await page.getByLabel('Область').fill('bulk');
  await apply.click();
  await byArea;
  await expect(cards).toHaveCount(20);
  await expect(feed).not.toContainText(REPORT_TITLE);

  // поиск по тексту: POST /search, только вид report, в пространстве решения
  await page.getByRole('button', { name: 'Сбросить' }).click();
  const searched = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/api/v1/search'));
  await page.getByLabel('Поиск по отчётам').fill(REPORT_WORD);
  await apply.click();
  const req = (await searched).postDataJSON() as { filters: { kinds: string[] }; scope: { spaces: string[] } };
  expect(req.filters.kinds).toEqual(['report']);
  expect(req.scope.spaces).toEqual([SOLUTION_GUID]);
  const hits = page.getByRole('feed', { name: 'Найденные отчёты' }).locator('article');
  await expect(hits).toHaveCount(1);
  await expect(hits.first()).toContainText('производительность индекса');
  await expect(hits.first().locator('mark')).toContainText(REPORT_WORD);
  await hits.first().getByRole('link').click();
  await expect(page).toHaveURL(new RegExp(`/j/reports/${env('E2E_REPORT_PERF')}$`));
  await expect(page.getByRole('heading', { name: 'Отчёт E2E: производительность индекса' })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_page_renders_katex_mermaid_images_table_code', async ({ page }) => {
  const problems = watch(page);
  await openReport(page, env('E2E_REPORT_FULL'));
  const doc = page.locator('article.report .doc');
  // первый `#` тела — заголовок отчёта: в шапке он не повторяется
  await expect(doc.locator('h1').first()).toContainText(REPORT_TITLE);
  await expect(page.locator('article.report .head h2')).toHaveCount(0);
  await expect(page.locator('article.report .head')).toContainText(`сессия ${REPORT_SESSION}`);
  await expect(doc.locator('.katex')).toHaveCount(2);
  await expect(doc.locator('.mermaid-diagram svg')).toBeVisible();
  await expect(doc.locator('.mermaid-diagram')).toContainText('Модель');
  await expect(doc.locator('table td')).toHaveCount(4);
  await expect(doc.locator('.code-block pre code')).toContainText('print("готово")');
  const imgs = doc.locator('img');
  await expect(imgs).toHaveCount(2);
  await expect
    .poll(() => imgs.evaluateAll((xs: HTMLImageElement[]) => xs.every((i) => i.complete && i.naturalWidth > 0)))
    .toBe(true);
  // картинка открывается в лайтбоксе
  await imgs.first().click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await page.screenshot({ path: `${screens}report-page.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_svg_as_img_no_script', async ({ page }) => {
  const problems = watch(page);
  await openReport(page, env('E2E_REPORT_FULL'));
  const svg = page.locator('article.report .doc img[src*="diag.svg"]');
  await expect.poll(() => svg.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth > 0)).toBe(true);
  // SVG показан картинкой: ни `<script>`, ни `onload` не исполнились
  expect(await page.evaluate((f) => (window as unknown as Record<string, unknown>)[f], SVG_FLAG)).toBeUndefined();
  expect(await page.locator('article.report .doc svg[onload], article.report .doc script').count()).toBe(0);
  // прямой адрес файла — с CSP sandbox и nosniff
  const r = await page.request.get(`${base()}${await svg.getAttribute('src')}`);
  expect(r.headers()['content-type']).toContain('image/svg+xml');
  expect(r.headers()['content-security-policy']).toContain('sandbox');
  expect(r.headers()['x-content-type-options']).toBe('nosniff');
  expect(r.headers()['content-disposition']).toMatch(/^attachment/);
  // прямой переход по адресу SVG документом не открывается (attachment + CSP sandbox): браузер
  // скачивает файл, страница остаётся прежней, скрипт и `onload` не исполняются нигде
  const src = `${base()}${await svg.getAttribute('src')}`;
  const download = page.waitForEvent('download');
  await page.goto(src).catch((e: Error) => expect(e.message).toContain('Download is starting'));
  expect((await download).suggestedFilename()).toBe('diag.svg');
  expect(await page.evaluate((f) => (window as unknown as Record<string, unknown>)[f], SVG_FLAG)).toBeUndefined();
  expect(page.url()).not.toContain('/attachments/');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_search_one_card_per_report', async ({ page }) => {
  const problems = watch(page);
  await openFeed(page);
  const searched = page.waitForResponse((r) => r.request().method() === 'POST' && r.url().endsWith('/api/v1/search'));
  await page.getByLabel('Поиск по отчётам').fill(REPORT_DUP_WORD);
  await page.getByRole('button', { name: 'Применить' }).click();
  // сервер отдал по элементу на чанк: совпадения в двух разделах — два элемента одного отчёта
  const body = (await (await searched).json()) as { items: { entry_id?: string }[] };
  expect(body.items.filter((x) => x.entry_id === env('E2E_REPORT_DUP')).length).toBeGreaterThan(1);
  const hits = page.getByRole('feed', { name: 'Найденные отчёты' }).locator('article');
  await expect(hits).toHaveCount(1);
  await expect(hits.first()).toContainText('два раздела');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_other_file_download_attachment', async ({ page }) => {
  const problems = watch(page);
  await openReport(page, env('E2E_REPORT_FULL'));
  const files = page.getByRole('region', { name: 'Файлы отчёта' });
  const link = files.getByRole('link', { name: /results\.csv/ });
  await expect(link).toHaveAttribute('download', 'results.csv');
  // картинки — в теле, не в списке файлов
  await expect(files.getByRole('link')).toHaveCount(1);
  const r = await page.request.get(`${base()}${await link.getAttribute('href')}`);
  expect(r.status()).toBe(200);
  expect(r.headers()['content-disposition']).toMatch(/^attachment/);
  expect(r.headers()['x-content-type-options']).toBe('nosniff');
  expect(await r.text()).toContain('epoch,loss');
  // ссылка из тела ведёт на тот же файл
  await expect(page.locator('article.report .doc a', { hasText: 'results.csv' })).toHaveAttribute(
    'href',
    (await link.getAttribute('href'))!,
  );
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_refs_links', async ({ page }) => {
  const problems = watch(page);
  await openReport(page, env('E2E_REPORT_FULL'));
  const refs = page.getByRole('region', { name: 'Ссылки отчёта' });
  await expect(refs.locator('[data-ref="commits"] code')).toHaveText(REPORT_COMMIT);
  await expect(refs.locator('[data-ref="files"] code')).toHaveText('src/train.py');
  const task = refs.locator('[data-ref="tasks"] a');
  await expect(task).toHaveAttribute('href', new RegExp(`/j/tasks/${env('E2E_REPORT_TASK')}$`));
  const note = refs.locator('[data-ref="notes"] a');
  await expect(note).toHaveAttribute('href', new RegExp(`/s/knowledge/n/${env('E2E_NOTE_MEDIA')}$`));
  await note.click();
  await expect(page.getByRole('heading', { level: 1, name: /Медиа-заметка/ })).toBeVisible();
  await page.goBack();
  await refs.locator('[data-ref="tasks"] a').click();
  await expect(page).toHaveURL(new RegExp(`/j/tasks/${env('E2E_REPORT_TASK')}$`));
  await expect(page.getByRole('heading', { level: 1, name: 'Задачи' })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ctrl_k_finds_report_opens_page', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await search.fill(REPORT_WORD);
  const opt = page.getByRole('option').filter({ has: page.locator('.ekind', { hasText: /^отчёт$/ }) });
  await expect(opt).toHaveCount(1);
  await expect(opt).toContainText('производительность индекса');
  await opt.click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/reports/${env('E2E_REPORT_PERF')}$`));
  await expect(page.locator(`article.report[data-report="${env('E2E_REPORT_PERF')}"] .doc`)).toContainText(REPORT_WORD);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_no_create_or_edit_ui', async ({ page }) => {
  const problems = watch(page);
  const noEdit = async () => {
    await expect(page.getByRole('button', { name: /Создать|Новый|Новая|Править|Редактировать|Сохранить/ })).toHaveCount(0);
    await expect(page.locator('main textarea, main .cm-editor')).toHaveCount(0);
  };
  await openFeed(page);
  await noEdit();
  await openReport(page, env('E2E_REPORT_FULL'));
  await noEdit();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_links_rel_copy_path_file_no_navigation', async ({ page, context }) => {
  // M6, задача 15a: ссылки тела — пять схем; внешним — rel; `vscode:` кликается и с кнопкой;
  // `file:` — без перехода (нет href), с кнопкой «копировать путь» (путь ОС)
  const problems = watch(page);
  await openReport(page, env('E2E_REPORT_PRINT'));
  const doc = page.locator('article.report .doc');
  const link = (text: string) => doc.locator('a', { hasText: new RegExp(`^${text}$`) });
  const REL = 'noopener noreferrer';
  await expect(link('сайт')).toHaveAttribute('href', LINK_HTTPS);
  await expect(link('сайт')).toHaveAttribute('rel', REL);
  await expect(link('почта')).toHaveAttribute('href', LINK_MAILTO);
  await expect(link('почта')).toHaveAttribute('rel', REL);
  // у http(s) и mailto кнопки нет: кнопки — у `vscode:`, `file:` и автоссылки `file:`
  await expect(doc.getByRole('button', { name: 'копировать путь' })).toHaveCount(3);

  // vscode: ссылка остаётся ссылкой, рядом кнопка — путь после vscode://file/
  const code = link('код');
  await expect(code).toHaveAttribute('href', LINK_VSCODE);
  await expect(code).toHaveAttribute('rel', REL);
  const codeBtn = code.locator('xpath=following-sibling::*[1]');
  await expect(codeBtn).toHaveText('копировать путь');
  await expect(codeBtn).toHaveAttribute('data-copy-path', LINK_VSCODE_PATH);

  // первая линия — сервер: в HTML страницы у `file:` нет `href` вовсе, адрес — в `data-url`
  // (ревью 15a, I-1); `href` со схемой `vscode:` — только `vscode://file/…`
  const served = await page.request.get(`${base()}api/v1/entries/${env('E2E_REPORT_PRINT')}?format=html`);
  const { html } = (await served.json()) as { html: string };
  expect(html).not.toMatch(/href="file:/i);
  expect(html).toContain(`data-url="${LINK_FILE}"`);
  expect(html).toContain(`data-url="${LINK_FILE_AUTO}"`);
  expect(html).toContain(`href="${LINK_VSCODE}" rel="noopener noreferrer"`);

  // file: — без href: клик никуда не ведёт, новых вкладок нет
  const data = link('данные');
  await expect(data).not.toHaveAttribute('href');
  await expect(data).toHaveAttribute('data-url', LINK_FILE);
  await expect(data).toHaveClass(/local-link/);
  await expect(data).toHaveAttribute('title', LINK_FILE_PATH);
  const url = page.url();
  let popups = 0;
  context.on('page', () => popups++);
  let navigations = 0;
  page.on('framenavigated', () => navigations++);
  await data.click();
  await page.waitForTimeout(300);
  expect(page.url()).toBe(url);
  expect(navigations).toBe(0);
  expect(popups).toBe(0);
  await expect(doc).toBeVisible();
  // автоссылка file: — так же
  const auto = doc.locator('a', { hasText: LINK_FILE_AUTO });
  await expect(auto).not.toHaveAttribute('href');
  await expect(auto.locator('xpath=following-sibling::*[1]')).toHaveAttribute('data-copy-path', '/home/u/notes.md');
  // ни одной ссылки с href на file: в теле нет
  expect(await doc.locator('a[href^="file:" i]').count()).toBe(0);

  // «копировать путь» по HTTP (без Clipboard API) кладёт путь ОС в буфер
  expect(await page.evaluate(() => typeof navigator.clipboard)).toBe('undefined');
  const dataBtn = data.locator('xpath=following-sibling::*[1]');
  await expect(dataBtn).toHaveAttribute('data-copy-path', LINK_FILE_PATH);
  await dataBtn.click();
  await expect(dataBtn).toHaveText('Скопировано');
  const port = new URL(base()).port;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(`http://localhost:${port}/`);
  expect(await probe.evaluate(() => navigator.clipboard.readText())).toBe(LINK_FILE_PATH);
  await page.bringToFront();
  await codeBtn.click();
  await expect(codeBtn).toHaveText('Скопировано');
  await probe.bringToFront();
  expect(await probe.evaluate(() => navigator.clipboard.readText())).toBe(LINK_VSCODE_PATH);
  await probe.close();
  // подпись кнопки возвращается
  await expect(dataBtn).toHaveText('копировать путь');
  await page.screenshot({ path: `${screens}report-links.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_links_hostile_schemes_no_href_in_webui', async ({ page }) => {
  // вторая линия защиты: если бы сервер пропустил чужую схему, рендер WebUI снимает href сам —
  // та же проверка схемы, что в `zam report` и на сервере (регистр, пробел, управляющие символы)
  const problems = watch(page);
  const hostile = [
    'javascript:window.__zamLinkPwned=1',
    'JaVaScRiPt:window.__zamLinkPwned=1',
    ' javascript:window.__zamLinkPwned=1',
    'java\tscript:window.__zamLinkPwned=1',
    'java\nscript:window.__zamLinkPwned=1',
    '\u0001javascript:window.__zamLinkPwned=1',
    'data:text/html,<script>parent.__zamLinkPwned=1</script>',
    'vbscript:x',
    'ftp://example.org/a',
    'blob:http://example.org/1',
    'vscode-insiders://file/x',
    // `vscode:` разрешена, но переход — только у `vscode://file/…`: команды расширений не кликаются
    'vscode://vscode.git/clone?url=https://example.org/x.git',
    'vscode:extension/publisher.name',
  ];
  const safe = ['%6Aavascript:window.__zamLinkPwned=1', 'https://example.org/ok', 'vscode://file/C:/ok.rs', '#h-x'];
  const id = env('E2E_REPORT_PERF');
  await page.route(new RegExp(`/api/v1/entries/${id}\\?format=html`), async (route) => {
    const resp = await route.fetch();
    const j = (await resp.json()) as { html: string };
    const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');
    j.html +=
      '<p class="hostile">' +
      [...hostile, ...safe].map((h, i) => `<a data-i="${i}" href="${esc(h)}">ссылка ${i}</a>`).join(' ') +
      '</p>';
    await route.fulfill({ response: resp, json: j });
  });
  await openReport(page, id);
  const links = page.locator('article.report .doc p.hostile a');
  await expect(links).toHaveCount(hostile.length + safe.length);
  for (let i = 0; i < hostile.length; i++) {
    await expect(links.nth(i), hostile[i]).not.toHaveAttribute('href');
    await links.nth(i).click();
  }
  await expect(links.nth(hostile.length)).toHaveAttribute('href', safe[0]);
  await expect(links.nth(hostile.length + 1)).toHaveAttribute('rel', 'noopener noreferrer');
  await expect(links.nth(hostile.length + 2)).toHaveAttribute('href', safe[2]);
  // кнопка «копировать путь» — только у `vscode://file/…`; у команд `vscode:` её нет
  await expect(page.locator('article.report .doc p.hostile button.copy-path')).toHaveCount(1);
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => (window as unknown as Record<string, unknown>).__zamLinkPwned)).toBeUndefined();
  await expect(page.locator(`article.report[data-report="${id}"] .doc`)).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Участник решения администратора с одобренным доступом `access` (без входа в WebUI). */
async function member(browser: Browser, page: Page, access: 'rw' | 'ro') {
  const rc = page.request;
  const me = await rc.get(`${base()}api/v1/auth/me`);
  const { csrf } = (await me.json()) as { csrf: string };
  const call = async <T>(method: string, path: string, data?: unknown, headers: Record<string, string> = {}) => {
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf, ...headers }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
  const name = `e2e-report-${access}-${Date.now()}`;
  const u = await call<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
  // свой контекст без cookie администратора: запрос доступа — токеном участника
  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const req = await ctx.request.post(`${base()}api/v1/solutions`, {
    headers: { Authorization: `Bearer ${u.token.plaintext}` },
    data: { agent_solution_id: SOLUTION_GUID, name: SOLUTION_NAME },
  });
  expect(req.status()).toBe(202);
  await call('POST', `solutions/${SOLUTION_GUID}/access/${u.user.id}/approve`, { access });
  return { ctx, call, name, id: u.user.id, token: u.token.plaintext };
}

/** Участник `rw` решения администратора: вход с обязательной сменой пароля, свой контекст. */
async function memberRw(browser: Browser, page: Page) {
  const { ctx, call, name, id } = await member(browser, page, 'rw');
  await call('POST', `admin/users/${id}/password`, { password: PASS });
  const login = await ctx.request.post(`${base()}api/v1/auth/login`, { data: { login: name, password: PASS } });
  expect(login.status()).toBe(200);
  const mc = ((await login.json()) as { csrf: string }).csrf;
  const pw = await ctx.request.post(`${base()}api/v1/auth/password`, {
    headers: { 'X-CSRF-Token': mc },
    data: { old: PASS, new: PASS2 },
  });
  expect(pw.status()).toBe(204);
  return { ctx, id, csrf: mc };
}

test('report_admin_for_user_no_trash_ui_files_with_user', async ({ page, browser }) => {
  const problems = watch(page);
  // отчёт в решении другого пользователя (его агент, multipart); администратор открывает журнал
  // от его имени: база не своя — адреса файлов с ?user=<владелец>, корзины и «Удалить» нет
  const m = await member(browser, page, 'ro');
  const guid = '0e2e0000-0000-4000-8000-0000000005a5';
  const auth = { Authorization: `Bearer ${m.token}` };
  const reg = await m.ctx.request.post(`${base()}api/v1/solutions`, {
    headers: auth,
    data: { agent_solution_id: guid, name: 'E2E решение пользователя' },
  });
  expect(reg.status()).toBe(201);
  const created = await m.ctx.request.post(`${base()}api/v1/spaces/${guid}/entries`, {
    headers: auth,
    multipart: {
      meta: {
        name: 'meta',
        mimeType: 'application/json',
        buffer: Buffer.from(JSON.stringify({ title: 'Отчёт пользователя E2E', body: '![график](plot.png)\n', tags: [] })),
      },
      file: { name: 'plot.png', mimeType: 'image/png', buffer: makePng(80, 40) },
    },
  });
  expect(created.status()).toBe(201);
  const id = ((await created.json()) as { id: string }).id;
  await m.ctx.close();

  const url = `${base()}#/u/${m.id}/s/${guid}/j/reports`;
  await page.goto(url);
  const feed = page.getByRole('feed', { name: 'Лента отчётов' });
  const thumb = feed.locator(`article[data-report="${id}"] img.thumb`);
  // миниатюра — превью от имени владельца (`&user=`), не исходный файл
  await expect(thumb).toHaveAttribute('src', new RegExp(`api/v1/previews/[0-9a-f]{64}\\?kind=thumb&px=256&user=${m.id}$`));
  await expect.poll(() => thumb.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth)).toBe(80);
  await expect(page.getByRole('button', { name: 'Корзина', exact: true })).toHaveCount(0);
  await page.goto(`${url}/${id}`);
  const img = page.locator('article.report .doc img');
  await expect(img).toHaveAttribute('src', new RegExp(`\\?user=${m.id}$`));
  await expect.poll(() => img.evaluate((i: HTMLImageElement) => i.complete && i.naturalWidth)).toBe(80);
  await expect(page.getByRole('button', { name: 'Удалить' })).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('report_delete_restore_owner_only_ui', async ({ page, browser }) => {
  const id = env('E2E_REPORT_TRASH');
  // участник rw: страница и лента без «Удалить» и «Корзины»; сервер — 403
  const m = await memberRw(browser, page);
  const mp = await m.ctx.newPage();
  const mProblems = watch(mp);
  await mp.goto(reportsUrl(m.id, id));
  await expect(mp.locator(`article.report[data-report="${id}"] .doc`)).toContainText('Удаляется');
  await expect(mp.getByRole('button', { name: 'Удалить' })).toHaveCount(0);
  await mp.goto(reportsUrl(m.id));
  await expect(mp.getByRole('feed', { name: 'Лента отчётов' }).locator('article').first()).toBeVisible();
  await expect(mp.getByRole('button', { name: 'Корзина', exact: true })).toHaveCount(0);
  const entry = await m.ctx.request.get(`${base()}api/v1/entries/${id}`);
  const rev = ((await entry.json()) as { rev: number }).rev;
  const del = await m.ctx.request.delete(`${base()}api/v1/entries/${id}`, {
    headers: { 'X-CSRF-Token': m.csrf, 'If-Match': `"${rev}"` },
  });
  expect(del.status()).toBe(403);
  expect(mProblems, mProblems.join('\n')).toEqual([]);
  await m.ctx.close();

  // владелец: «Удалить» с подтверждением → лента без отчёта → «Корзина» → «Восстановить»
  const problems = watch(page);
  await openReport(page, id);
  await page.getByRole('button', { name: 'Удалить' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg).toContainText('Отчёт E2E для корзины');
  await dlg.getByRole('button', { name: 'Удалить' }).click();
  await expect(page).toHaveURL(new RegExp(`/j/reports$`));
  await expect(page.getByText('Отчёт «Отчёт E2E для корзины» перемещён в корзину.')).toBeVisible();
  const feed = page.getByRole('feed', { name: 'Лента отчётов' });
  await expect(feed.locator('article').first()).toBeVisible();
  await expect(feed.locator(`article[data-report="${id}"]`)).toHaveCount(0);

  await page.getByRole('button', { name: 'Корзина', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Корзина', exact: true })).toHaveAttribute('aria-pressed', 'true');
  const card = feed.locator(`article[data-report="${id}"]`);
  await expect(card).toContainText('удалён');
  await page.screenshot({ path: `${screens}reports-trash.png`, fullPage: true });
  await card.getByRole('button', { name: 'Восстановить' }).click();
  await expect(page.getByText('Отчёт «Отчёт E2E для корзины» восстановлен.')).toBeVisible();
  await expect(card).toHaveCount(0);
  await page.getByRole('button', { name: 'Корзина', exact: true }).click();
  await page.getByLabel('Тег').fill('trash');
  await page.getByRole('button', { name: 'Применить' }).click();
  await expect(feed.locator(`article[data-report="${id}"]`)).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});
