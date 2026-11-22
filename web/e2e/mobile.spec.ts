// Узкие экраны и сенсорный ввод (M7.2, спецификация §5.1): классы ширины (шапка, выдвижная
// панель, дерево), цели касания 44 px, полоса журналов, «список — карточка» (`JournalSplit`) в
// «Словаре», «Идеи» (аккордеон разделов, фильтры под кнопкой) и «Insights» (компактная таблица) на
// телефоне (§5.2 пп. 2–3; доска задач — tasks.spec.ts), сбой ленивого чанка, отсутствие
// горизонтальной прокрутки. Сессия — общая
// администратора; на телефоне «Выйти» — в меню аккаунта, поэтому сценарии ждут оболочку по кнопке
// «Поиск» (есть на всех классах ширины). Размеры и помощники — viewports.ts, адреса — mobile-pages.ts.
import { expect, test, type Browser, type Locator, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PAGES, env, pageCtx } from './mobile-pages';
import { IDEAS_GUID, SOLUTION_GUID } from './seed';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, minGap, minTarget, noHorizontalScroll, touch, type ViewportName } from './viewports';

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
    await expect(header(page).getByText('Заметки', { exact: true })).toBeVisible();
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
    // строки «Разделов» — по левому краю, как строки дерева
    const align = await sections.getByRole('link').evaluateAll((els) => els.map((e) => getComputedStyle(e).justifyContent));
    expect(align.filter((j) => j === 'center'), 'строки «Разделов» не по центру').toEqual([]);
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
    // выбор текущего раздела (адрес не меняется) — тоже закрывает
    await toggle.click();
    await expect(sidebar(page)).toBeInViewport();
    await sections.getByRole('link', { name: 'Заметки', exact: true }).click();
    await expect(sidebar(page)).not.toBeInViewport();
    await expect(toggle).toBeFocused();
    await toggle.click();
    await sections.getByRole('link', { name: 'Сессии' }).click();
    await expect(page).toHaveURL(/#\/sessions$/);
    await expect(sidebar(page)).not.toBeInViewport();
    await expect(header(page).getByText('Сессии', { exact: true })).toBeVisible();

    // меню аккаунта: логин и «Выйти»; панель меню — в DOM (на неё ссылается aria-controls);
    // Esc закрывает, фокус — на кнопке меню
    const account = page.getByRole('button', { name: 'Меню аккаунта', exact: true });
    const controls = await account.getAttribute('aria-controls');
    expect(await page.locator(`[id="${controls}"]`).count(), 'aria-controls — на существующий узел').toBe(1);
    await account.click();
    await expect(account).toHaveAttribute('aria-expanded', 'true');
    await expect(header(page).getByText('admin', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Выйти' })).toHaveCount(0);
    await expect(account).toBeFocused();
    // Esc закрывает меню и когда фокус не в нём (Safari и Firefox на macOS не фокусируют кнопку
    // по щелчку)
    await account.click();
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Выйти' })).toHaveCount(0);
    await expect(account).toHaveAttribute('aria-expanded', 'false');

    // смена класса ширины закрывает панель: телефон → планшет (фокус с исчезнувших «Разделов» —
    // на ☰), затем настольный и обратно — панель не открывается сама
    await toggle.click();
    await expect(sidebar(page)).toBeInViewport();
    expect(await sidebar(page).evaluate((s) => s.contains(document.activeElement))).toBe(true);
    await page.setViewportSize(VIEWPORTS.tablet);
    await expect(page.getByRole('button', { name: 'Показать дерево' })).toBeFocused();
    await expect(treeNav(page)).not.toBeInViewport();
    await page.getByRole('button', { name: 'Показать дерево' }).click();
    await expect(treeNav(page)).toBeInViewport();
    await page.setViewportSize(VIEWPORTS.desktop);
    await expect(page.getByRole('button', { name: 'Скрыть дерево' })).toHaveCount(0);
    await page.setViewportSize(VIEWPORTS.phone);
    await expect(page.getByRole('button', { name: 'Показать дерево' })).toHaveAttribute('aria-expanded', 'false');
    await expect(sidebar(page)).not.toBeInViewport();

    await page.screenshot({ path: `${screens}phone-header.png` });
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('touch_targets_44', async ({ page }) => {
    const problems = watch(page);
    const c = await pageCtx(page);
    await open(page, `#/u/${c.user}/s/${SOLUTION_GUID}/j/glossary`);
    // ссылки полосы журналов: 44 × 44 и зазор между соседними не меньше 8 px
    const strip = page.getByRole('navigation', { name: 'Журналы пространства' });
    await expect(strip.getByRole('link')).toHaveCount(10);
    await minTarget(strip.getByRole('link'), 44);
    expect(await minGap(strip.getByRole('link'), 8)).toBe(9);
    // поле поиска словаря и Ctrl+K — шрифт не меньше 16 px (Safari иначе увеличивает страницу)
    const fontPx = (l: Locator) => l.evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
    expect(await fontPx(page.getByRole('searchbox', { name: 'Поиск по словарю' }))).toBeGreaterThanOrEqual(16);

    // дерево: раскрывашки, строки (кнопки и ссылки), «⋯» — 44 × 44, в строке зазор не меньше 8 px
    await page.getByRole('button', { name: 'Показать дерево' }).click();
    await expect(sidebar(page)).toBeInViewport();
    const t = treeNav(page);
    await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
    await expect(t.getByRole('list', { name: 'Журналы: Knowledge' })).toBeVisible();
    await minTarget(t.getByRole('button', { name: /^(Развернуть|Свернуть) / }), 44);
    await minTarget(t.getByRole('button', { name: /^Действия: / }), 44);
    await minTarget(t.getByRole('button'), 44);
    await minTarget(t.getByRole('link'), 44);
    expect(await minGap(t.getByRole('button').or(t.getByRole('link')), 8)).toBeGreaterThan(10);
    // пункты контекстного меню — по 44 px; Esc закрывает меню, а не панель
    await t.getByRole('button', { name: 'Действия: Knowledge' }).click();
    await expect(page.getByRole('menuitem').first()).toBeFocused();
    await minTarget(page.getByRole('menuitem'), 44);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('menuitem')).toHaveCount(0);
    await expect(sidebar(page)).toBeInViewport();
    await page.keyboard.press('Escape');
    await expect(sidebar(page)).not.toBeInViewport();

    // «Копировать» над кодом — прежнего размера (не закрывает строки), цель касания расширена
    // `::after`: точки в 8 px выше и ниже кнопки попадают в неё
    await open(page, `#/u/${c.user}/s/knowledge/n/${env('E2E_NOTE_MEDIA')}`);
    const copy = page.locator('.doc').getByRole('button', { name: 'Копировать' }).first();
    await expect(copy).toBeVisible();
    await copy.scrollIntoViewIfNeeded();
    const cb = (await copy.boundingBox())!;
    expect(cb.height, 'видимая высота «Копировать»').toBeLessThan(32);
    const hits = await copy.evaluate((el) => {
      const r = el.getBoundingClientRect();
      const x = r.left + r.width / 2;
      return [r.top - 8, r.bottom + 8].map((y) => document.elementFromPoint(x, y) === el);
    });
    expect(hits, 'расширенная область касания').toEqual([true, true]);

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
    const ids: string[] = [];
    for (let i = 1; i <= 40; i++) {
      const t = await api<{ id: string }>('POST', 'spaces/knowledge/entries', {
        kind: 'term',
        title: name(i),
        body: '',
        data: { definition: `Определение мобильного термина номер ${i} для проверки раскладки`, aliases: [], code: [] },
      });
      ids.push(t.id);
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

    // открытие по адресу (поиск, ссылка `k:`): карточка сразу, фокус — на её заголовке; смена
    // записи в адресе внутри карточки — фокус на заголовке новой; «←» — фокус на строку записи
    await page.goto('about:blank');
    await open(page, `${listUrl}/${ids[34]}`);
    await expect(card.getByRole('heading', { name: name(35) })).toBeFocused();
    await expect(list).toBeHidden();
    await page.evaluate((h) => (location.hash = h), `${listUrl}/${ids[19]}`);
    await expect(card.getByRole('heading', { name: name(20) })).toBeFocused();
    await page.getByRole('link', { name: '← Словарь' }).click();
    const row20 = list.getByRole('button', { name: name(20), exact: true });
    await expect(row20).toBeFocused();
    await expect(row20).toBeInViewport();
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

  test('ideas_groups_accordion_phone', async ({ page }) => {
    // «Идеи» на телефоне (§5.2 п. 2): фильтры под кнопкой «Фильтры», чипы — полоса, «Группировать
    // по статусу» — аккордеон `<details>`: открыт первый непустой раздел, число — в заголовке
    const problems = watch(page);
    const labels: Record<string, string> = {
      new: 'Непросмотрено',
      returned: 'На доработку',
      accepted: 'В плане',
      declined: 'Отклонена',
      done: 'Сделано',
      deferred: 'Отложена',
      proposed: 'Предложена',
    };
    const counts = await api<Record<string, number>>('GET', `spaces/${IDEAS_GUID}/entries/counts?kind=idea&origin=human`);
    const c = await pageCtx(page);
    await open(page, `#/u/${c.user}/s/${IDEAS_GUID}/j/ideas`);
    await expect(page.getByRole('feed', { name: 'Лента идей' })).toBeVisible();

    const filters = page.getByRole('form', { name: 'Фильтры идей' });
    await expect(filters).toBeHidden();
    const toggle = page.getByRole('button', { name: /^Фильтры/ });
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await minTarget(toggle, 44);
    await toggle.tap();
    await expect(filters).toBeVisible();
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(page.getByLabel('Приоритет идей')).toBeVisible();
    await toggle.tap();
    await expect(filters).toBeHidden();

    // чипы — одна строка с прокруткой внутри полосы
    const chips = page.getByRole('group', { name: 'Статус идей' });
    const boxes = await chips.getByRole('button').evaluateAll((els) => els.map((e) => e.getBoundingClientRect().top));
    expect(boxes.every((t) => Math.abs(t - boxes[0]) < 1), 'чипы — одна строка').toBe(true);
    expect(await chips.evaluate((s) => s.scrollWidth > s.clientWidth), 'полоса чипов прокручивается').toBe(true);
    await minTarget(chips.getByRole('button'), 44);
    await minGap(chips.getByRole('button'), 8);

    await chips.getByRole('button', { name: /^Все/ }).tap();
    await expect(chips.getByRole('button', { name: /^Все/ })).toHaveAttribute('aria-pressed', 'true');
    await page.getByLabel('Группировать по статусу').check();
    const groups = page.locator('details[data-group]');
    const expected = Object.keys(labels).filter((s) => (counts[s] ?? 0) > 0);
    expect(expected.length).toBeGreaterThan(1);
    await expect(groups).toHaveCount(expected.length);
    expect(await groups.evaluateAll((els) => els.map((e) => e.getAttribute('data-group')))).toEqual(expected);
    // число — сервера («k из N», если загружены не все)
    await expect(groups.locator('summary')).toHaveText(expected.map((s) => new RegExp(`^${labels[s]} (\\d+ из )?${counts[s]}$`)));
    expect(await groups.evaluateAll((els) => els.map((e) => (e as HTMLDetailsElement).open))).toEqual(expected.map((_, i) => i === 0));
    const feedOf = (s: string) => page.getByRole('feed', { name: `Идеи раздела «${labels[s]}»` });
    await expect(feedOf(expected[0])).toBeVisible();
    await expect(feedOf(expected[1])).toBeHidden();
    // раскрытие раздела касанием заголовка
    await groups.nth(1).locator('summary').tap();
    await expect(feedOf(expected[1])).toBeVisible();
    await minTarget(groups.locator('summary'), 44);
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-ideas-accordion.png`, fullPage: true });
    expect(problems, problems.join('\n')).toEqual([]);
  });

  test('insights_compact_rows_phone', async ({ page, browser }) => {
    // «Insights» на телефоне (§5.2 п. 3): та же таблица (строки считаются), «Вид», «Область»,
    // «Показы», «Изменено» скрыты, их значения — под заголовком; условие — ширина области журнала
    const problems = watch(page);
    type Row = { id: string; area: string | null; data: { kind?: string }; usage?: { shown: number; useful: number; useless: number } };
    const all = await api<{ items: Row[] }>('GET', `spaces/${SOLUTION_GUID}/entries?kind=insight&limit=100`);
    expect(all.items.length).toBeGreaterThan(2);
    const c = await pageCtx(page);
    const url = `#/u/${c.user}/s/${SOLUTION_GUID}/j/insights`;
    await open(page, url);
    const table = page.getByRole('table', { name: 'Insights' });
    await expect(table.locator('tbody tr')).toHaveCount(all.items.length);
    await expect(table.getByRole('columnheader')).toHaveText(['Надёжность', 'Заголовок', 'Статус']);
    await expect(table.locator('tbody tr').first().getByRole('cell')).toHaveCount(3);
    const x = all.items.find((i) => i.area && i.data.kind) ?? all.items[0];
    const meta = table.locator(`tr[data-entry="${x.id}"]`).getByTestId('row-meta');
    if (x.data.kind) await expect(meta).toContainText(x.data.kind);
    if (x.area) await expect(meta).toContainText(x.area);
    const u = x.usage ?? { shown: 0, useful: 0, useless: 0 };
    await expect(meta).toContainText(`показы ${u.shown} / ${u.useful} / ${u.useless}`);
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.screenshot({ path: `${screens}phone-insights.png`, fullPage: true });
    // открытая запись — карточка вместо таблицы, «← Insights» — таблица прежняя
    await table.locator(`tr[data-entry="${x.id}"]`).getByRole('button').first().tap();
    await expect(page.getByRole('article', { name: 'Карточка insight' })).toBeVisible();
    await expect(table).toBeHidden();
    await page.getByRole('link', { name: '← Insights' }).tap();
    await expect(table.locator('tbody tr')).toHaveCount(all.items.length);
    expect(problems, problems.join('\n')).toEqual([]);

    // 1280 × 720 с открытой карточкой: колонка списка рядом с карточкой уже 720 px, но область
    // журнала шире — таблица полная
    const ctx = await browser.newContext({ storageState: ADMIN_STATE, viewport: { width: 1280, height: 720 }, hasTouch: false, isMobile: false });
    const dp = await ctx.newPage();
    const dProblems = watch(dp);
    await open(dp, `${url}/${x.id}`);
    await expect(dp.locator('.jsplit')).toHaveAttribute('data-layout', 'split');
    await expect(dp.getByRole('article', { name: 'Карточка insight' })).toBeVisible();
    const dt = dp.getByRole('table', { name: 'Insights' });
    expect((await dp.locator('.jsplit-list').boundingBox())!.width, 'колонка списка').toBeLessThan(720);
    await expect(dt.getByRole('columnheader')).toHaveText([
      'Надёжность',
      'Заголовок',
      'Вид',
      'Статус',
      'Область',
      'Показы / полезно / бесполезно',
      'Изменено',
    ]);
    await expect(dt.getByTestId('row-meta')).toHaveCount(0);
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
    await minTarget(sections.getByRole('link'), 44);
    expect(await minGap(sections.getByRole('link'), 8)).toBe(5);
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

  test('tablet_header_720', async ({ page }) => {
    // самый узкий планшет: шапка помещается, разделы при нехватке места прокручиваются внутри
    // своей полосы, страница вбок не прокручивается
    const problems = watch(page);
    await page.setViewportSize({ width: 720, height: 1024 });
    await open(page);
    const top = header(page);
    const sections = top.getByRole('navigation', { name: 'Разделы' });
    await expect(sections).toBeVisible();
    expect(await noHorizontalScroll(page)).toBe(true);
    expect(await top.evaluate((h) => h.scrollWidth <= h.clientWidth), 'шапка без переполнения').toBe(true);
    const nav = (await sections.boundingBox())!;
    const search = (await page.getByRole('button', { name: 'Поиск', exact: true }).boundingBox())!;
    expect(nav.x + nav.width, 'разделы не заходят под «Поиск»').toBeLessThanOrEqual(search.x + 0.5);
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeInViewport({ ratio: 1 });
    // последний раздел достижим (прокруткой полосы разделов, если не поместился)
    const last = sections.getByRole('link', { name: 'Сессии' });
    await last.scrollIntoViewIfNeeded();
    await expect(last).toBeInViewport({ ratio: 1 });
    expect(await noHorizontalScroll(page)).toBe(true);
    await minTarget(sections.getByRole('link'), 44);
    await minGap(sections.getByRole('link'), 8);
    await page.screenshot({ path: `${screens}tablet-720-header.png` });
    expect(problems, problems.join('\n')).toEqual([]);
  });
});

test.describe('планшет в альбомной ориентации, сенсорный ввод', () => {
  test.use({ viewport: VIEWPORTS.tabletLand, ...touch });

  test('card_keeps_focus_on_width_change', async ({ page }) => {
    // поворот 1180 → 820 и обратно с открытой формой правки: раскладка меняется (рядом ↔ вместо
    // списка), фокус и набранный текст остаются в поле
    const problems = watch(page);
    const run = Date.now().toString(36);
    const t = await api<{ id: string }>('POST', 'spaces/knowledge/entries', {
      kind: 'term',
      title: `Поворот ${run}`,
      body: '',
      data: { definition: 'Определение для проверки поворота', aliases: [], code: [] },
    });
    const c = await pageCtx(page);
    await open(page, `#/u/${c.user}/s/knowledge/j/glossary/${t.id}`);
    const split = page.locator('.jsplit');
    await expect(split).toHaveAttribute('data-layout', 'split');
    await page.getByRole('article', { name: 'Карточка термина' }).getByRole('button', { name: 'Править' }).click();
    const def = page.getByRole('form', { name: 'Правка термина' }).getByLabel('Определение');
    await def.click();
    await def.press('End');
    await def.pressSequentially(' — правка');
    await expect(def).toBeFocused();

    await page.setViewportSize({ width: VIEWPORTS.tablet.width, height: VIEWPORTS.tablet.height });
    await expect(split).toHaveAttribute('data-layout', 'stack');
    await expect(page.getByRole('link', { name: '← Словарь' })).toBeVisible();
    await expect(def).toBeFocused();
    await expect(def).toHaveValue('Определение для проверки поворота — правка');

    await page.setViewportSize(VIEWPORTS.tabletLand);
    await expect(split).toHaveAttribute('data-layout', 'split');
    await expect(def).toBeFocused();
    await expect(page.getByRole('list', { name: 'Термины' })).toBeVisible();

    // фокус в списке, а сужение прячет список — фокус на заголовок карточки, не на body
    await page.getByRole('searchbox', { name: 'Поиск по словарю' }).focus();
    await page.setViewportSize({ width: VIEWPORTS.tablet.width, height: VIEWPORTS.tablet.height });
    await expect(split).toHaveAttribute('data-layout', 'stack');
    await expect(
      page.getByRole('article', { name: 'Карточка термина' }).getByRole('heading', { name: `Поворот ${run}` }),
    ).toBeFocused();
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

test('journals_split_no_horizontal_scroll', async ({ browser }) => {
  // Задачи, Идеи, Insights (§5.2 пп. 1–3): без горизонтальной прокрутки на узком телефоне,
  // планшете и ноутбуке (область 700 px — вкладки доски и компактная таблица)
  test.setTimeout(120_000);
  mkdirSync(screens, { recursive: true });
  const pages = PAGES.filter((p) => ['задачи', 'идеи', 'insights'].includes(p.name));
  expect(pages).toHaveLength(3);
  const wide: string[] = [];
  for (const v of ['phoneS', 'tablet', 'laptop'] as ViewportName[]) {
    const ctx = await contextAt(browser, v);
    const page = await ctx.newPage();
    const problems = watch(page);
    const c = await pageCtx(page);
    for (const p of pages) {
      await open(page, p.path(c));
      await expect(page.locator('.jsplit')).toBeVisible();
      await expect(page.getByText('Загрузка…')).toHaveCount(0);
      if (!(await noHorizontalScroll(page))) wide.push(`${v} ${VIEWPORTS[v].width}: ${p.name}`);
      await page.screenshot({ path: `${screens}journal-${v}-${p.name}.png`, fullPage: true });
    }
    expect(problems, `${v}: ${problems.join('\n')}`).toEqual([]);
    await ctx.close();
  }
  expect(wide, 'горизонтальная прокрутка страницы').toEqual([]);
});
