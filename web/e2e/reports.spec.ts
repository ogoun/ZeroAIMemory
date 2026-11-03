// Сквозные сценарии журнала «Отчёты» (M5.5): журнал в дереве решения, лента (дата, автор,
// теги, миниатюра, курсор), фильтры и полнотекстовый поиск, страница отчёта (KaTeX, Mermaid,
// картинки, таблица, код), SVG без исполнения скриптов, прочие файлы на скачивание, ссылки
// `data.refs`, Ctrl+K, отсутствие правки, удаление и восстановление только владельцем.
// Данные — seedReports (global-setup, отчёты агента по токену). Снимки — в .tmp/e2e/screens/.
import { expect, test, type Browser, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  REPORT_BULK,
  REPORT_COMMIT,
  REPORT_MACHINE,
  REPORT_SESSION,
  REPORT_TITLE,
  REPORT_WORD,
  SOLUTION_GUID,
  SOLUTION_NAME,
  SVG_FLAG,
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
    'Changelog', 'Releases', 'Отчёты', 'Insights', 'Словарь', 'Задачи', 'Вопросы', 'Vault',
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
  // миниатюра — исходный файл первой картинки, лениво, с object-fit
  const thumb = first.locator('img.thumb');
  await expect(thumb).toHaveAttribute('loading', 'lazy');
  await expect(thumb).toHaveAttribute('src', /\/attachments\/[0-9a-f]{64}\/loss\.png$/);
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
  await expect(cards).toHaveCount(REPORT_BULK + 3);
  await expect(page.getByRole('button', { name: 'Показать ещё' })).toHaveCount(0);
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
  const opt = page.getByRole('option').filter({ hasText: 'отчёт' });
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

/** Участник `rw` решения администратора: вход с обязательной сменой пароля, свой контекст. */
async function memberRw(browser: Browser, page: Page) {
  const rc = page.request;
  const me = await rc.get(`${base()}api/v1/auth/me`);
  const { csrf } = (await me.json()) as { csrf: string };
  const call = async <T>(method: string, path: string, data?: unknown, headers: Record<string, string> = {}) => {
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf, ...headers }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
  const name = `e2e-report-rw-${Date.now()}`;
  const u = await call<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
  // свой контекст без cookie администратора: запрос доступа — токеном участника
  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const req = await ctx.request.post(`${base()}api/v1/solutions`, {
    headers: { Authorization: `Bearer ${u.token.plaintext}` },
    data: { agent_solution_id: SOLUTION_GUID, name: SOLUTION_NAME },
  });
  expect(req.status()).toBe(202);
  await call('POST', `solutions/${SOLUTION_GUID}/access/${u.user.id}/approve`, { access: 'rw' });
  await call('POST', `admin/users/${u.user.id}/password`, { password: PASS });
  const login = await ctx.request.post(`${base()}api/v1/auth/login`, { data: { login: name, password: PASS } });
  expect(login.status()).toBe(200);
  const mc = ((await login.json()) as { csrf: string }).csrf;
  const pw = await ctx.request.post(`${base()}api/v1/auth/password`, {
    headers: { 'X-CSRF-Token': mc },
    data: { old: PASS, new: PASS2 },
  });
  expect(pw.status()).toBe(204);
  return { ctx, id: u.user.id, csrf: mc };
}

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
