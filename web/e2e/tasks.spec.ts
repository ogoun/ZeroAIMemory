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
