// Узкие экраны и сенсорный ввод (M7.2, спецификация §5.1): классы ширины (шапка, выдвижная
// панель, дерево), цели касания 44 px, полоса журналов, «список — карточка» (`JournalSplit`) в
// «Словаре», сбой ленивого чанка, отсутствие горизонтальной прокрутки. Сессия — общая
// администратора; на телефоне «Выйти» — в меню аккаунта, поэтому сценарии ждут оболочку по кнопке
// «Поиск» (есть на всех классах ширины). Размеры и помощники — viewports.ts, адреса — mobile-pages.ts.
import { expect, test, type Browser, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PAGES, env, pageCtx } from './mobile-pages';
import { SOLUTION_GUID } from './seed';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, minTarget, noHorizontalScroll, touch, type ViewportName } from './viewports';

const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/mobile/', import.meta.url));

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

/** Адрес приложения (`#/…`), оболочка готова: кнопка «Поиск» — в шапке на любой ширине. */
async function open(page: Page, hash = ''): Promise<void> {
  await page.goto(`${base()}${hash}`);
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
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

// По CSS, а не по роли: закрытая панель inert и выпадает из дерева доступности.
const sidebar = (page: Page) => page.locator('#sidebar');
const treeNav = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');
const header = (page: Page) => page.locator('header.topbar');

test.describe('телефон, сенсорный ввод', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('touch_emulation_pointer_coarse', async ({ page }) => {
    // способ эмуляции касания в Edge: hasTouch включает (pointer: coarse) без CDP (viewports.ts)
    await page.setContent('<meta name="viewport" content="width=device-width, initial-scale=1"><p>x</p>');
    const m = await page.evaluate(() => ({
      coarse: matchMedia('(pointer: coarse)').matches,
      hoverNone: matchMedia('(hover: none)').matches,
      width: innerWidth,
    }));
    expect(m).toEqual({ coarse: true, hoverNone: true, width: VIEWPORTS.phone.width });
  });

  test('phone_drawer_has_sections', async ({ page }) => {
    const problems = watch(page);
    await open(page);
    // шапка телефона: ☰, заголовок раздела, поиск, меню аккаунта; разделов в шапке нет
    await expect(header(page).getByRole('navigation', { name: 'Разделы' })).toHaveCount(0);
    await expect(header(page).locator('.section-title')).toHaveText('Заметки');
    await expect(page.getByRole('button', { name: 'Выйти' })).toHaveCount(0);
    await expect(sidebar(page)).not.toBeInViewport();

    const toggle = page.getByRole('button', { name: 'Показать дерево' });
    await toggle.click();
    await expect(sidebar(page)).toBeInViewport();
    await expect(page.getByRole('button', { name: 'Скрыть дерево' })).toHaveAttribute('aria-expanded', 'true');
    // в панели — «Разделы» строками по 44 px, затем дерево
    const sections = sidebar(page).getByRole('navigation', { name: 'Разделы' });
    for (const name of ['Заметки', 'Индекс', 'Администрирование', 'Подключение агента', 'Кэш выжимок', 'Сессии']) {
      await expect(sections.getByRole('link', { name, exact: true })).toBeVisible();
    }
    await minTarget(sections.getByRole('link'), 44);
    await expect(treeNav(page)).toBeVisible();
    const order = await sidebar(page).evaluate((s) => {
      const nav = s.querySelector('nav[aria-label="Разделы"]')!;
      const tree = s.querySelector('nav[aria-label="Дерево заметок"]')!;
      return Boolean(nav.compareDocumentPosition(tree) & Node.DOCUMENT_POSITION_FOLLOWING);
    });
    expect(order, 'разделы — перед деревом').toBe(true);
    // фокус — в панели; Esc закрывает, фокус — на ☰
    expect(await sidebar(page).evaluate((s) => s.contains(document.activeElement))).toBe(true);
    await page.keyboard.press('Escape');
    await expect(sidebar(page)).not.toBeInViewport();
    await expect(toggle).toBeFocused();

    // фон закрывает панель; переход по разделу — тоже, заголовок шапки — новый раздел
    await toggle.click();
    await expect(sidebar(page)).toBeInViewport();
    await page.mouse.click(VIEWPORTS.phone.width - 10, VIEWPORTS.phone.height / 2);
    await expect(sidebar(page)).not.toBeInViewport();
    await expect(toggle).toBeFocused();
    await toggle.click();
    await sections.getByRole('link', { name: 'Сессии' }).click();
    await expect(page).toHaveURL(/#\/sessions$/);
    await expect(sidebar(page)).not.toBeInViewport();
    await expect(header(page).locator('.section-title')).toHaveText('Сессии');

    // меню аккаунта: логин и «Выйти»; Esc закрывает, фокус — на кнопке меню
    const account = page.getByRole('button', { name: 'Меню аккаунта' });
    await account.click();
    await expect(account).toHaveAttribute('aria-expanded', 'true');
    await expect(page.locator('#account-menu')).toContainText('admin');
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Выйти' })).toHaveCount(0);
    await expect(account).toBeFocused();

    await page.screenshot({ path: `${screens}phone-header.png` });
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('touch_targets_44', async ({ page }) => {
    const problems = watch(page);
    const c = await pageCtx(page);
    await open(page, `#/u/${c.user}/s/${SOLUTION_GUID}/j/glossary`);
    // ссылки полосы журналов
    const strip = page.getByRole('navigation', { name: 'Журналы пространства' });
    await expect(strip.getByRole('link')).toHaveCount(10);
    await minTarget(strip.getByRole('link'), 44);
    // поле поиска словаря и Ctrl+K — шрифт не меньше 16 px (Safari иначе увеличивает страницу)
    const fontPx = (sel: string) => page.locator(sel).evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
    expect(await fontPx('input[aria-label="Поиск по словарю"]')).toBeGreaterThanOrEqual(16);

    // дерево: строки, раскрывашки, «⋯», ссылки журналов
    await page.getByRole('button', { name: 'Показать дерево' }).click();
    await expect(sidebar(page)).toBeInViewport();
    const t = treeNav(page);
    await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
    await expect(t.getByRole('list', { name: 'Журналы: Knowledge' })).toBeVisible();
    await minTarget(t.locator('.tree-row'), 44);
    await minTarget(t.locator('button.twisty'), 44);
    await minTarget(t.locator('button.more'), 44);
    await minTarget(t.locator('.tree-row .label'), 44);
    await page.keyboard.press('Escape');
    await expect(sidebar(page)).not.toBeInViewport();

    await page.getByRole('button', { name: 'Поиск', exact: true }).click();
    const q = page.getByRole('combobox', { name: 'Поиск по заметкам' });
    await expect(q).toBeFocused();
    expect(await q.evaluate((el) => parseFloat(getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(16);
    // поиск на телефоне — во весь экран, закрывается кнопкой
    const dlg = page.getByRole('dialog');
    const box = (await dlg.boundingBox())!;
    expect([box.x, box.y, box.width, box.height]).toEqual([0, 0, VIEWPORTS.phone.width, VIEWPORTS.phone.height]);
    await page.getByRole('button', { name: 'Закрыть поиск' }).click();
    await expect(dlg).toBeHidden();
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('journal_strip_single_row_phone', async ({ page }) => {
    const problems = watch(page);
    const c = await pageCtx(page);
    // «Словарь» — шестой из десяти журналов решения: на 390 px он за краем полосы
    await open(page, `#/u/${c.user}/s/${SOLUTION_GUID}/j/glossary`);
    const strip = page.getByRole('navigation', { name: 'Журналы пространства' });
    const links = strip.getByRole('link');
    await expect(links).toHaveCount(10);
    const boxes = await links.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON() as DOMRect));
    const top = boxes[0].top;
    expect(boxes.every((b) => Math.abs(b.top - top) < 1), 'полоса — одна строка').toBe(true);
    expect(await strip.evaluate((s) => s.scrollWidth > s.clientWidth), 'полоса прокручивается').toBe(true);
    // выбранный журнал — целиком в видимой части полосы
    const selected = strip.locator('[aria-current="page"]');
    await expect(selected).toHaveText('Словарь');
    await expect
      .poll(async () => {
        const [s, l] = await Promise.all([strip.boundingBox(), selected.boundingBox()]);
        return !!s && !!l && l.x >= s.x - 1 && l.x + l.width <= s.x + s.width + 1;
      })
      .toBe(true);
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-journal-strip.png` });
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('glossary_card_replaces_list_phone', async ({ page, browser }) => {
    const problems = watch(page);
    // 40 терминов в Knowledge — список длиннее окна; имена уникальны на прогон
    const run = Date.now().toString(36);
    const name = (i: number) => `Моб ${run} ${String(i).padStart(2, '0')}`;
    for (let i = 1; i <= 40; i++) {
      await api('POST', 'spaces/knowledge/entries', {
        kind: 'term',
        title: name(i),
        body: '',
        data: { definition: `Определение мобильного термина номер ${i} для проверки раскладки`, aliases: [], code: [] },
      });
    }
    const c = await pageCtx(page);
    const listUrl = `#/u/${c.user}/s/knowledge/j/glossary`;
    await open(page, listUrl);
    const split = page.locator('.jsplit');
    await expect(split).toHaveAttribute('data-layout', 'stack');
    const list = page.getByRole('list', { name: 'Термины' });
    const row = list.getByRole('button', { name: name(30), exact: true });
    // фильтр списка — по метке прогона (все 40): после возврата он цел
    const find = page.getByRole('searchbox', { name: 'Поиск по словарю' });
    await find.fill(run);
    await expect(list.getByRole('listitem')).toHaveCount(40);
    await row.scrollIntoViewIfNeeded();
    // прокрутка устоялась: два одинаковых замера подряд
    let y0 = -1;
    await expect
      .poll(async () => {
        const y = await page.evaluate(() => window.scrollY);
        const same = y === y0;
        y0 = y;
        return same;
      })
      .toBe(true);
    expect(y0, 'список прокручен').toBeGreaterThan(300);
    const rowTop = (await row.boundingBox())!.y;

    // открытие: карточка вместо списка, заголовок — в видимой области под шапкой, фокус — на нём
    await row.click();
    const card = page.getByRole('article', { name: 'Карточка термина' });
    const title = card.getByRole('heading', { name: name(30) });
    await expect(title).toBeInViewport({ ratio: 1 });
    await expect(title).toBeFocused();
    const headerBottom = (await header(page).boundingBox())!;
    expect((await title.boundingBox())!.y).toBeGreaterThanOrEqual(headerBottom.y + headerBottom.height - 1);
    await expect(list).toBeHidden();
    await expect(page).toHaveURL(/\/j\/glossary\/[0-9A-Za-z]+$/);

    // «← Словарь»: список на прежней позиции, фокус — на строке термина
    await page.getByRole('link', { name: '← Словарь' }).click();
    await expect(list).toBeVisible();
    await expect(card).toHaveCount(0);
    await expect(row).toBeFocused();
    expect(Math.abs((await row.boundingBox())!.y - rowTop), 'строка — на прежнем месте окна').toBeLessThanOrEqual(2);
    expect(Math.abs((await page.evaluate(() => window.scrollY)) - y0), `scrollY к ${y0}`).toBeLessThanOrEqual(2);
    await expect(find).toHaveValue(run);

    // «назад» браузера — то же, что «←»
    await row.click();
    await expect(title).toBeFocused();
    await page.goBack();
    await expect(list).toBeVisible();
    await expect(row).toBeFocused();
    await page.screenshot({ path: `${screens}phone-glossary-list.png` });
    expect(problems, problems.join('\n')).toEqual([]);

    // настольный: список и карточка рядом
    // мышь: параметры касания describe переходят и в контексты, созданные в тесте
    const ctx = await browser.newContext({ storageState: ADMIN_STATE, viewport: VIEWPORTS.desktop, hasTouch: false, isMobile: false });
    const dp = await ctx.newPage();
    const dProblems = watch(dp);
    await open(dp, listUrl);
    await dp.getByRole('list', { name: 'Термины' }).getByRole('button', { name: name(30), exact: true }).click();
    await expect(dp.locator('.jsplit')).toHaveAttribute('data-layout', 'split');
    const dCard = dp.getByRole('article', { name: 'Карточка термина' });
    await expect(dCard.getByRole('heading', { name: name(30) })).toBeVisible();
    const dList = dp.getByRole('list', { name: 'Термины' });
    await expect(dList).toBeVisible();
    const [lb, cb] = [(await dList.boundingBox())!, (await dCard.boundingBox())!];
    expect(cb.x, 'карточка справа от списка').toBeGreaterThanOrEqual(lb.x + lb.width);
    expect(cb.width).toBeGreaterThanOrEqual(380);
    await expect(dp.getByRole('link', { name: '← Словарь' })).toHaveCount(0);
    await dp.screenshot({ path: `${screens}desktop-glossary-split.png` });
    expect(dProblems, dProblems.join('\n')).toEqual([]);
    await ctx.close();
  });
});

test.describe('планшет, сенсорный ввод', () => {
  test.use({ viewport: VIEWPORTS.tablet, ...touch });

  test('tablet_tree_is_drawer', async ({ page }) => {
    const problems = watch(page);
    await open(page);
    // разделы — в шапке короткими подписями (имя ссылки — полное), «Выйти» — в шапке
    const sections = header(page).getByRole('navigation', { name: 'Разделы' });
    await expect(sections).toBeVisible();
    await expect(sections.getByRole('link', { name: 'Администрирование' })).toHaveText('Админ', { useInnerText: true });
    await expect(sections.getByRole('link', { name: 'Сессии' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Меню аккаунта' })).toHaveCount(0);
    // дерево — выдвижная панель, в ней только дерево
    await expect(treeNav(page)).not.toBeInViewport();
    await page.getByRole('button', { name: 'Показать дерево' }).click();
    await expect(treeNav(page)).toBeInViewport();
    await expect(sidebar(page).getByRole('navigation', { name: 'Разделы' })).toHaveCount(0);
    await page.keyboard.press('Escape');
    await expect(treeNav(page)).not.toBeInViewport();
    await expect(page.getByRole('button', { name: 'Показать дерево' })).toBeFocused();
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}tablet-header.png` });
    expect(problems, problems.join('\n')).toEqual([]);
  });
});

test.describe('настольный', () => {
  test.use({ viewport: VIEWPORTS.desktop });

  test('desktop_tree_fixed', async ({ page }) => {
    const problems = watch(page);
    await open(page);
    await expect(treeNav(page)).toBeInViewport();
    await expect(page.getByRole('button', { name: 'Показать дерево' })).toBeHidden();
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    const sections = header(page).getByRole('navigation', { name: 'Разделы' });
    await expect(sections.getByRole('link', { name: 'Администрирование' })).toHaveText('Администрирование', { useInnerText: true });
    // дерево 300 px с 1280, 260 px — на 1024–1279
    expect((await sidebar(page).boundingBox())!.width).toBe(300);
    await page.setViewportSize(VIEWPORTS.laptop);
    await expect.poll(async () => (await sidebar(page).boundingBox())!.width).toBe(260);
    await expect(treeNav(page)).toBeInViewport();
    await expect(page.getByRole('button', { name: 'Показать дерево' })).toBeHidden();
    expect(await noHorizontalScroll(page)).toBe(true);
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('lazy_chunk_failure_reload', async ({ page }) => {
    // отказ загрузки чанка страницы «Сессии»: текст ошибки и «Обновить страницу»
    const chunk = /\/assets\/SessionsView-[^/]+\.js$/;
    await page.route(chunk, (r) => r.abort('failed'));
    await open(page, '#/sessions');
    const fail = page.getByRole('alert').filter({ hasText: 'Страница не загрузилась' });
    await expect(fail).toBeVisible();
    const reload = fail.getByRole('button', { name: 'Обновить страницу' });
    await expect(reload).toBeVisible();
    // перезагрузка (адрес сохраняется) с доступным чанком — страница открывается
    await page.unroute(chunk);
    await reload.click();
    await expect(page).toHaveURL(/#\/sessions$/);
    await expect(page.getByRole('heading', { name: 'Сессии', level: 2 })).toBeVisible();
    await expect(page.getByText('Страница не загрузилась')).toHaveCount(0);
  });
});

/** Новая вкладка с общей сессией администратора на размере `v` (касание — у телефонов и планшетов). */
async function contextAt(browser: Browser, v: ViewportName) {
  const touchy = v === 'phoneS' || v === 'phone' || v === 'tablet' || v === 'tabletLand';
  return browser.newContext({ storageState: ADMIN_STATE, viewport: VIEWPORTS[v], ...(touchy ? touch : { hasTouch: false, isMobile: false }) });
}

test('frame_no_horizontal_scroll', async ({ browser }) => {
  test.setTimeout(180_000);
  mkdirSync(screens, { recursive: true });
  const frame = PAGES.filter((p) => ['стартовая', 'документ', 'журнал'].includes(p.name));
  expect(frame).toHaveLength(3);
  const wide: string[] = [];
  for (const v of Object.keys(VIEWPORTS) as ViewportName[]) {
    const ctx = await contextAt(browser, v);
    const page = await ctx.newPage();
    const problems = watch(page);
    const c = await pageCtx(page);
    for (const p of frame) {
      await open(page, p.path(c));
      if (p.name === 'документ') {
        await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Медиа-заметка/ })).toBeVisible();
        await expect(page.locator('.doc .mermaid-diagram svg')).toBeVisible({ timeout: 20_000 });
      } else if (p.name === 'журнал') {
        await expect(page.getByRole('heading', { level: 1, name: 'Словарь' })).toBeVisible();
        await expect(page.locator('.jsplit')).toBeVisible();
      } else {
        await expect(page.locator('.welcome h1')).toBeVisible();
      }
      // дерево на узком окне — после анимации панели
      await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== 'running'));
      if (!(await noHorizontalScroll(page))) wide.push(`${v} ${VIEWPORTS[v].width}: ${p.name}`);
      await page.screenshot({ path: `${screens}frame-${v}-${p.name}.png`, fullPage: true });
    }
    expect(problems, `${v}: ${problems.join('\n')}`).toEqual([]);
    await ctx.close();
  }
  expect(wide, 'горизонтальная прокрутка страницы').toEqual([]);
});
