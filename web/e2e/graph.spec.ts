// Локальный граф связей (M7.2, спецификация §5.10): переключатель «Список / Граф» на вкладке
// «Связи» документа и в разделе «Связи» карточки записи; глубина и фильтр типа — запросом
// `GET /graph`; узлы — ссылки в порядке списка с именем «<тип связи>: <заголовок>»; сдвиг, щипок,
// «+», «−», «По размеру»; печать — список; больше 60 узлов — «Показано 60 из N». Данные — своё
// новое решение (`POST /api/v1/solutions`, как в tasks.spec): заметки и связи по токену
// администратора (агент). Снимки — в .tmp/e2e/screens/ (под .gitignore).
import { expect, test, type Locator, type Page } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { env, pageCtx } from './mobile-pages';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, touch } from './viewports';

const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));

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

/** REST по токену администратора (агент). */
async function api<T = Record<string, unknown>>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(`${base()}api/v1/${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as T;
}

interface Fixture {
  guid: string;
  center: string;
  alpha: string;
  insight: string;
}

let made: Promise<Fixture> | null = null;

/**
 * Решение сценариев (одно на файл): «Центр» ссылается на «Альфа», «Бета», «Гамма» (вики-ссылки,
 * `links_to`), «Дельта» — на «Центр» (явная `relates_to`, обратная ссылка), «Альфа» — на «Эпсилон»
 * (второй шаг от центра); insight со связью `relates_to` к «Альфа». Цели создаются раньше
 * ссылок: порядок рёбер — порядок ссылок в тексте.
 */
function fixture(): Promise<Fixture> {
  made ??= (async () => {
    const guid = randomUUID();
    await api('POST', 'solutions', { agent_solution_id: guid, name: `Граф E2E ${Date.now()}` });
    const note = async (name: string, text = '') =>
      (await api<{ id: string }>('POST', 'nodes', { space: guid, path: name, kind: 'note', content: `# ${name}\n\n${text}\n` })).id;
    await note('Эпсилон');
    const alpha = await note('Альфа', 'Дальше — [[Эпсилон]].');
    await note('Бета');
    await note('Гамма');
    const center = await note('Центр', 'Ссылки: [[Альфа]], [[Бета]] и [[Гамма]].');
    const delta = await note('Дельта');
    await api('POST', 'edges', { src: `n:${delta}`, type: 'relates_to', dst: `n:${center}` });
    const insight = (
      await api<{ id: string }>('POST', `spaces/${guid}/entries`, {
        kind: 'insight',
        title: 'Граф: запись о связях',
        body: 'Запись со связью к заметке.',
        data: { kind: 'discovery' },
        links: [{ type: 'relates_to', target: `node:${alpha}` }],
      })
    ).id;
    return { guid, center, alpha, insight };
  })();
  return made;
}

/** Открывает вкладку «Связи» заметки `node` решения сценариев. */
async function openLinks(page: Page, node: string): Promise<void> {
  const { guid } = await fixture();
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${guid}/n/${node}`);
  await page.getByRole('tab', { name: 'Связи', exact: true }).click();
}

const views = (page: Page) => page.getByRole('tablist', { name: 'Вид связей' });
const graphOf = (page: Page) => page.getByRole('group', { name: 'Граф связей' });

/** Имена узлов-ссылок графа в порядке документа (порядок Tab). */
const labels = (g: Locator) => g.getByRole('link').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label')));

/** `viewBox` холста: x, y, ширина, высота. */
async function viewBox(g: Locator): Promise<number[]> {
  return ((await g.getAttribute('viewBox')) ?? '').split(' ').map(Number);
}

test('links_graph_depth_and_navigate', async ({ page }) => {
  const problems = watch(page);
  const fx = await fixture();
  await openLinks(page, fx.center);
  // по умолчанию — список
  await expect(views(page).getByRole('tab', { name: 'Список' })).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('region', { name: 'Исходящие связи' })).toContainText('Гамма');

  // «Граф»: четыре узла и центр; порядок — как в списке (исходящие, затем обратная ссылка)
  const first = page.waitForRequest((r) => r.url().includes('/api/v1/graph?') && r.url().includes(`node=${fx.center}`));
  await views(page).getByRole('tab', { name: 'Граф' }).click();
  await first;
  const g = graphOf(page);
  await expect(g.getByRole('link')).toHaveCount(4);
  await expect(g.getByRole('img', { name: 'Текущая: Центр' })).toBeVisible();
  const depth1 = ['links_to: Альфа', 'links_to: Бета', 'links_to: Гамма', 'relates_to: Дельта'];
  expect(await labels(g)).toEqual(depth1);
  await expect(page.getByRole('region', { name: 'Исходящие связи' })).toBeHidden();
  await page.screenshot({ path: `${screens}graph.png`, fullPage: true });

  // Tab по узлам — в порядке списка
  await g.getByRole('link').first().focus();
  for (const name of depth1) {
    await expect(page.locator(':focus')).toHaveAttribute('aria-label', name);
    await page.keyboard.press('Tab');
  }

  // глубина 2 — больше узлов: второй шаг через «Альфа» — «Эпсилон» (исходящая) и запись со связью
  // к «Альфа» (обратная)
  const deep = page.waitForRequest((r) => r.url().includes('/api/v1/graph?') && r.url().includes('depth=2'));
  await page.getByLabel('Глубина').selectOption('2');
  await deep;
  await expect(g.getByRole('link')).toHaveCount(6);
  expect(await labels(g)).toEqual([...depth1, 'links_to: Эпсилон', 'relates_to: Граф: запись о связях']);

  // фильтр типа — меньше: фильтрует сервер (`types`), «Дельта» связана relates_to
  const typed = page.waitForRequest((r) => r.url().includes('types=links_to') && r.url().includes('depth=2'));
  await page.getByLabel('Тип связи').selectOption('links_to');
  await typed;
  await expect(g.getByRole('link')).toHaveCount(4);
  expect(await labels(g)).toEqual(['links_to: Альфа', 'links_to: Бета', 'links_to: Гамма', 'links_to: Эпсилон']);
  await expect(page.getByRole('status').filter({ hasText: 'Показано' })).toHaveCount(0);

  // нажатие узла открывает заметку
  await g.getByRole('link', { name: 'links_to: Альфа' }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${fx.guid}/n/${fx.alpha}$`));
  // у «Альфа» узел-запись (Enter) открывает её в журнале её вида — по `GET /entries/{id}`
  await page.getByRole('tab', { name: 'Связи', exact: true }).click();
  await views(page).getByRole('tab', { name: 'Граф' }).click();
  await expect(g.getByRole('link')).toHaveCount(3);
  expect(await labels(g)).toEqual(['links_to: Эпсилон', 'links_to: Центр', 'relates_to: Граф: запись о связях']);
  await g.getByRole('link', { name: 'relates_to: Граф: запись о связях' }).focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp(`/s/${fx.guid}/j/insights/${fx.insight}$`));
  expect(problems, problems.join('\n')).toEqual([]);
});

test('links_graph_entry_card_zoom_pan_print', async ({ page }) => {
  // карточка записи (Insights), кнопки масштаба, сдвиг мышью, печать — список; без анимации при
  // prefers-reduced-motion — узлы сразу на месте
  const problems = watch(page);
  const fx = await fixture();
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${fx.guid}/j/insights/${fx.insight}`);
  const card = page.locator('article').filter({ hasText: 'Граф: запись о связях' }).last();
  await card.getByRole('tablist', { name: 'Вид связей' }).getByRole('tab', { name: 'Граф' }).click();
  const g = card.getByRole('group', { name: 'Граф связей' });
  await expect(g.getByRole('img', { name: 'Текущая: Граф: запись о связях' })).toBeVisible();
  await expect(g.getByRole('link')).toHaveCount(1);
  await expect(g.getByRole('link')).toHaveAttribute('aria-label', 'relates_to: Альфа');
  // глубина 2: «Эпсилон» (через «Альфа») и «Центр» (ссылается на «Альфа»); перестройка при
  // reduced motion — без анимации: «Альфа» сразу на новом месте
  const at = () =>
    g.getByRole('link', { name: 'relates_to: Альфа' }).evaluate((a) => (a.parentElement as Element).getAttribute('transform'));
  const before = await at();
  await card.getByLabel('Глубина').selectOption('2');
  await expect(g.getByRole('link')).toHaveCount(3);
  const t0 = await at();
  await page.waitForTimeout(150);
  expect(await at(), 'reduced motion: узел сразу на месте').toBe(t0);
  expect(t0, 'перестройка сдвинула «Альфа»').not.toBe(before);

  // «+» — крупнее (вид уже), «−» дважды — мельче исходного, «По размеру» — исходный
  const fit = await viewBox(g);
  await card.getByRole('button', { name: 'Увеличить' }).click();
  expect((await viewBox(g))[2]).toBeLessThan(fit[2]);
  await card.getByRole('button', { name: 'Уменьшить' }).click();
  await card.getByRole('button', { name: 'Уменьшить' }).click();
  expect((await viewBox(g))[2]).toBeGreaterThan(fit[2]);
  await card.getByRole('button', { name: 'По размеру' }).click();
  expect(await viewBox(g)).toEqual(fit);

  // сдвиг мышью: содержимое — за указателем (вид — в обратную сторону); отпускание — не переход
  await g.scrollIntoViewIfNeeded();
  const box = (await g.boundingBox())!;
  await page.mouse.move(box.x + 12, box.y + 12);
  await page.mouse.down();
  await page.mouse.move(box.x + 52, box.y + 32, { steps: 4 });
  await page.mouse.up();
  const moved = await viewBox(g);
  expect(moved[0]).toBeLessThan(fit[0]);
  expect(moved[1]).toBeLessThan(fit[1]);
  expect(moved[2]).toBe(fit[2]);
  await expect(page).toHaveURL(new RegExp(`/j/insights/${fx.insight}$`));

  // печать — список, без графа и переключателя
  await page.emulateMedia({ media: 'print' });
  await expect(card.getByRole('region', { name: 'Исходящие связи' })).toBeVisible();
  await expect(card.getByRole('region', { name: 'Исходящие связи' })).toContainText('Альфа');
  await expect(g).toBeHidden();
  await expect(card.getByRole('tablist', { name: 'Вид связей' })).toBeHidden();
  await page.emulateMedia({ media: 'screen' });
  await expect(g).toBeVisible();
  await expect(card.getByRole('region', { name: 'Исходящие связи' })).toBeHidden();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('links_graph_cap_60', async ({ page }) => {
  // больше 60 узлов — 60 ближайших и «Показано 60 из N» (подграф подставлен: 80 узлов, сервер
  // обрезал — «+»); узлы второго шага не показаны раньше первого
  const problems = watch(page);
  const fx = await fixture();
  const me = `n:${fx.center}`;
  const node = (ref: string, title: string) => ({ ref, space_id: fx.guid, title, path: title, status: null, certainty: null, deleted: false });
  const edge = (i: number, src: string, dst: string) => ({
    id: `e${i}`, src, src_space: fx.guid, dst, dst_space: fx.guid, dst_raw: null, dst_anchor: null,
    type: 'links_to', origin: 'explicit', created_at: '2026-11-23T00:00:00Z',
  });
  const near = Array.from({ length: 50 }, (_, i) => node(`n:near${i}`, `Ближний ${i}`));
  const far = Array.from({ length: 29 }, (_, i) => node(`n:far${i}`, `Дальний ${i}`));
  const body = {
    // второй шаг — раньше первого во входе: отбор — по шагам, а не по порядку ответа
    nodes: [node(me, 'Центр'), ...far, ...near],
    edges: [...near.map((n, i) => edge(i, me, n.ref)), ...far.map((n, i) => edge(100 + i, near[i % 50].ref, n.ref))],
    truncated: true,
  };
  await page.route('**/api/v1/graph?*', (route) => route.fulfill({ json: body }));
  await openLinks(page, fx.center);
  await views(page).getByRole('tab', { name: 'Граф' }).click();
  const g = graphOf(page);
  await expect(g.getByRole('link')).toHaveCount(59);
  await expect(page.getByRole('status').filter({ hasText: 'Показано' })).toHaveText('Показано 60 из 80+');
  const shown = await labels(g);
  expect(shown.slice(0, 50)).toEqual(near.map((n) => `links_to: ${n.title}`));
  expect(shown.slice(50)).toEqual(far.slice(0, 9).map((n) => `links_to: ${n.title}`));
  await page.screenshot({ path: `${screens}graph-60.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});

test.describe('телефон', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('links_graph_phone_list_default_pan_pinch', async ({ page }) => {
    // на телефоне вид по умолчанию — список; граф — сдвиг одним пальцем и щипок (касания CDP)
    const problems = watch(page);
    const fx = await fixture();
    await openLinks(page, fx.center);
    await expect(views(page).getByRole('tab', { name: 'Список' })).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByRole('region', { name: 'Исходящие связи' })).toBeVisible();
    await expect(graphOf(page)).toHaveCount(0);

    await views(page).getByRole('tab', { name: 'Граф' }).click();
    const g = graphOf(page);
    await expect(g.getByRole('link')).toHaveCount(4);
    await g.scrollIntoViewIfNeeded();
    const fit = await viewBox(g);
    const box = (await g.boundingBox())!;
    const cdp = await page.context().newCDPSession(page);
    const touchAt = (type: string, pts: [number, number][]) =>
      cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });
    // щипок: пальцы расходятся от середины — крупнее
    const [cx, cy] = [box.x + box.width / 2, box.y + box.height / 2];
    await touchAt('touchStart', [[cx - 20, cy], [cx + 20, cy]]);
    for (const d of [35, 50, 65, 80]) await touchAt('touchMove', [[cx - d, cy], [cx + d, cy]]);
    await touchAt('touchEnd', []);
    const pinched = await viewBox(g);
    expect(pinched[2]).toBeLessThan(fit[2] * 0.6);
    // сдвиг одним пальцем: вид — в обратную сторону, масштаб прежний
    await touchAt('touchStart', [[box.x + 10, box.y + 10]]);
    for (const d of [10, 20, 30, 40]) await touchAt('touchMove', [[box.x + 10 + d, box.y + 10 + d]]);
    await touchAt('touchEnd', []);
    const panned = await viewBox(g);
    expect(panned[0]).toBeLessThan(pinched[0]);
    expect(panned[1]).toBeLessThan(pinched[1]);
    expect(panned[2]).toBeCloseTo(pinched[2], 6);
    await page.screenshot({ path: `${screens}graph-phone.png`, fullPage: true });
    expect(problems, problems.join('\n')).toEqual([]);
  });
});
