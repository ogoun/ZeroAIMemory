// Сквозные сценарии страницы «Код» (M5.6, спецификация §11): пункт в дереве решения (у
// Knowledge — нет), состояние снимка (языки, пропуски по причинам, возраст, ветка и коммит),
// поиск символа по Ctrl+K страницы, карточка символа (сигнатура, строки, кто ссылается, на что
// ссылается, привязанные знания со ссылками), таблица файлов по рангу с фильтром языка, «Обзор»
// Mermaid (≤ 40 файлов, ≤ 120 рёбер), очистка карты только владельцем, маска секрета в
// сигнатуре, тёмная тема и печать. Данные — seedCodeMap/seedCodeKnowledge (global-setup).
import { expect, test, type Browser, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  CODE_BRANCH,
  CODE_COMMIT,
  CODE_TASK_TITLE,
  CODE_MACHINE,
  CODE_NOTE_TITLE,
  CODE_REF,
  CODE_SECRET,
  SOLUTION_GUID,
  SOLUTION_NAME,
  seedCodeMap,
} from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));
const PASS = 'e2e-code-pass-1';
const PASS2 = 'e2e-code-pass-2';

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы. */
function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) ||
      (r.status() === 404 && path.endsWith('/favicon.ico'));
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

const codeUrl = (user: string, ref = '') =>
  `${base()}#/u/${user}/s/${SOLUTION_GUID}/j/code${ref ? `/${encodeURIComponent(ref)}` : ''}`;

const statusBox = (page: Page) => page.getByRole('region', { name: 'Состояние карты' });

async function openCode(page: Page, ref = '') {
  await page.goto(codeUrl(await admin(page), ref));
  await expect(page.getByRole('heading', { level: 1, name: 'Код' })).toBeVisible();
}

test('code_item_in_solution_tree_not_knowledge', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  const t = page.locator('nav[aria-label="Дерево заметок"]');
  await t.getByRole('button', { name: /^Solutions/ }).first().click();
  await t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).first().click();
  const journals = t.getByRole('list', { name: `Журналы: ${SOLUTION_NAME}` });
  await expect(journals.getByRole('link')).toHaveText([
    'Changelog', 'Releases', 'Отчёты', 'Код', 'Insights', 'Словарь', 'Задачи', 'Вопросы', 'Vault',
  ]);
  await journals.getByRole('link', { name: 'Код', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/code$`));
  await expect(page.getByRole('heading', { level: 1, name: 'Код' })).toBeVisible();
  await expect(statusBox(page)).toBeVisible();
  // у Knowledge карты кода нет
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  const kj = t.getByRole('list', { name: 'Журналы: Knowledge' });
  await expect(kj.getByRole('link', { name: 'Insights' })).toBeVisible();
  await expect(kj.getByRole('link', { name: 'Код' })).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_status_langs_skips_age_branch', async ({ page }) => {
  const problems = watch(page);
  await openCode(page);
  const box = statusBox(page);
  await expect(box).toContainText('8 файлов, 40 символов');
  await expect(box).toContainText(`${CODE_BRANCH}@${CODE_COMMIT.slice(0, 7)}`);
  await expect(box).toContainText(CODE_MACHINE);
  await expect(box.getByTestId('code-age')).toHaveText(/^снимок (только что|\d+ мин назад)$/);
  const langs = box.getByRole('table', { name: 'Языки' });
  await expect(langs.locator('tbody tr')).toHaveCount(4);
  await expect(langs.locator('tr', { hasText: 'rust' })).toContainText(/rust\s*3\s*20/);
  await expect(langs.locator('tr', { hasText: 'typescript' })).toContainText(/typescript\s*3\s*14/);
  await expect(langs.locator('tr', { hasText: 'python' })).toContainText(/python\s*1\s*6/);
  // пропуски — по причинам, со списком путей
  const skips = box.getByTestId('code-skips');
  await expect(skips).toContainText('Пропущено: 2');
  await skips.locator('summary').click();
  await expect(skips.locator('li', { hasText: 'web/vendor.min.js' })).toContainText('минифицирован');
  await expect(skips.locator('li', { hasText: 'assets/big.ts' })).toContainText('слишком большой');
  // владелец видит очистку
  await expect(page.getByRole('button', { name: 'Очистить карту' })).toBeVisible();
  await page.screenshot({ path: `${screens}code-status.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_search_ctrl_k_opens_card', async ({ page }) => {
  const problems = watch(page);
  await openCode(page);
  await expect(statusBox(page)).toContainText('40 символов');
  await page.keyboard.press('Control+k');
  const input = page.getByRole('searchbox', { name: 'Поиск символа' });
  await expect(input).toBeFocused();
  // глобальный поиск по заметкам на странице «Код» не открывается
  await expect(page.getByRole('combobox', { name: 'Поиск по заметкам' })).toHaveCount(0);
  await input.fill('open');
  await input.press('Enter');
  const hits = page.getByRole('list', { name: 'Найденные символы' });
  const hit = hits.getByRole('link', { name: /Store::open/ });
  await expect(hit).toBeVisible();
  await hit.click();
  await expect(page).toHaveURL(new RegExp(`/j/code/${encodeURIComponent(CODE_REF).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
  await expect(page.getByRole('article', { name: 'Карточка символа' }).getByRole('heading', { level: 2 })).toHaveText(
    'Store::open',
  );
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_card_sig_refs_knowledge_links', async ({ page }) => {
  const problems = watch(page);
  await openCode(page, CODE_REF);
  const card = page.getByRole('article', { name: 'Карточка символа' });
  await expect(card.getByRole('heading', { level: 2 })).toHaveText('Store::open');
  await expect(card.getByTestId('code-sig')).toHaveText('pub fn open(path: &Path) -> Result<Store, Error>');
  await expect(card.getByTestId('code-where')).toContainText('src/a.rs:5–9');
  await expect(card).toContainText('method');
  const callers = card.getByRole('list', { name: 'Кто ссылается' });
  await expect(callers.locator('li')).toHaveCount(2);
  await expect(callers).toContainText('src/b.rs');
  await expect(callers).toContainText('src/c.rs');
  const uses = card.getByRole('list', { name: 'На что ссылается' });
  await expect(uses).toContainText('Error');
  await expect(uses).toContainText('normalize');
  const know = card.getByRole('list', { name: 'Привязанные знания' });
  // ссылка на символ — первой, затем знание о файле
  await expect(know.locator('li').first()).toContainText(CODE_NOTE_TITLE);
  await expect(know.getByRole('link', { name: CODE_TASK_TITLE })).toHaveAttribute(
    'href',
    new RegExp(`/j/tasks/${env('E2E_CODE_TASK')}$`),
  );
  await page.screenshot({ path: `${screens}code-card.png`, fullPage: true });
  await know.getByRole('link', { name: CODE_NOTE_TITLE }).click();
  await expect(page).toHaveURL(new RegExp(`/n/${env('E2E_CODE_NOTE')}$`));
  await expect(page.locator('.doc')).toContainText('Открытие хранилища берёт замок файла.');
  // символ, которого нет: понятное сообщение
  await page.goto(codeUrl(await admin(page), 'src/a.rs#Нет::такого'));
  await expect(page.getByRole('alert')).toContainText('Символ не найден');
  expect(problems.filter((p) => !p.includes('/code/symbols/')), problems.join('\n')).toEqual([]);
});

test('code_files_table_rank_lang_filter', async ({ page }) => {
  const problems = watch(page);
  await openCode(page);
  const table = page.getByRole('table', { name: 'Файлы карты' });
  const rows = table.locator('tbody tr');
  await expect(rows).toHaveCount(8);
  // по убыванию ранга (PageRank сервера); у пропущенных файлов символов нет
  const ranks = (await table.locator('tbody td.rank').allTextContents()).map(Number);
  expect([...ranks].sort((a, b) => b - a)).toEqual(ranks);
  await expect(page.getByTestId('code-files-total')).toHaveText('1–8 из 8');
  await page.getByLabel('Язык').selectOption('python');
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText('tools/gen.py');
  await expect(rows.first()).toContainText('6');
  await page.getByLabel('Язык').selectOption('');
  await page.getByLabel('Путь содержит').fill('WEB/');
  await page.getByLabel('Путь содержит').press('Enter');
  await expect(rows).toHaveCount(3);
  await expect(page.getByTestId('code-files-total')).toHaveText('1–3 из 3');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_overview_mermaid_40_files_120_edges', async ({ page }) => {
  const problems = watch(page);
  await openCode(page);
  await page.getByRole('tab', { name: 'Обзор' }).click();
  const svg = page.locator('.code-overview .mermaid-diagram svg');
  await expect(svg).toBeVisible({ timeout: 30_000 });
  const nodes = await svg.locator('g.node').count();
  expect(nodes).toBeGreaterThanOrEqual(6);
  expect(nodes).toBeLessThanOrEqual(40);
  await expect(svg).toContainText('src/a.rs');
  await page.screenshot({ path: `${screens}code-overview.png`, fullPage: true });

  const isMap = (u: URL) => u.pathname.endsWith('/api/v1/code/map');
  // большая карта (подменённый ответ): 60 файлов, 400 рёбер → 40 узлов, ≤ 120 рёбер, имена экранированы
  const files = Array.from({ length: 60 }, (_, i) => ({
    id: 1000 + i,
    path: i === 0 ? 'src/"кавычки"#[x]<y>.rs' : `src/m${i}.rs`,
    lang: 'rust',
    rank: 1 - i / 100,
    symbols: [],
  }));
  const edges = Array.from({ length: 400 }, (_, k) => ({
    src: 1000 + (k % 60),
    dst: 1000 + ((k * 7 + 1) % 60),
    weight: 400 - k,
  })).filter((e) => e.src !== e.dst);
  await page.route(isMap, (route) =>
    route.fulfill({
      json: {
        format: 'json', header: '', files, edges, files_shown: 60, symbols_shown: 0,
        files_left: 0, symbols_left: 0, tokens: 1, rank_state: 'ok', warnings: [],
      },
    }),
  );
  await page.reload();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(await svg.locator('g.node').count()).toBe(40);
  const src = await page.getByTestId('code-overview-src').textContent();
  expect(src).toContain('%%{init: {"layout": "elk"}}%%');
  expect((src!.match(/-->/g) ?? []).length).toBeLessThanOrEqual(120);
  expect((src!.match(/-->/g) ?? []).length).toBeGreaterThan(0);
  expect(src).not.toContain('"кавычки"');
  await expect(page.getByTestId('code-overview-note')).toContainText('40 из 60');
  await page.unroute(isMap);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_masked_signature_shown_masked', async ({ page }) => {
  const problems = watch(page);
  await openCode(page, 'src/b.rs#API_TOKEN');
  const sig = page.getByRole('article', { name: 'Карточка символа' }).getByTestId('code-sig');
  await expect(sig).toContainText('const API_TOKEN: &str = "');
  await expect(sig).toContainText('‹secret›');
  expect(await page.content()).not.toContain(CODE_SECRET);
  // и в выдаче поиска
  await openCode(page);
  const input = page.getByRole('searchbox', { name: 'Поиск символа' });
  await input.fill('API_TOKEN');
  await input.press('Enter');
  await expect(page.getByRole('list', { name: 'Найденные символы' })).toContainText('‹secret›');
  expect(await page.content()).not.toContain(CODE_SECRET);
  expect(problems, problems.join('\n')).toEqual([]);
});

test.describe('тёмная тема ОС', () => {
  test.use({ colorScheme: 'dark' });

  test('code_dark_theme_and_print_styles', async ({ page }) => {
    const problems = watch(page);
    await openCode(page, CODE_REF);
    const card = page.getByRole('article', { name: 'Карточка символа' });
    await expect(card.getByTestId('code-sig')).toBeVisible();
    const bg = (sel: string) => page.evaluate((s) => getComputedStyle(document.querySelector(s)!).backgroundColor, sel);
    // экран: тёмные токены (--bg #14171c, --panel #1d2128)
    expect(await bg('body')).toBe('rgb(20, 23, 28)');
    expect(await bg('article.code-card')).toBe('rgb(29, 33, 40)');
    await page.screenshot({ path: `${screens}code-dark.png`, fullPage: true });
    // печать: светлая, без навигации, дерева и кнопок; карточка видна
    await page.emulateMedia({ media: 'print' });
    expect(await bg('body')).toBe('rgb(255, 255, 255)');
    expect(await page.evaluate(() => getComputedStyle(document.querySelector('article.code-card')!).color)).toBe(
      'rgb(28, 32, 38)',
    );
    await expect(card.getByTestId('code-sig')).toBeVisible();
    await expect(page.locator('button:visible, nav:visible, input:visible')).toHaveCount(0);
    await page.emulateMedia({ media: 'screen' });
    expect(problems, problems.join('\n')).toEqual([]);
  });
});

/** Участник `rw` решения администратора: вход с обязательной сменой пароля, свой контекст. */
async function memberRw(browser: Browser, page: Page) {
  const rc = page.request;
  const me = await rc.get(`${base()}api/v1/auth/me`);
  const { csrf } = (await me.json()) as { csrf: string };
  const call = async <T>(method: string, path: string, data?: unknown) => {
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
  const name = `e2e-code-rw-${Date.now()}`;
  const u = await call<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
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
  return { ctx, id: u.user.id };
}

test('code_clear_owner_only', async ({ page, browser }) => {
  // участник `rw` видит карту, но не кнопку очистки
  const m = await memberRw(browser, page);
  const mp = await m.ctx.newPage();
  const mProblems = watch(mp);
  await mp.goto(codeUrl(m.id));
  await expect(mp.getByRole('heading', { level: 1, name: 'Код' })).toBeVisible();
  await expect(statusBox(mp)).toContainText('40 символов');
  await expect(mp.getByRole('button', { name: 'Очистить карту' })).toHaveCount(0);
  expect(mProblems, mProblems.join('\n')).toEqual([]);
  await m.ctx.close();

  // владелец: подтверждение → пустое состояние с подсказкой
  const problems = watch(page);
  await openCode(page);
  await expect(statusBox(page)).toContainText('40 символов');
  await page.getByRole('button', { name: 'Очистить карту' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg).toContainText('8 файлов');
  await dlg.getByRole('button', { name: 'Очистить' }).click();
  await expect(statusBox(page)).toContainText('zam code sync');
  await expect(page.getByRole('table', { name: 'Файлы карты' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Очистить карту' })).toHaveCount(0);
  await page.screenshot({ path: `${screens}code-empty.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
  // карта — обратно (повторный прогон отдельных сценариев)
  await seedCodeMap(base(), env('E2E_ADMIN_TOKEN'));
});
