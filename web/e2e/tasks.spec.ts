// Доска задач (M7.2, спецификация §5.2 п. 1; задача владельца k:01M7GEAKFYE95RVCCSRRES6P5X — «чтобы
// перейти к другому статусу, нужно промотать полностью текущий»): загрузка по статусу (свой запрос
// на статус, по 50), числа колонок и вкладок — сервера, вкладки статусов в узкой области (телефон),
// карточка — панель поверх доски на широкой. Сценарии с точными числами — каждый в своём новом
// решении (`POST /api/v1/solutions`, пре-флайт N15): в решении E2E уже есть задачи сида и
// спецификаций, идущих раньше. Данные — REST по токену администратора (агент).
import { expect, test, type Page, type Request } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { env, pageCtx } from './mobile-pages';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, minGap, minTarget, noHorizontalScroll, touch } from './viewports';

const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/mobile/', import.meta.url));

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы; `allow(path, status)` — ожидаемые. */
function watch(page: Page, allow: (path: string, status: number) => boolean = () => false): string[] {
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
      allow(path, r.status());
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

/** REST по токену администратора (агент); `extra` — заголовки. */
async function api<T = Record<string, unknown>>(
  method: string,
  path: string,
  body?: unknown,
  extra: Record<string, string> = {},
): Promise<T> {
  const r = await fetch(`${base()}api/v1/${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...extra,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as T;
}

/** Новое решение администратора для сценария с точными числами; возвращает его GUID. */
async function newSolution(name: string): Promise<string> {
  const guid = randomUUID();
  await api('POST', 'solutions', { agent_solution_id: guid, name: `Задачи E2E: ${name}` });
  return guid;
}

/** Задача решения `guid`: создаётся открытой, другой статус — одной правкой. */
async function addTask(guid: string, title: string, status = 'open'): Promise<string> {
  const t = await api<{ id: string; rev: number }>('POST', `spaces/${guid}/entries`, { kind: 'task', title, body: '' });
  if (status !== 'open') await api('PATCH', `entries/${t.id}`, { status }, { 'If-Match': `"${t.rev}"` });
  return t.id;
}

/** Связь `part_of` шага `step` с общей задачей `parent` (`POST /edges`, M7 §5.4): порядок вызовов —
 * порядок шагов. */
async function partOf(step: string, parent: string): Promise<void> {
  await api('POST', 'edges', { src: `e:${step}`, type: 'part_of', dst: `e:${parent}` });
}

/** Шаг плана: id, заголовок и статус. */
interface Step {
  id: string;
  title: string;
  status: string;
}

/**
 * Общая задача `title` (в работе) и 13 шагов «<title>: шаг NN»: шаги 01–08 выполнены, 09–10 — в
 * работе, 11–13 открыты; связи `part_of` — в порядке (5·k) mod 13, а не в порядке создания задач
 * (раскрытие и карточка — в порядке шагов, то есть связей). Возвращает общую и шаги в порядке связей.
 */
async function umbrella13(guid: string, title: string): Promise<{ umbrella: string; steps: Step[] }> {
  const umbrella = await addTask(guid, title, 'in_progress');
  const created: Step[] = [];
  for (let i = 1; i <= 13; i++) {
    const status = i <= 8 ? 'done' : i <= 10 ? 'in_progress' : 'open';
    const t = `${title}: шаг ${pad(i)}`;
    created.push({ id: await addTask(guid, t, status), title: t, status });
  }
  const steps = Array.from({ length: 13 }, (_, k) => created[(5 * k) % 13]);
  for (const s of steps) await partOf(s.id, umbrella);
  return { umbrella, steps };
}

const STATUS_RU: Record<string, string> = { open: 'открыт', in_progress: 'в работе', done: 'выполнен' };

/** Запросы ленты задач решения `guid` (`GET …/entries?kind=task…`), по мере отправки. */
function listRequests(page: Page, guid: string): URL[] {
  const out: URL[] = [];
  page.on('request', (r: Request) => {
    const u = new URL(r.url());
    if (r.method() === 'GET' && u.pathname.endsWith(`/spaces/${guid}/entries`) && u.searchParams.get('kind') === 'task') out.push(u);
  });
  return out;
}

/** Адрес приложения (`#/…`), оболочка готова: кнопка «Поиск» — в шапке на любой ширине. */
async function open(page: Page, hash: string): Promise<void> {
  await page.goto(`${base()}${hash}`);
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
}

/** Запросы счётчиков задач решения `guid` (`GET …/entries/counts?kind=task…`). */
function countRequests(page: Page, guid: string): URL[] {
  const out: URL[] = [];
  page.on('request', (r: Request) => {
    const u = new URL(r.url());
    if (r.method() === 'GET' && u.pathname.endsWith(`/spaces/${guid}/entries/counts`) && u.searchParams.get('kind') === 'task') out.push(u);
  });
  return out;
}

const tasksUrl = (user: string, guid: string, entry = '') => `#/u/${user}/s/${guid}/j/tasks${entry ? `/${entry}` : ''}`;
const columnOf = (page: Page, status: string) =>
  page.getByRole('group', { name: 'Доска задач' }).getByRole('region', { name: `Задачи: ${status}` });
const tablist = (page: Page) => page.getByRole('tablist', { name: 'Статусы задач' });
/** Вкладка статуса по началу имени («Выполнен, 60 задач»). */
const tabOf = (page: Page, title: string) => tablist(page).getByRole('tab', { name: new RegExp(`^${title},`) });
const pad = (i: number) => String(i).padStart(2, '0');

/** Прокрутка документа устоялась: два одинаковых замера подряд; возвращает её. */
async function settledScroll(page: Page): Promise<number> {
  let y0 = -1;
  await expect
    .poll(async () => {
      const y = await page.evaluate(() => window.scrollY);
      const same = y === y0;
      y0 = y;
      return same;
    })
    .toBe(true);
  return y0;
}

test.describe('настольный', () => {
  test.use({ viewport: VIEWPORTS.desktop });

  test('tasks_board_loads_per_status', async ({ page }) => {
    test.setTimeout(180_000);
    const problems = watch(page);
    const guid = await newSolution('загрузка по статусу');
    // открытая — первой: 250 выполненных после неё вытесняют её из 200 новых задач всех статусов
    await addTask(guid, 'Старая открытая задача');
    for (let i = 1; i <= 250; i++) await addTask(guid, `Выполненная задача ${i}`, 'done');
    const lists = listRequests(page, guid);
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));

    await expect(columnOf(page, 'открыт')).toContainText('Старая открытая задача');
    // число в заголовке колонки — серверное, а не по загруженному
    await expect(columnOf(page, 'выполнен').getByRole('heading')).toHaveText('выполнен 250');
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 1');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 0');
    await expect(page.locator('[data-board]')).toHaveAttribute('data-board', 'columns');
    // каждый статус — свой запрос по 50; «Показать ещё» — у колонки
    const done = columnOf(page, 'выполнен').locator('[data-task]');
    await expect(done).toHaveCount(50);
    await expect(columnOf(page, 'открыт').getByRole('button', { name: 'Показать ещё' })).toHaveCount(0);
    // «Показать ещё» с клавиатуры: кнопка остаётся на месте (на время загрузки — `aria-disabled`),
    // фокус на ней не теряется
    const more = columnOf(page, 'выполнен').getByRole('button', { name: 'Показать ещё' });
    await more.focus();
    await page.keyboard.press('Enter');
    await expect(done).toHaveCount(100);
    await expect(more).toBeFocused();
    await expect(more).toHaveAttribute('aria-busy', 'false');
    await expect(done.first()).toContainText('Выполненная задача 250');
    const statuses = lists.map((u) => u.searchParams.get('status') ?? '');
    expect([...new Set(statuses)].sort()).toEqual(['blocked', 'done', 'dropped', 'in_progress', 'open']);
    expect(lists.every((u) => u.searchParams.get('limit') === '50'), lists.join('\n')).toBe(true);
    expect(lists.filter((u) => u.searchParams.get('cursor')).map((u) => u.searchParams.get('status'))).toEqual(['done']);
    expect(lists).toHaveLength(6);

    // правка из карточки перечитывает колонки, но не укорачивает догруженную: «выполнен» — 100
    await columnOf(page, 'открыт').getByRole('button', { name: 'Старая открытая задача' }).click();
    const card = page.getByRole('article', { name: 'Карточка задачи' });
    await card.getByRole('button', { name: '→ в работе' }).click();
    await card.getByRole('button', { name: 'Применить' }).click();
    await expect(card.getByRole('status')).toHaveText('Статус: в работе.');
    await expect(columnOf(page, 'в работе')).toContainText('Старая открытая задача');
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 0');
    await expect(done).toHaveCount(100);
    expect(lists.filter((u) => u.searchParams.get('status') === 'done').at(-1)?.searchParams.get('limit')).toBe('100');
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_overlay_desktop', async ({ page }) => {
    // 500 на правку — подменённый ответ сценария «неудачная отправка»
    const problems = watch(page, (path, status) => status === 500 && /\/entries\/[0-9A-Za-z]+$/.test(path));
    const guid = await newSolution('панель поверх доски');
    const id = await addTask(guid, 'Панель: открытая');
    const working = await addTask(guid, 'Панель: в работе', 'in_progress');
    await addTask(guid, 'Панель: выполненная', 'done');
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    const board = page.getByRole('group', { name: 'Доска задач' });
    await expect(columnOf(page, 'выполнен')).toContainText('Панель: выполненная');
    const regions = board.getByRole('region');
    await expect(regions).toHaveCount(5);
    const widths = async () => regions.evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().width)));
    const before = await widths();
    const boardBox = (await board.boundingBox())!;

    // карточка — панель 480 px у правого края под шапкой, доска не сузилась
    const tile = columnOf(page, 'открыт').getByRole('button', { name: 'Панель: открытая' });
    const historyLength = () => page.evaluate(() => history.length);
    const h0 = await historyLength();
    await tile.click();
    await expect(page).toHaveURL(new RegExp(`/j/tasks/${id}$`));
    await expect(page.locator('.jsplit')).toHaveAttribute('data-layout', 'overlay');
    const sheet = page.getByRole('dialog', { name: 'Задача', exact: true });
    const card = sheet.getByRole('article', { name: 'Карточка задачи' });
    await expect(card.getByRole('heading', { name: 'Панель: открытая' })).toBeFocused();
    const sb = (await sheet.boundingBox())!;
    const header = (await page.locator('header.topbar').boundingBox())!;
    expect(Math.round(sb.width)).toBe(480);
    expect(Math.round(sb.x + sb.width)).toBe(VIEWPORTS.desktop.width);
    expect(Math.round(sb.y)).toBe(Math.round(header.y + header.height));
    expect(await widths(), 'ширины колонок до и после').toEqual(before);
    expect((await board.boundingBox())!.width).toBe(boardBox.width);
    await page.screenshot({ path: `${screens}desktop-tasks-overlay.png` });

    // фокус — внутри панели: Tab и Shift+Tab по кругу её целей
    const inSheet = () => sheet.evaluate((s) => s.contains(document.activeElement));
    for (let i = 0; i < 12; i++) {
      await page.keyboard.press('Tab');
      expect(await inSheet(), `Tab ${i + 1}`).toBe(true);
    }
    for (let i = 0; i < 12; i++) {
      await page.keyboard.press('Shift+Tab');
      expect(await inSheet(), `Shift+Tab ${i + 1}`).toBe(true);
    }

    // фокус не уходит под фон: после шапки Tab возвращает фокус в панель, а не на доску и дерево
    const where = () =>
      page.evaluate(() => {
        const a = document.activeElement;
        if (a?.closest('[role="dialog"]')) return 'панель';
        if (a?.closest('header.topbar')) return 'шапка';
        return 'под фоном';
      });
    await page.getByRole('button', { name: 'Поиск', exact: true }).focus();
    for (let i = 0; i < 6; i++) {
      await page.keyboard.press('Tab');
      await expect.poll(where, `Tab ${i + 1} после шапки`).not.toBe('под фоном');
    }

    // Esc закрывает, фокус — на плитке; адрес журнала без записи — «назад» истории: закрытие
    // записи истории не добавляет, и «назад» браузера панель снова не открывает
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(tile).toBeFocused();
    await expect(page).toHaveURL(/\/j\/tasks$/);
    expect(await historyLength()).toBe(h0 + 1);
    // это был «назад», а не замена адреса: «вперёд» снова открывает задачу
    await page.goForward();
    await expect(card.getByRole('heading', { name: 'Панель: открытая' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(page).toHaveURL(/\/j\/tasks$/);
    // фон закрывает панель; «Закрыть» — тоже, двойной щелчок по нему не уводит назад дважды
    await tile.click();
    await expect(card).toBeVisible();
    await page.mouse.click(boardBox.x + 40, VIEWPORTS.desktop.height - 40);
    await expect(sheet).toHaveCount(0);
    await expect(tile).toBeFocused();
    await tile.click();
    await sheet.getByRole('button', { name: 'Закрыть', exact: true }).dblclick();
    await expect(sheet).toHaveCount(0);
    await expect(page).toHaveURL(/\/j\/tasks$/);
    await expect(tile).toBeFocused();
    expect(await historyLength()).toBe(h0 + 1);

    // несохранённый текст молча не теряется: Esc и фон спрашивают, «Продолжить правку» — в поле
    await tile.click();
    const step = card.getByLabel('Следующий шаг');
    await step.fill('черновик шага');
    await page.keyboard.press('Escape');
    const ask = sheet.getByRole('alertdialog', { name: 'Несохранённый текст' });
    await expect(ask).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`/j/tasks/${id}$`));
    await expect(ask.getByRole('button', { name: 'Продолжить правку' })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(ask).toHaveCount(0);
    await expect(step).toBeFocused();
    await expect(step).toHaveValue('черновик шага');
    // пока вопрос на экране, повторные щелчки по фону и «Закрыть» панель мимо него не закрывают
    await page.mouse.click(boardBox.x + 40, VIEWPORTS.desktop.height - 40);
    await expect(ask).toBeVisible();
    await expect(ask).toHaveAccessibleDescription('В карточке есть несохранённый текст. Закрыть без сохранения?');
    await page.mouse.dblclick(boardBox.x + 40, VIEWPORTS.desktop.height - 40);
    await sheet.getByRole('button', { name: 'Закрыть', exact: true }).click();
    await expect(ask).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`/j/tasks/${id}$`));
    await expect(ask.getByRole('button', { name: 'Продолжить правку' })).toBeFocused();
    await ask.getByRole('button', { name: 'Закрыть без сохранения' }).click();
    await expect(sheet).toHaveCount(0);
    await expect(tile).toBeFocused();
    // сохранённое — не черновик, и с пробелами по краям (уходит обрезанным, карточка перечитана):
    // после «Сохранить шаг» Esc закрывает сразу
    await tile.click();
    await step.fill('  сохранённый шаг  ');
    await card.getByRole('button', { name: 'Сохранить шаг' }).click();
    await expect(card.getByRole('status')).toHaveText('Следующий шаг сохранён.');
    await expect(step).toHaveValue('сохранённый шаг');
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    // неудачная отправка: ошибка видна, текст цел, Esc — снова вопрос, а не молчаливое закрытие
    await tile.click();
    await page.route(`**/api/v1/entries/${id}`, (route) =>
      route.request().method() === 'PATCH'
        ? route.fulfill({ status: 500, contentType: 'text/plain', body: 'сбой для теста' })
        : route.continue(),
    );
    await step.fill('не ушло на сервер');
    await card.getByRole('button', { name: 'Сохранить шаг' }).click();
    await expect(card.getByRole('alert')).toBeVisible();
    await expect(step).toHaveValue('не ушло на сервер');
    await page.keyboard.press('Escape');
    await expect(ask).toBeVisible();
    await page.unroute(`**/api/v1/entries/${id}`);
    await ask.getByRole('button', { name: 'Закрыть без сохранения' }).click();
    await expect(sheet).toHaveCount(0);

    // в панели открыли другую задачу (ссылка из карточки): после закрытия фокус — на её плитке
    await tile.click();
    await expect(card.getByRole('heading', { name: 'Панель: открытая' })).toBeFocused();
    await page.evaluate((h) => (location.hash = h), tasksUrl(c.user, guid, working));
    await expect(card.getByRole('heading', { name: 'Панель: в работе' })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(columnOf(page, 'в работе').getByRole('button', { name: 'Панель: в работе' })).toBeFocused();

    // адрес с записью сразу открывает панель; закрытие — заменой адреса, без новой записи истории
    await page.goto('about:blank');
    await open(page, tasksUrl(c.user, guid, id));
    await expect(sheet.getByRole('heading', { name: 'Панель: открытая' })).toBeFocused();
    const h1 = await historyLength();
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(page).toHaveURL(/\/j\/tasks$/);
    expect(await historyLength()).toBe(h1);
    await expect(tile).toBeFocused();
    // это была замена адреса, а не «назад»: «вперёд» никуда не ведёт, панель закрыта
    await page.goForward();
    await expect(page).toHaveURL(/\/j\/tasks$/);
    await expect(sheet).toHaveCount(0);
    expect(await noHorizontalScroll(page)).toBe(true);
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_overlay_esc_layers', async ({ page }) => {
    // планшет 1000 px: дерево — выдвижная панель, а область журнала шире 840 px — колонки и панель
    // карточки. Esc — самому верхнему слою: сначала выдвижной панели дерева, потом карточке
    const problems = watch(page);
    await page.setViewportSize({ width: 1000, height: 800 });
    const guid = await newSolution('слои Esc');
    await addTask(guid, 'Слои: открытая');
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    await expect(page.locator('[data-board]')).toHaveAttribute('data-board', 'columns');
    await columnOf(page, 'открыт').getByRole('button', { name: 'Слои: открытая' }).click();
    const sheet = page.getByRole('dialog', { name: 'Задача', exact: true });
    await expect(sheet).toBeVisible();
    const step = sheet.getByRole('article', { name: 'Карточка задачи' }).getByLabel('Следующий шаг');
    await step.fill('черновик');
    const toggle = page.getByRole('button', { name: 'Показать дерево' });
    await toggle.click();
    const tree = page.locator('nav[aria-label="Дерево заметок"]');
    await expect(tree).toBeInViewport();
    // фокус в выдвижной панели дерева в карточку не уводится
    await page.keyboard.press('Tab');
    await expect.poll(() => page.evaluate(() => !!document.activeElement?.closest('#sidebar'))).toBe(true);
    await page.keyboard.press('Escape');
    await expect(tree).not.toBeInViewport();
    await expect(sheet).toBeVisible();
    await expect(step).toHaveValue('черновик');
    await expect(sheet.getByRole('alertdialog')).toHaveCount(0);
    await expect(toggle).toBeFocused();
    // следующий Esc — карточке: несохранённый черновик — вопрос, а не молчаливое закрытие
    await page.keyboard.press('Escape');
    await expect(sheet.getByRole('alertdialog', { name: 'Несохранённый текст' })).toBeVisible();
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_from_idea_counts', async ({ page, browser }) => {
    // «Из идей» — и в числах колонок и вкладок (`entries/counts?from_idea=1`)
    const problems = watch(page);
    const guid = await newSolution('из идей');
    const idea = await api<{ id: string }>('POST', `spaces/${guid}/entries`, {
      kind: 'idea',
      title: 'Идея доски',
      body: 'Идея доски: замысел.',
      data: { origin: 'human' },
    });
    await api('POST', `entries/${idea.id}/decide`, { decision: 'accept', by: 'human' });
    await addTask(guid, 'Обычная задача 1');
    await addTask(guid, 'Обычная задача 2');
    await addTask(guid, 'Обычная в работе', 'in_progress');
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 3');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 1');
    const counted = page.waitForRequest((r) => r.url().includes('/entries/counts?') && r.url().includes('from_idea=1'));
    await page.getByRole('button', { name: 'Из идей' }).click();
    await counted;
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 1');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 0');
    await expect(columnOf(page, 'открыт')).toContainText('Идея доски');
    expect(problems, problems.join('\n')).toEqual([]);

    // телефон: те же числа во вкладках
    const ctx = await browser.newContext({ storageState: ADMIN_STATE, viewport: VIEWPORTS.phone, ...touch });
    const p = await ctx.newPage();
    const pProblems = watch(p);
    await open(p, tasksUrl(c.user, guid));
    await expect(tabOf(p, 'Открыт')).toHaveAccessibleName('Открыт, 3 задачи');
    await p.getByRole('button', { name: 'Из идей' }).tap();
    await expect(tabOf(p, 'Открыт')).toHaveAccessibleName('Открыт, 1 задача');
    await expect(tabOf(p, 'В работе')).toHaveAccessibleName('В работе, 0 задач');
    expect(pProblems, pProblems.join('\n')).toEqual([]);
    await ctx.close();
  });

  test('tasks_retry_after_error', async ({ page }) => {
    // колонка не загрузилась (500) — ошибка над доской; следующий переход (открытие задачи)
    // повторяет загрузку этой колонки, остальное не перечитывается
    const problems = watch(page, (path, status) => status === 500 && path.endsWith('/entries'));
    const guid = await newSolution('повтор после ошибки');
    await addTask(guid, 'Повтор: открытая');
    await addTask(guid, 'Повтор: выполненная', 'done');
    let fail = true;
    await page.route(/\/entries\?.*status=done/, async (route) => {
      if (!fail) return route.continue();
      fail = false;
      await route.fulfill({ status: 500, contentType: 'text/plain', body: 'сбой для теста' });
    });
    const lists = listRequests(page, guid);
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    await expect(columnOf(page, 'открыт')).toContainText('Повтор: открытая');
    const alert = page.locator('section.tasks').getByRole('alert');
    await expect(alert).toBeVisible();
    await expect(columnOf(page, 'выполнен')).not.toContainText('Повтор: выполненная');
    const before = lists.length;
    await columnOf(page, 'открыт').getByRole('button', { name: 'Повтор: открытая' }).click();
    await expect(page.getByRole('article', { name: 'Карточка задачи' })).toBeVisible();
    await expect(columnOf(page, 'выполнен')).toContainText('Повтор: выполненная');
    await expect(alert).toHaveCount(0);
    expect(lists.slice(before).map((u) => u.searchParams.get('status'))).toEqual(['done']);
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_status_change_reloads_both_columns', async ({ page, browser }) => {
    const problems = watch(page);
    const guid = await newSolution('смена статуса');
    const first = await addTask(guid, 'Смена: первая');
    await addTask(guid, 'Смена: вторая');
    await addTask(guid, 'Смена: в работе', 'in_progress');
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 2');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 1');

    // «→ в работе» из карточки: задача ушла из «открыт» и появилась в «в работе», числа — сервера
    await columnOf(page, 'открыт').getByRole('button', { name: 'Смена: первая' }).click();
    const card = page.getByRole('article', { name: 'Карточка задачи' });
    await card.getByRole('button', { name: '→ в работе' }).click();
    await card.getByRole('button', { name: 'Применить' }).click();
    await expect(card.getByRole('status')).toHaveText('Статус: в работе.');
    await expect(columnOf(page, 'открыт')).not.toContainText('Смена: первая');
    await expect(columnOf(page, 'в работе').locator(`[data-task="${first}"]`)).toHaveCount(1);
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 1');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 2');
    expect(problems, problems.join('\n')).toEqual([]);

    // телефон: числа вкладок после смены статуса из карточки; вкладка — прежняя
    const ctx = await browser.newContext({ storageState: ADMIN_STATE, viewport: VIEWPORTS.phone, ...touch });
    const p = await ctx.newPage();
    const pProblems = watch(p);
    await open(p, tasksUrl(c.user, guid));
    await expect(tabOf(p, 'В работе')).toHaveAttribute('aria-selected', 'true');
    await expect(tabOf(p, 'В работе')).toHaveAccessibleName('В работе, 2 задачи');
    await p.getByRole('tabpanel').getByRole('button', { name: 'Смена: первая' }).tap();
    const pc = p.getByRole('article', { name: 'Карточка задачи' });
    await pc.getByRole('button', { name: '→ выполнен' }).tap();
    await pc.getByRole('button', { name: 'Применить' }).tap();
    await expect(pc.getByRole('status')).toHaveText('Статус: выполнен.');
    await p.getByRole('link', { name: '← Задачи' }).tap();
    await expect(tabOf(p, 'В работе')).toHaveAttribute('aria-selected', 'true');
    await expect(tabOf(p, 'В работе')).toHaveAccessibleName('В работе, 1 задача');
    await expect(tabOf(p, 'Выполнен')).toHaveAccessibleName('Выполнен, 1 задача');
    await expect(p.getByRole('tabpanel')).not.toContainText('Смена: первая');
    await tabOf(p, 'Выполнен').tap();
    await expect(p.getByRole('tabpanel')).toContainText('Смена: первая');
    expect(pProblems, pProblems.join('\n')).toEqual([]);
    await ctx.close();
  });

  test('tasks_subtasks_progress_and_fold', async ({ page }) => {
    // M7.2, спецификация §5.5 п. 1 (решение 23): доска — верхний уровень (`top_level=1`), у общей —
    // «8/13» и полоса прогресса, шаги — раскрытием под плиткой в порядке шагов (`parent=<id>`),
    // вложенная общая — со своим счётчиком и раскрытием; «Подзадачи на доске» — все задачи по
    // статусам с пометкой «часть k:… «…»», как до волны
    test.setTimeout(120_000);
    const problems = watch(page);
    const guid = await newSolution('подзадачи на доске');
    const { umbrella, steps } = await umbrella13(guid, 'Этап');
    // последний шаг (09, в работе) — сам общая: два своих шага, один выполнен
    const nested = steps[12];
    const sub = [await addTask(guid, `${nested.title}.1`, 'done'), await addTask(guid, `${nested.title}.2`)];
    for (const s of sub) await partOf(s, nested.id);
    const single = await addTask(guid, 'Отдельная задача');
    const lists = listRequests(page, guid);
    const counted = countRequests(page, guid);
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));

    // верхний уровень: общая и отдельная; шагов в колонках нет, числа колонок их не считают
    const board = page.getByRole('group', { name: 'Доска задач' });
    const tile = columnOf(page, 'в работе').locator(`[data-task="${umbrella}"]`);
    await expect(tile).toBeVisible();
    await expect(columnOf(page, 'открыт').locator(`[data-task="${single}"]`)).toBeVisible();
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 1');
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 1');
    await expect(columnOf(page, 'выполнен').getByRole('heading')).toHaveText('выполнен 0');
    await expect(board.locator('[data-task]')).toHaveCount(2);
    await expect(board).not.toContainText('Этап: шаг');
    expect(lists, lists.join('\n')).toHaveLength(5);
    expect(lists.every((u) => u.searchParams.get('top_level') === '1'), lists.join('\n')).toBe(true);
    expect(counted.map((u) => u.searchParams.get('top_level'))).toEqual(['1']);

    // у общей — «8/13» и полоса прогресса; у задачи без подзадач — ни того, ни другого
    await expect(tile.getByTestId('task-progress')).toHaveText('8/13');
    const bar = tile.getByRole('progressbar', { name: 'Выполнено подзадач' });
    await expect(bar).toHaveAttribute('aria-valuenow', '8');
    await expect(bar).toHaveAttribute('aria-valuemin', '0');
    await expect(bar).toHaveAttribute('aria-valuemax', '13');
    await expect(bar).toHaveAttribute('aria-valuetext', '8 из 13');
    const singleTile = columnOf(page, 'открыт').locator(`[data-task="${single}"]`);
    await expect(singleTile.getByRole('progressbar')).toHaveCount(0);
    await expect(singleTile.getByRole('button', { name: 'Подзадачи', exact: true })).toHaveCount(0);

    // «Подзадачи ▸» — 13 строк в порядке шагов (связей, не создания), все статусы, одним запросом;
    // кнопка общей — первая в плитке (после раскрытия ниже — ещё кнопка вложенной общей)
    const fold = tile.getByRole('button', { name: 'Подзадачи', exact: true }).first();
    await expect(fold).toHaveText('Подзадачи ▸');
    await expect(fold).toHaveAttribute('aria-expanded', 'false');
    await fold.click();
    await expect(fold).toHaveAttribute('aria-expanded', 'true');
    await expect(fold).toHaveText('Подзадачи ▾');
    const list = tile.getByRole('list', { name: 'Подзадачи «Этап»' });
    const rows = list.locator(':scope > li');
    await expect(rows).toHaveCount(13);
    await expect(list.getByRole('link')).toHaveText(steps.map((s) => s.title));
    for (const [k, s] of steps.entries()) await expect(rows.nth(k)).toContainText(STATUS_RU[s.status]);
    const controls = await fold.getAttribute('aria-controls');
    expect(controls).toBeTruthy();
    await expect(page.locator(`[id="${controls}"]`).getByRole('list', { name: 'Подзадачи «Этап»' })).toBeVisible();
    const byParent = () => lists.filter((u) => u.searchParams.has('parent'));
    expect(byParent().map((u) => u.searchParams.get('parent'))).toEqual([umbrella]);
    expect(byParent()[0].searchParams.get('status')).toBeNull();
    expect(byParent()[0].searchParams.get('top_level')).toBeNull();
    expect(byParent()[0].searchParams.get('limit')).toBe('200');
    // раскрытые шаги не входят в число плиток колонки
    await expect(board.locator('[data-task]')).toHaveCount(2);

    // вложенная общая — со своим счётчиком и раскрытием
    const nestedRow = list.locator(`[data-subtask="${nested.id}"]`);
    await expect(nestedRow.getByTestId('task-progress')).toHaveText('1/2');
    await expect(nestedRow.getByRole('progressbar', { name: 'Выполнено подзадач' })).toHaveAttribute('aria-valuenow', '1');
    await nestedRow.getByRole('button', { name: 'Подзадачи', exact: true }).click();
    const nestedList = nestedRow.getByRole('list', { name: `Подзадачи «${nested.title}»` });
    await expect(nestedList.getByRole('link')).toHaveText([`${nested.title}.1`, `${nested.title}.2`]);
    await expect(rows).toHaveCount(13);
    expect(byParent().map((u) => u.searchParams.get('parent'))).toEqual([umbrella, nested.id]);
    await page.screenshot({ path: `${screens}desktop-tasks-subtasks.png` });

    // свернуть и раскрыть снова — загруженное цело, нового запроса нет
    await fold.click();
    await expect(fold).toHaveAttribute('aria-expanded', 'false');
    await expect(list).toHaveCount(0);
    await fold.click();
    await expect(rows).toHaveCount(13);
    expect(byParent()).toHaveLength(2);

    // «Подзадачи на доске» — все задачи по статусам, у шага — «часть k:… «…»» со ссылкой на общую
    const toggle = page.getByRole('button', { name: 'Подзадачи на доске' });
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
    const before = lists.length;
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await expect(columnOf(page, 'выполнен').getByRole('heading')).toHaveText('выполнен 9');
    await expect(columnOf(page, 'в работе').getByRole('heading')).toHaveText('в работе 3');
    await expect(columnOf(page, 'открыт').getByRole('heading')).toHaveText('открыт 5');
    await expect(board.locator('[data-task]')).toHaveCount(17);
    const after = lists.slice(before);
    expect(after.map((u) => u.searchParams.get('status')).sort()).toEqual(['blocked', 'done', 'dropped', 'in_progress', 'open']);
    expect(after.some((u) => u.searchParams.has('top_level') || u.searchParams.has('parent')), after.join('\n')).toBe(false);
    expect(counted.at(-1)?.searchParams.has('top_level')).toBe(false);
    const mark = board.locator(`[data-task="${steps[0].id}"]`).getByTestId('task-part-of');
    await expect(mark).toHaveText(`часть k:${umbrella} «Этап»`);
    await expect(mark).toHaveAttribute('href', tasksUrl(c.user, guid, umbrella));
    await expect(board.locator(`[data-task="${sub[0]}"]`).getByTestId('task-part-of')).toHaveText(
      `часть k:${nested.id} «${nested.title}»`,
    );
    await expect(tile.getByTestId('task-part-of')).toHaveCount(0);
    await expect(singleTile.getByTestId('task-part-of')).toHaveCount(0);
    await expect(tile.getByTestId('task-progress')).toHaveText('8/13');
    // выключен — снова верхний уровень
    await toggle.click();
    await expect(board.locator('[data-task]')).toHaveCount(2);
    await expect(columnOf(page, 'выполнен').getByRole('heading')).toHaveText('выполнен 0');
    expect(await noHorizontalScroll(page)).toBe(true);
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_subtask_card_links_parent', async ({ page }) => {
    // §5.5 п. 2: у подзадачи — «Часть: k:<id> «…» (8/13)» со ссылкой на общую; у общей — раздел
    // «Подзадачи 8/13»: шаги по порядку, статус, «в работе у <агент>», ссылка
    const problems = watch(page);
    const guid = await newSolution('карточка подзадачи');
    const { umbrella, steps } = await umbrella13(guid, 'План');
    const working = steps.find((s) => s.status === 'in_progress')!;
    await api('POST', `entries/${working.id}/claim`, { ttl_min: 60, agent: 'claude-e2e' }, { 'X-Zam-Session': 'e2e-subtasks-session' });
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid, steps[0].id));
    const sheet = page.getByRole('dialog', { name: 'Задача', exact: true });
    const card = sheet.getByRole('article', { name: 'Карточка задачи' });
    await expect(card.getByRole('heading', { name: steps[0].title })).toBeFocused();
    const part = card.getByTestId('task-parent');
    await expect(part).toHaveText(`Часть: k:${umbrella} «План» (8/13)`);
    await expect(card.getByTestId('task-subtasks')).toHaveCount(0);

    // переход открывает общую в той же панели
    await part.getByRole('link', { name: `k:${umbrella} «План»` }).click();
    await expect(page).toHaveURL(new RegExp(`/j/tasks/${umbrella}$`));
    await expect(card.getByRole('heading', { name: 'План', exact: true })).toBeFocused();
    await expect(card.getByTestId('task-parent')).toHaveCount(0);
    const section = card.getByRole('region', { name: 'Подзадачи 8/13' });
    await expect(section.getByRole('heading', { level: 3 })).toHaveText('Подзадачи 8/13');
    await expect(section.getByRole('progressbar', { name: 'Выполнено подзадач' })).toHaveAttribute('aria-valuenow', '8');
    const items = section.getByRole('listitem');
    await expect(items).toHaveCount(13);
    await expect(section.getByRole('link')).toHaveText(steps.map((s) => s.title));
    for (const [k, s] of steps.entries()) await expect(items.nth(k)).toContainText(STATUS_RU[s.status]);
    // «в работе у <агент>» — только у захваченного шага
    await expect(items.nth(steps.indexOf(working)).getByTestId('subtask-claim')).toHaveText('в работе у claude-e2e');
    await expect(section.getByTestId('subtask-claim')).toHaveCount(1);

    // шаг из раздела — его карточка в той же панели, снова с «Часть: …»
    await section.getByRole('link', { name: steps[3].title }).click();
    await expect(page).toHaveURL(new RegExp(`/j/tasks/${steps[3].id}$`));
    await expect(card.getByRole('heading', { name: steps[3].title })).toBeFocused();
    await expect(card.getByTestId('task-parent')).toHaveText(`Часть: k:${umbrella} «План» (8/13)`);
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(page).toHaveURL(/\/j\/tasks$/);
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_idle_badge', async ({ page }) => {
    // §5.5 п. 3: плашка «давно без обновлений» (`stale`) — на плитке и в карточке: часы — от
    // `last_activity_at`, точное время — в подсказке. Порог 12 ч ставит сервер, часы сервера в e2e
    // не сдвинуть (порог держат серверные тесты §5.4) — здесь поля в ответах ленты и записи подменены
    const problems = watch(page);
    const guid = await newSolution('давно без обновлений');
    const idle = await addTask(guid, 'Давняя в работе', 'in_progress');
    const fresh = await addTask(guid, 'Свежая в работе', 'in_progress');
    const last = new Date(Date.now() - 14 * 3_600_000 - 10 * 60_000).toISOString();
    const mark = (e: Record<string, unknown>) => {
      if (e.id === idle) Object.assign(e, { stale: true, last_activity_at: last });
    };
    await page.route(new RegExp(`/api/v1/spaces/${guid}/entries\\?`), async (route) => {
      const r = await route.fetch();
      const j = await r.json();
      for (const e of j.items ?? []) mark(e);
      await route.fulfill({ response: r, json: j });
    });
    await page.route(`**/api/v1/entries/${idle}`, async (route) => {
      if (route.request().method() !== 'GET') return route.continue();
      const r = await route.fetch();
      const j = await r.json();
      mark(j);
      await route.fulfill({ response: r, json: j });
    });
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    const col = columnOf(page, 'в работе');
    const badge = col.locator(`[data-task="${idle}"]`).getByTestId('task-idle');
    await expect(badge).toHaveText('в работе, без обновлений 14 ч');
    const local = await page.evaluate((iso) => new Date(iso).toLocaleString('ru-RU', { dateStyle: 'short', timeStyle: 'short' }), last);
    await expect(badge).toHaveAttribute('title', `последнее обновление — ${local}`);
    await expect(col.locator(`[data-task="${fresh}"]`).getByTestId('task-idle')).toHaveCount(0);
    // своя плашка, не пометка устаревших ссылок на файлы
    await expect(col).not.toContainText('возможно устарело');
    await col.getByRole('button', { name: 'Давняя в работе' }).click();
    const card = page.getByRole('article', { name: 'Карточка задачи' });
    await expect(card.getByTestId('task-idle')).toHaveText('в работе, без обновлений 14 ч');
    await expect(card.getByTestId('task-idle')).toHaveAttribute('title', `последнее обновление — ${local}`);
    expect(problems, problems.join('\n')).toEqual([]);
  });
});

test.describe('телефон, сенсорный ввод', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('tasks_tabs_phone', async ({ page }) => {
    test.setTimeout(120_000);
    const problems = watch(page);
    const guid = await newSolution('вкладки телефона');
    for (let i = 1; i <= 60; i++) await addTask(guid, `Выполненная ${pad(i)}`, 'done');
    const opened: string[] = [];
    for (let i = 1; i <= 3; i++) opened.push(await addTask(guid, `Открытая ${i}`));
    for (let i = 1; i <= 2; i++) await addTask(guid, `В работе ${i}`, 'in_progress');
    const lists = listRequests(page, guid);
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));

    // вкладки статусов с числами сервера; по умолчанию — «В работе»
    await expect(page.locator('[data-board]')).toHaveAttribute('data-board', 'tabs');
    const tabs = tablist(page).getByRole('tab');
    await expect(tabs).toHaveText(['Открыт 3', 'В работе 2', 'Заблокирован 0', 'Выполнен 60', 'Снят 0']);
    const names = ['Открыт, 3 задачи', 'В работе, 2 задачи', 'Заблокирован, 0 задач', 'Выполнен, 60 задач', 'Снят, 0 задач'];
    for (const [i, name] of names.entries()) await expect(tabs.nth(i)).toHaveAccessibleName(name);
    await expect(tabOf(page, 'В работе')).toHaveAttribute('aria-selected', 'true');
    // все вкладки — в окне без прокрутки (перенос, а не полоса), цели касания 44 px с зазором
    for (let i = 0; i < 5; i++) await expect(tabs.nth(i)).toBeInViewport({ ratio: 1 });
    await minTarget(tabs, 44);
    await minGap(tabs, 8);
    const panel = page.getByRole('tabpanel');
    await expect(panel).toHaveAccessibleName('В работе, 2 задачи');
    await expect(panel.locator('[data-task]')).toHaveCount(2);
    // на вкладках грузится только выбранная
    expect(lists.map((u) => u.searchParams.get('status'))).toEqual(['in_progress']);
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-tasks-tabs.png` });

    // «Выполнен» — одним касанием: первая выполненная — в видимой области без прокрутки
    await tabOf(page, 'Выполнен').tap();
    await expect(tabOf(page, 'Выполнен')).toHaveAttribute('aria-selected', 'true');
    const tiles = panel.locator('[data-task]');
    await expect(tiles.first()).toContainText('Выполненная 60');
    await expect(tiles.first()).toBeInViewport({ ratio: 1 });
    expect(await page.evaluate(() => window.scrollY)).toBe(0);
    expect(lists.map((u) => u.searchParams.get('status'))).toEqual(['in_progress', 'done']);
    // по 50 и «Показать ещё» у вкладки
    await expect(tiles).toHaveCount(50);
    await panel.getByRole('button', { name: 'Показать ещё' }).tap();
    await expect(tiles).toHaveCount(60);
    const loaded = lists.length;

    // открытие задачи — карточка вместо доски; «← Задачи» — та же вкладка и позиция прокрутки
    const row = panel.getByRole('button', { name: 'Выполненная 20', exact: true });
    await row.scrollIntoViewIfNeeded();
    const y0 = await settledScroll(page);
    expect(y0, 'доска прокручена').toBeGreaterThan(300);
    const rowTop = (await row.boundingBox())!.y;
    await row.tap();
    const card = page.getByRole('article', { name: 'Карточка задачи' });
    const title = card.getByRole('heading', { name: 'Выполненная 20' });
    await expect(title).toBeInViewport({ ratio: 1 });
    await expect(title).toBeFocused();
    await expect(page.locator('.jsplit')).toHaveAttribute('data-layout', 'stack');
    await expect(page.getByRole('group', { name: 'Доска задач' })).toBeHidden();
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-tasks-card.png` });
    await page.getByRole('link', { name: '← Задачи' }).tap();
    await expect(card).toHaveCount(0);
    await expect(tabOf(page, 'Выполнен')).toHaveAttribute('aria-selected', 'true');
    await expect(row).toBeFocused();
    expect(Math.abs((await row.boundingBox())!.y - rowTop), 'строка — на прежнем месте окна').toBeLessThanOrEqual(2);
    expect(Math.abs((await page.evaluate(() => window.scrollY)) - y0), `scrollY к ${y0}`).toBeLessThanOrEqual(2);
    // загруженные страницы целы: доска не перечитывалась
    await expect(tiles).toHaveCount(60);
    expect(lists).toHaveLength(loaded);

    // задача из адреса открывает вкладку своего статуса (а не «В работе» по умолчанию)
    await page.goto('about:blank');
    await open(page, tasksUrl(c.user, guid, opened[0]));
    await expect(card.getByRole('heading', { name: 'Открытая 1' })).toBeFocused();
    await page.getByRole('link', { name: '← Задачи' }).tap();
    await expect(tabOf(page, 'Открыт')).toHaveAttribute('aria-selected', 'true');
    await expect(panel.getByRole('button', { name: 'Открытая 1' })).toBeFocused();
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('tasks_tabs_count_top_level', async ({ page }) => {
    // §5.5 п. 4: числа вкладок — верхнего уровня (`entries/counts?top_level=1`), подзадачи не
    // считаются; «Подзадачи на доске» — числа всех задач. Раскрытие шагов — касанием, цели 44 px
    const problems = watch(page);
    const guid = await newSolution('вкладки без подзадач');
    const umbrella = await addTask(guid, 'Общая на телефоне', 'in_progress');
    const titles: string[] = [];
    const steps: string[] = [];
    for (const [i, st] of ['done', 'done', 'done', 'in_progress', 'in_progress', 'open'].entries()) {
      titles.push(`Шаг телефона ${i + 1}`);
      steps.push(await addTask(guid, titles[i], st));
    }
    for (const s of steps) await partOf(s, umbrella);
    await addTask(guid, 'Отдельная открытая');
    const counted = countRequests(page, guid);
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    const tabs = tablist(page).getByRole('tab');
    await expect(tabs).toHaveText(['Открыт 1', 'В работе 1', 'Заблокирован 0', 'Выполнен 0', 'Снят 0']);
    await expect(tabOf(page, 'В работе')).toHaveAccessibleName('В работе, 1 задача');
    await expect(tabOf(page, 'В работе')).toHaveAttribute('aria-selected', 'true');
    expect(counted.map((u) => u.searchParams.get('top_level'))).toEqual(['1']);

    // общая — во вкладке «В работе» с прогрессом; шаги — касанием «Подзадачи»
    const panel = page.getByRole('tabpanel');
    const tile = panel.locator(`[data-task="${umbrella}"]`);
    await expect(tile.getByTestId('task-progress')).toHaveText('3/6');
    await expect(panel.locator('[data-task]')).toHaveCount(1);
    const fold = tile.getByRole('button', { name: 'Подзадачи', exact: true });
    await minTarget(fold, 44);
    await fold.tap();
    const list = tile.getByRole('list', { name: 'Подзадачи «Общая на телефоне»' });
    await expect(list.getByRole('link')).toHaveText(titles);
    await minTarget(list.getByRole('link'), 44);
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-tasks-subtasks.png` });

    // шаг из раскрытия — карточка вместо доски; «← Задачи» — та же вкладка, раскрытие и фокус на шаге
    const link = list.getByRole('link', { name: 'Шаг телефона 1' });
    await link.tap();
    const card = page.getByRole('article', { name: 'Карточка задачи' });
    await expect(card.getByRole('heading', { name: 'Шаг телефона 1' })).toBeFocused();
    await expect(card.getByTestId('task-parent')).toHaveText(`Часть: k:${umbrella} «Общая на телефоне» (3/6)`);
    await page.getByRole('link', { name: '← Задачи' }).tap();
    await expect(card).toHaveCount(0);
    await expect(tabOf(page, 'В работе')).toHaveAttribute('aria-selected', 'true');
    await expect(link).toBeFocused();
    // шаг из адреса — вкладка общей (у шага на доске верхнего уровня своей плитки нет)
    await page.goto('about:blank');
    await open(page, tasksUrl(c.user, guid, steps[0]));
    await expect(card.getByRole('heading', { name: 'Шаг телефона 1' })).toBeFocused();
    await page.getByRole('link', { name: '← Задачи' }).tap();
    await expect(tabOf(page, 'В работе')).toHaveAttribute('aria-selected', 'true');

    // «Подзадачи на доске» — числа всех задач
    const before = counted.length;
    await page.getByRole('button', { name: 'Подзадачи на доске' }).tap();
    await expect(tabs).toHaveText(['Открыт 2', 'В работе 3', 'Заблокирован 0', 'Выполнен 3', 'Снят 0']);
    expect(counted.slice(before).map((u) => u.searchParams.has('top_level'))).toEqual([false]);
    expect(problems, problems.join('\n')).toEqual([]);
  });
});

test.describe('телефон, клавиатура', () => {
  test.use({ viewport: VIEWPORTS.phone });

  test('tasks_tabs_keyboard', async ({ page }) => {
    const problems = watch(page);
    const guid = await newSolution('вкладки с клавиатуры');
    await addTask(guid, 'Клавиши: открытая');
    await addTask(guid, 'Клавиши: в работе', 'in_progress');
    await addTask(guid, 'Клавиши: выполненная', 'done');
    const c = await pageCtx(page);
    await open(page, tasksUrl(c.user, guid));
    const tabs = tablist(page).getByRole('tab');
    await expect(tabs).toHaveText(['Открыт 1', 'В работе 1', 'Заблокирован 0', 'Выполнен 1', 'Снят 0']);
    const panel = page.getByRole('tabpanel');
    /** Выбрана и в фокусе вкладка `title`; бегающий `tabindex` — только у неё. */
    const at = async (title: string) => {
      const t = tabOf(page, title);
      await expect(t).toHaveAttribute('aria-selected', 'true');
      await expect(t).toBeFocused();
      expect(await tabs.evaluateAll((els) => els.filter((e) => e.getAttribute('tabindex') === '0').length)).toBe(1);
      await expect(t).toHaveAttribute('tabindex', '0');
    };
    await tabOf(page, 'В работе').focus();
    await at('В работе');
    await expect(panel).toContainText('Клавиши: в работе');
    await page.keyboard.press('ArrowRight');
    await at('Заблокирован');
    await expect(panel).toContainText('Задач нет.');
    await page.keyboard.press('End');
    await at('Снят');
    await page.keyboard.press('ArrowRight');
    await at('Открыт');
    await expect(panel).toContainText('Клавиши: открытая');
    await page.keyboard.press('ArrowLeft');
    await at('Снят');
    await page.keyboard.press('Home');
    await at('Открыт');
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('ArrowLeft');
    await at('Выполнен');
    await expect(panel).toContainText('Клавиши: выполненная');
    // Tab — из списка вкладок на панель (`tabindex="0"`, как в ARIA APG), затем на её содержимое;
    // пустая вкладка («Задач нет.») так тоже достижима
    await page.keyboard.press('Tab');
    await expect(panel).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(panel.getByRole('button', { name: 'Клавиши: выполненная' })).toBeFocused();
    await tabOf(page, 'Выполнен').focus();
    await page.keyboard.press('End');
    await at('Снят');
    await expect(panel).toContainText('Задач нет.');
    await page.keyboard.press('Tab');
    await expect(panel).toBeFocused();
    expect(problems, problems.join('\n')).toEqual([]);
  });
});
