// Сквозные сценарии заметок в WebUI: дерево, просмотр документа с «оживлением»,
// контекстное меню, корзина, узкий экран. Данные — из seed.ts (global-setup).
import { expect, test, type Page } from '@playwright/test';
import { ADMIN_STATE } from './session';
import { RUST_CODE } from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

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

/** Открывает приложение с общей сессией (вход — один раз в global-setup). */
async function login(page: Page) {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
}

// По CSS, а не по роли: на узком экране скрытая панель inert и выпадает из дерева доступности.
const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');

test('документ: подсветка, KaTeX, Mermaid, картинка, копирование, вики-ссылки', async ({ page, context }) => {
  const problems = watch(page);
  const media = env('E2E_NOTE_MEDIA');
  const second = env('E2E_NOTE_SECOND');
  await login(page);
  // по умолчанию — свой пользователь
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);

  // Открыть в дереве: Knowledge → E2E → Медиа-заметка
  const t = tree(page);
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await t.getByRole('button', { name: 'E2E', exact: true }).click();
  await t.getByRole('link', { name: 'Медиа-заметка' }).click();
  await expect(page).toHaveURL(new RegExp(`/s/knowledge/n/${media}$`));
  await expect(t.getByRole('link', { name: 'Медиа-заметка' })).toHaveAttribute('aria-current', 'page');

  const doc = page.locator('.doc');
  await expect(doc.getByRole('heading', { level: 1, name: /^Медиа-заметка/ })).toBeVisible();

  // подсветка кода сервером (классы hl-*)
  const rust = doc.locator('pre.hl-code').first();
  await expect(rust).toBeVisible();
  expect(await rust.locator('[class^="hl-"], [class*=" hl-"]').count()).toBeGreaterThan(0);

  // формулы KaTeX: строчная и блочная
  await expect(doc.locator('.katex').first()).toBeVisible();
  await expect(doc.locator('.katex-display')).toHaveCount(1);
  await expect(doc.locator('.math-error')).toHaveCount(0);

  // callout
  await expect(doc.locator('.callout[data-callout="note"]')).toContainText('Это callout');

  // Mermaid → SVG (после DOMPurify), исходника и ошибок не осталось
  await expect(doc.locator('.mermaid-diagram svg')).toBeVisible({ timeout: 20_000 });
  await expect(doc.locator('pre.mermaid-src')).toHaveCount(0);
  await expect(doc.locator('.mermaid-error')).toHaveCount(0);
  await expect(doc.locator('.mermaid-diagram svg')).toContainText('Старт');

  // картинка из вложения загрузилась
  const img = doc.locator('img[alt="Градиент"]');
  await expect(img).toBeVisible();
  await expect
    .poll(() => img.evaluate((i: HTMLImageElement) => (i.complete ? i.naturalWidth : 0)))
    .toBe(240);

  // лайтбокс
  await img.click();
  const lb = page.getByRole('dialog', { name: 'Градиент' });
  await expect(lb).toBeVisible();
  await lb.getByRole('button', { name: 'Увеличить' }).click();
  await page.keyboard.press('Escape');
  await expect(lb).toBeHidden();

  // «Копировать» по HTTP (без Clipboard API) кладёт текст кода в буфер
  expect(await page.evaluate(() => typeof navigator.clipboard)).toBe('undefined');
  const block = doc.locator('.code-block').filter({ has: page.locator('pre.hl-code') }).first();
  await block.hover();
  await block.getByRole('button', { name: 'Копировать' }).click();
  await expect(block.getByRole('button', { name: 'Скопировано' })).toBeVisible();
  const port = new URL(base()).port;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(`http://localhost:${port}/`);
  // буфер Windows хранит переводы строк как CRLF
  const copied = await probe.evaluate(() => navigator.clipboard.readText());
  expect(copied.replace(/\r\n/g, '\n').trimEnd()).toBe(RUST_CODE);
  await probe.close();

  // якорь заголовка
  await expect(doc.locator('h2#h-код a.heading-anchor')).toHaveCount(1);

  // битая вики-ссылка — подсказка, переход не выполняется
  const broken = doc.locator('a.wikilink.broken');
  await expect(broken).toHaveAttribute('title', 'заметка не найдена');

  // вики-ссылка с якорем: переход внутри SPA и прокрутка к разделу
  await doc.locator('a.wikilink:not(.broken)').click();
  await expect(page).toHaveURL(new RegExp(`/s/knowledge/n/${second}#h-`));
  const target = page.locator('.doc').getByRole('heading', { level: 2, name: /^Раздел два/ });
  await expect(target).toBeInViewport();
  await expect(t.getByRole('link', { name: 'Вторая заметка' })).toHaveAttribute('aria-current', 'page');

  // назад — снова медиа-заметка; F5 по прямому адресу раскрывает дерево до узла
  await page.goBack();
  await expect(page).toHaveURL(new RegExp(`/n/${media}$`));
  await page.reload();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Медиа-заметка/ })).toBeVisible();
  await expect(tree(page).getByRole('link', { name: 'Медиа-заметка' })).toHaveAttribute('aria-current', 'page');

  expect(problems, problems.join('\n')).toEqual([]);
});

test('дерево: создание из меню, перенос, удаление в корзину и восстановление', async ({ page }) => {
  const problems = watch(page);
  await login(page);
  const t = tree(page);
  const name = `Проверка ${Date.now()}`;

  // «⋯» у пространства → «Новая заметка» → открывается созданная заметка
  await t.getByRole('button', { name: 'Действия: Knowledge' }).click();
  await page.getByRole('menuitem', { name: 'Новая заметка' }).click();
  const dlg = page.getByRole('dialog');
  await dlg.getByLabel('Имя').fill(name);
  await dlg.getByRole('button', { name: 'Создать' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: new RegExp(`^${name}`) })).toBeVisible();
  const link = t.getByRole('link', { name });
  await expect(link).toHaveAttribute('aria-current', 'page');

  // правый клик → перенос в папку
  await link.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Переименовать/переместить' }).click();
  await dlg.getByLabel('Путь').fill(`E2E/Перенос/${name}`);
  await dlg.getByRole('button', { name: 'Сохранить' }).click();
  await expect(dlg).toBeHidden();
  await expect(t.getByRole('button', { name: 'Перенос', exact: true })).toBeVisible();
  await expect(t.getByRole('link', { name })).toBeVisible();
  await expect(page.locator('.crumbs')).toContainText('Перенос');

  // удаление → открытый документ закрывается, узел в корзине
  await t.getByRole('button', { name: `Действия: ${name}` }).click();
  await page.getByRole('menuitem', { name: 'Удалить' }).click();
  await dlg.getByRole('button', { name: 'Удалить' }).click();
  await expect(dlg).toBeHidden();
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  await expect(t.getByRole('link', { name })).toHaveCount(0);

  await t.getByRole('button', { name: 'Корзина', exact: false }).first().click();
  const item = t.locator('.trash-item').filter({ hasText: name });
  await expect(item).toBeVisible();
  await item.getByRole('button', { name: 'Восстановить' }).click();
  await expect(item).toHaveCount(0);
  await expect(t.getByRole('link', { name })).toBeVisible();

  expect(problems, problems.join('\n')).toEqual([]);
});

test('узкий экран: дерево — выдвижная панель', async ({ page }) => {
  const problems = watch(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page);
  const t = tree(page);
  await expect(t).not.toBeInViewport();
  await page.getByRole('button', { name: 'Показать дерево' }).click();
  await expect(t).toBeInViewport();
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await t.getByRole('button', { name: 'E2E', exact: true }).click();
  await t.getByRole('link', { name: 'Вторая заметка' }).click();
  // выбор заметки закрывает панель
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Вторая заметка/ })).toBeVisible();
  await expect(t).not.toBeInViewport();
  // документ без горизонтальной прокрутки страницы
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

  expect(problems, problems.join('\n')).toEqual([]);
});
