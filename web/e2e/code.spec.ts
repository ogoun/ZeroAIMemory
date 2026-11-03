// Сквозные сценарии страницы «Код» (M5.6, спецификация §11): пункт в дереве решения (у
// Knowledge — нет), состояние снимка (языки, пропуски по причинам, возраст, ветка и коммит),
// поиск символа по Ctrl+K страницы, карточка символа (сигнатура, строки, кто ссылается, на что
// ссылается, привязанные знания со ссылками), таблица файлов по рангу с фильтром языка, «Обзор»
// Mermaid (≤ 40 файлов, ≤ 120 рёбер; экранирование подписей; кэш ответа между вкладками;
// печать светлой копией в тёмной теме), вкладки ARIA с клавиатуры, очистка карты только
// владельцем, маска секрета в сигнатуре, тёмная тема и печать. Данные — seedCodeMap/seedCodeKnowledge (global-setup).
import { expect, test, type Browser, type Locator, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  CODE_BOTH_TITLE,
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
  // ссылки на символ — первыми, затем знания о файле
  await expect(know.locator('li').first()).toContainText('о символе');
  await expect(know.locator('li').last()).toContainText('о файле src/a.rs');
  await expect(know.locator('li', { hasText: CODE_NOTE_TITLE })).toContainText('о символе');
  // заметка со ссылками и на файл, и на этот символ — одной строкой (в ответе две строки одного id)
  const both = know.locator('li', { hasText: CODE_BOTH_TITLE });
  await expect(both).toHaveCount(1);
  await expect(both).toContainText('о символе');
  await expect(both.getByRole('link')).toHaveAttribute('href', new RegExp(`/n/${env('E2E_CODE_BOTH')}$`));
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

test('code_card_knowledge_link_to_record_space', async ({ page }) => {
  const problems = watch(page);
  const base_ = base();
  const auth = { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` };
  // знание из другого пространства (Knowledge участника): ссылка — в его адрес (`space` знания)
  const sp = await page.request.get(`${base_}api/v1/spaces`, { headers: auth });
  expect(sp.ok()).toBe(true);
  const spaces = ((await sp.json()) as { spaces: { id: string; kind: string }[] }).spaces;
  const knowledgeId = spaces.find((s) => s.kind === 'knowledge')!.id;
  const solutionId = spaces.find((s) => s.kind === 'solution')?.id;
  const title = `Знание из Knowledge ${Date.now()}`;
  const r = await page.request.post(`${base_}api/v1/nodes`, {
    headers: auth,
    data: { space: 'knowledge', path: `E2E/${title}`, kind: 'note', content: `# ${title}\n\nЗаметка другого пространства.\n` },
  });
  expect(r.ok(), await r.text()).toBe(true);
  const node = (await r.json()) as { id: string; current_rev: number };
  const isCard = (u: URL) => u.pathname.includes('/api/v1/code/symbols/');
  await page.route(isCard, async (route) => {
    const resp = await route.fetch();
    const card = (await resp.json()) as { knowledge: Record<string, unknown>[] };
    card.knowledge.push({ path: 'src/a.rs', id: `n:${node.id}`, kind: 'note', title, state: 'ok', space: knowledgeId, symbol: 'Store::open' });
    return route.fulfill({ json: card });
  });
  try {
    await openCode(page, CODE_REF);
    const know = page.getByRole('article', { name: 'Карточка символа' }).getByRole('list', { name: 'Привязанные знания' });
    const link = know.getByRole('link', { name: title });
    await expect(link).toHaveAttribute('href', `#/u/${await admin(page)}/s/${knowledgeId}/n/${node.id}`);
    // знания решения: с `space` сервера (задача 9A) — его Id, без поля — текущее пространство
    const own = await know.getByRole('link', { name: CODE_NOTE_TITLE }).getAttribute('href');
    expect(own).toMatch(new RegExp(`/s/(${SOLUTION_GUID}${solutionId ? `|${solutionId}` : ''})/n/${env('E2E_CODE_NOTE')}$`));
    await link.click();
    await expect(page).toHaveURL(new RegExp(`/s/${knowledgeId}/n/${node.id}$`));
    await expect(page.locator('.doc')).toContainText('Заметка другого пространства.');
  } finally {
    await page.unroute(isCard);
    await page.request.delete(`${base_}api/v1/nodes/${node.id}`, { headers: { ...auth, 'If-Match': String(node.current_rev) } });
  }
  expect(problems, problems.join('\n')).toEqual([]);
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

test('code_tabs_keyboard_aria', async ({ page }) => {
  const problems = watch(page);
  await openCode(page);
  const list = page.getByRole('tablist', { name: 'Вид карты' });
  const files = list.getByRole('tab', { name: 'Файлы' });
  const over = list.getByRole('tab', { name: 'Обзор' });
  await expect(files).toHaveAttribute('aria-selected', 'true');
  await expect(files).toHaveAttribute('tabindex', '0');
  await expect(over).toHaveAttribute('aria-selected', 'false');
  await expect(over).toHaveAttribute('tabindex', '-1');
  // вкладка управляет своей панелью, панель подписана вкладкой
  const panelOf = async (tab: Locator) => page.locator(`[id="${await tab.getAttribute('aria-controls')}"]`);
  const filesPanel = await panelOf(files);
  const overPanel = await panelOf(over);
  await expect(filesPanel).toHaveAttribute('role', 'tabpanel');
  await expect(filesPanel).toHaveAttribute('aria-labelledby', (await files.getAttribute('id'))!);
  await expect(overPanel).toHaveAttribute('aria-labelledby', (await over.getAttribute('id'))!);
  await expect(page.getByRole('tabpanel', { name: 'Файлы' }).getByRole('table', { name: 'Файлы карты' })).toBeVisible();
  await expect(overPanel).toBeHidden();
  // клавиатура: ←/→ выбирают и фокусируют соседнюю вкладку по кругу; Home/End — крайние
  await files.focus();
  await page.keyboard.press('ArrowRight');
  await expect(over).toBeFocused();
  await expect(over).toHaveAttribute('aria-selected', 'true');
  await expect(over).toHaveAttribute('tabindex', '0');
  await expect(files).toHaveAttribute('tabindex', '-1');
  await expect(overPanel).toBeVisible();
  await expect(filesPanel).toBeHidden();
  await page.keyboard.press('ArrowRight');
  await expect(files).toBeFocused();
  await expect(files).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('ArrowLeft');
  await expect(over).toBeFocused();
  await page.keyboard.press('Home');
  await expect(files).toBeFocused();
  await expect(files).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('End');
  await expect(over).toBeFocused();
  await expect(over).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('tabpanel', { name: 'Обзор' })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_overview_cached_between_tabs', async ({ page }) => {
  const problems = watch(page);
  let maps = 0;
  const urls: URL[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname.endsWith('/api/v1/code/map')) {
      maps++;
      urls.push(u);
    }
  });
  await openCode(page);
  const svg = page.locator('.code-overview .mermaid-diagram svg').first();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(maps).toBe(1);
  // только первые 40 файлов и рёбра между ними (`files`, задача 9A)
  expect(urls[0].searchParams.get('files')).toBe('40');
  expect(urls[0].searchParams.get('format')).toBe('json');
  const first = await svg.getAttribute('id');
  // туда и обратно: тот же снимок — без запроса и без нового mermaid.render (SVG из кэша: тот же id)
  await page.getByRole('tab', { name: 'Файлы' }).click();
  await expect(page.getByRole('table', { name: 'Файлы карты' })).toBeVisible();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(await svg.getAttribute('id')).toBe(first);
  // карточка символа и назад к карте — тоже из кэша
  await page.goto(codeUrl(await admin(page), CODE_REF));
  await expect(page.getByRole('article', { name: 'Карточка символа' })).toBeVisible();
  await page.getByRole('link', { name: '← К карте' }).click();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(maps).toBe(1);
  // новая загрузка страницы — новый запрос (кэш живёт в памяти страницы)
  await page.reload();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(maps).toBe(2);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_overview_rereads_status_new_snapshot', async ({ page }) => {
  const problems = watch(page);
  let maps = 0;
  let statuses = 0;
  // «фоновая синхронизация»: после первого «Обзора» сервер отдаёт другой снимок
  let synced = false;
  const NEW_COMMIT = 'feedface00000000000000000000000000000000';
  page.on('request', (r) => {
    if (new URL(r.url()).pathname.endsWith('/api/v1/code/map')) maps++;
  });
  const isStatus = (u: URL) => u.pathname.endsWith('/api/v1/code/status');
  await page.route(isStatus, async (route) => {
    statuses++;
    const resp = await route.fetch();
    if (!synced) return route.fulfill({ response: resp });
    const body = (await resp.json()) as Record<string, unknown>;
    return route.fulfill({ json: { ...body, etag: `${body.etag}-synced`, commit: NEW_COMMIT } });
  });
  await openCode(page);
  const svg = page.locator('.code-overview .mermaid-diagram svg').first();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(maps).toBe(1);
  // показ «Обзора» перечитывает состояние; снимок тот же — карта из кэша
  await expect.poll(() => statuses).toBe(2);
  await page.getByRole('tab', { name: 'Файлы' }).click();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect.poll(() => statuses).toBe(3);
  await expect(svg).toBeVisible({ timeout: 30_000 });
  expect(maps).toBe(1);
  // новый снимок: состояние обновлено, «Обзор» запрошен заново
  synced = true;
  await page.getByRole('tab', { name: 'Файлы' }).click();
  await page.getByRole('tab', { name: 'Обзор' }).click();
  await expect(statusBox(page)).toContainText(`${CODE_BRANCH}@${NEW_COMMIT.slice(0, 7)}`);
  await expect.poll(() => maps).toBe(2);
  await expect(svg).toBeVisible({ timeout: 30_000 });
  // возврат с карточки тоже перечитывает состояние
  const before = statuses;
  await page.goto(codeUrl(await admin(page), CODE_REF));
  await expect(page.getByRole('article', { name: 'Карточка символа' })).toBeVisible();
  await page.getByRole('link', { name: '← К карте' }).click();
  await expect(page.getByRole('tablist', { name: 'Вид карты' })).toBeVisible();
  await expect.poll(() => statuses).toBe(before + 1);
  await page.unroute(isStatus);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('code_overview_mermaid_escapes_labels', async ({ page }) => {
  const problems = watch(page);
  const dialogs: string[] = [];
  page.on('dialog', (d) => {
    dialogs.push(d.message());
    void d.dismiss();
  });
  // имена, которые ломали бы лексер Mermaid или давали разметку
  const paths = [
    'src/a:b@c=d.rs',
    'src/style:x#1;y.rs',
    'src/classDef:k#35;v.rs',
    'src/"q"|p;(r)[s]{t}.rs',
    'src/<img src=x onerror=alert(1)>.ts',
    'src/e1@-->f@{ shape: circle }.ts',
    'src/%%{init: {}}%%&amp;#lt;.ts',
    'src/пробел и ёлка`~+,!.py',
  ];
  const files = paths.map((path, i) => ({ id: 2000 + i, path, lang: 'rust', rank: 1 - i / 100, symbols: [] }));
  const edges = paths.slice(1).map((_, i) => ({ src: 2000 + i, dst: 2001 + i, weight: 10 - i }));
  const isMap = (u: URL) => u.pathname.endsWith('/api/v1/code/map');
  await page.route(isMap, (route) =>
    route.fulfill({
      json: {
        format: 'json', header: '', files, edges, files_shown: files.length, symbols_shown: 0,
        files_left: 0, symbols_left: 0, tokens: 1, rank_state: 'ok', warnings: [],
      },
    }),
  );
  await openCode(page);
  await page.getByRole('tab', { name: 'Обзор' }).click();
  const svg = page.locator('.code-overview .mermaid-diagram svg').first();
  await expect(svg).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('.mermaid-error')).toHaveCount(0);
  expect(await svg.locator('g.node').count()).toBe(paths.length);
  // исходник: подписи без «опасных» символов вне сущностей `#<код>;`, все рёбра на месте
  const src = (await page.getByTestId('code-overview-src').textContent())!;
  const labels = src.split('\n').filter((l) => /^ {2}f\d+\["/.test(l));
  expect(labels).toHaveLength(paths.length);
  for (const l of labels) {
    const body = l.slice(l.indexOf('["') + 2, l.lastIndexOf('"]'));
    expect(body, l).not.toMatch(/[:=@"<>|()[\]{}`%&~+,!]/);
    expect(body.replace(/#\d+;/g, ''), l).not.toMatch(/[#;]/);
  }
  expect((src.match(/-->/g) ?? []).length).toBe(paths.length - 1);
  // диаграмма: подписи — те же имена буквально (Mermaid переносит строки по пробелам — без них)
  const flat = (t: string) => t.replace(/\s+/g, '');
  const text = (await svg.locator('g.node').allTextContents()).map(flat);
  for (const p of paths) expect(text, p).toContain(flat(p));
  // никакой разметки из имён: ни картинок, ни обработчиков, ни диалогов
  expect(await page.locator('.code-overview img, .code-overview [onerror]').count()).toBe(0);
  expect(dialogs).toEqual([]);
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

test.describe('тёмная тема ОС: печать «Обзора»', () => {
  test.use({ colorScheme: 'dark' });

  test('code_overview_print_light_without_controls', async ({ page }) => {
    const problems = watch(page);
    await openCode(page);
    await page.getByRole('tab', { name: 'Обзор' }).click();
    const screen = page.locator('.code-overview:not(.print-only) .mermaid-diagram svg');
    const print = page.getByTestId('code-overview-print').locator('.mermaid-diagram svg');
    await expect(screen).toBeVisible({ timeout: 30_000 });
    // светлая копия нарисована заранее и на экране не видна
    await expect(print).toBeAttached({ timeout: 30_000 });
    await expect(print).toBeHidden();
    const fill = (l: Locator) => l.locator('g.node').first().locator('rect, polygon, path').first()
      .evaluate((e) => getComputedStyle(e).fill);
    const light = (c: string) => {
      const [r, g, b] = (c.match(/\d+/g) ?? []).map(Number);
      return (r + g + b) / 3 > 160;
    };
    const darkFill = await fill(screen);
    expect(light(darkFill), darkFill).toBe(false);
    await page.emulateMedia({ media: 'print' });
    // печать: только светлая копия; без исходника, вкладок, кнопок и форм (заголовок списка
    // пропусков состояния карты печатается — это текст)
    await expect(print).toBeVisible();
    await expect(screen).toBeHidden();
    const printFill = await fill(print);
    expect(light(printFill), printFill).toBe(true);
    await expect(page.getByTestId('code-overview-note')).toBeVisible();
    await expect(page.locator('details.src')).toBeHidden();
    await expect(page.getByRole('tablist', { name: 'Вид карты' })).toBeHidden();
    await expect(
      page.locator('button:visible, nav:visible, input:visible, select:visible, .src summary:visible'),
    ).toHaveCount(0);
    const bg = (sel: string) => page.evaluate((s) => getComputedStyle(document.querySelector(s)!).backgroundColor, sel);
    expect(await bg('body')).toBe('rgb(255, 255, 255)');
    expect(await bg('.code-overview.print-only')).toBe('rgb(255, 255, 255)');
    await page.screenshot({ path: `${screens}code-overview-print.png`, fullPage: true });
    await page.emulateMedia({ media: 'screen' });
    await expect(screen).toBeVisible();
    await expect(print).toBeHidden();
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

  // владелец: подтверждение → пустое состояние с подсказкой; карта — обратно в любом исходе
  // (повторный прогон отдельных сценариев)
  try {
    await clearAsOwner(page);
  } finally {
    await seedCodeMap(base(), env('E2E_ADMIN_TOKEN'));
  }
});

async function clearAsOwner(page: Page) {
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
  // на пустой карте Ctrl+K не перехватывается: открывается общий поиск
  await page.keyboard.press('Control+k');
  await expect(page.getByRole('combobox', { name: 'Поиск по заметкам' })).toBeVisible();
  await page.keyboard.press('Escape');
  expect(problems, problems.join('\n')).toEqual([]);
}
