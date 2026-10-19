// Сквозные сценарии векторного индекса в WebUI на сервере E2E без моделей и без Qdrant
// (свежая установка до `zam-server models fetch`): страница «Индекс» сообщает, что модели
// не загружены, и подсказывает команду; переиндексация отказывает с понятной причиной;
// поиск Ctrl+K показывает строку деградации и метки каналов; заметки — значок «в очереди».
// Снимки — в .tmp/e2e/screens/ (каталог под .gitignore).
import { expect, test, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы; `allow` — ожидаемые вида `409 POST`. */
function watch(page: Page, allow: string[] = []): string[] {
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
      allow.includes(`${r.status()} ${r.request().method()}`);
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

async function login(page: Page): Promise<string> {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  return /#\/u\/([0-9A-Za-z]+)$/.exec(page.url())![1];
}

test('индекс: модели не загружены, подсказка команды, обновление, отказ reindex', async ({ page }) => {
  // reindex без запущенного воркера — 409 с причиной (ожидаемо на сервере без моделей)
  const problems = watch(page, ['409 POST']);
  await login(page);

  await page.getByRole('link', { name: 'Индекс', exact: true }).click();
  await expect(page).toHaveURL(/#\/admin\/index$/);
  await expect(page.getByRole('link', { name: 'Индекс', exact: true })).toHaveAttribute('aria-current', 'page');
  await expect(page.getByRole('heading', { name: 'Векторный индекс' })).toBeVisible();

  // причина деградации — человеческим текстом, с командой загрузки моделей
  const alert = page.locator('.alert').first();
  await expect(alert).toContainText('Векторный поиск недоступен: модели не загружены');
  await expect(alert).toContainText('Поиск работает в режиме полнотекстового (FTS)');
  await expect(alert.locator('code.cmd')).toHaveText('zam-server models fetch');
  await expect(page.locator('dl.grid')).toContainText('не загружена');
  // повреждённых и исключённых баз на свежем сервере нет
  await expect(page.getByText('Базы пользователей не открываются')).toHaveCount(0);
  await expect(page.getByText('Исключены из пересборки индекса')).toHaveCount(0);
  await page.screenshot({ path: `${screens}index-status.png`, fullPage: true });

  // статус обновляется сам (раз в 5 с), пока страница открыта
  await page.waitForResponse((r) => r.url().endsWith('/api/v1/index/status') && r.ok(), { timeout: 8000 });

  // переиндексация — с подтверждением; без воркера сервер отказывает, текст причины виден
  await page.getByRole('button', { name: 'Переиндексировать из embeddings' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg).toContainText('из кэша эмбеддингов');
  await dlg.getByRole('button', { name: 'Отмена' }).click();
  await expect(dlg).toBeHidden();
  await page.getByRole('button', { name: 'Полная переиндексация' }).click();
  await expect(dlg).toContainText('перечанкованы');
  await dlg.getByRole('button', { name: 'Запустить' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.locator('p.error[role="alert"]')).toContainText('векторный индекс не запущен');

  // прямой адрес после F5 открывает ту же страницу
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Векторный индекс' })).toBeVisible();

  // узкий экран: страница и шапка без горизонтальной прокрутки
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator('.alert').first()).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: `${screens}index-status-narrow.png`, fullPage: true });

  expect(problems, problems.join('\n')).toEqual([]);
});

test('поиск: строка деградации, метки каналов, значки индексации', async ({ page }) => {
  const problems = watch(page);
  const uid = await login(page);

  // Ctrl+K: выдача только полнотекстовая — строка деградации над результатами и метка fts
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await expect(search).toBeFocused();
  await search.fill('прокрутки');
  const option = page.getByRole('option').filter({ hasText: 'Вторая заметка' }).first();
  await expect(option).toBeVisible();
  const line = page.locator('.notices .degraded');
  await expect(line).toContainText('Векторный поиск недоступен: модели не загружены');
  await expect(line).toContainText('показаны результаты полнотекстового поиска');
  await expect(option.locator('.src', { hasText: 'fts' })).toBeVisible();
  await expect(option.locator('.src', { hasText: 'vector' })).toHaveCount(0);
  await page.screenshot({ path: `${screens}search-degraded.png` });
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toBeHidden();

  // заметки без векторной индексации — значок «в очереди» в дереве и в документе
  const second = env('E2E_NOTE_SECOND');
  await page.goto(`${base()}#/u/${uid}/s/knowledge/n/${second}`);
  await expect(page.locator('.doc').getByRole('heading', { level: 1, name: /^Вторая заметка/ })).toBeVisible();
  await expect(page.locator('.doc-head .meta')).toContainText('в очереди на индексацию');
  await expect(page.locator('.doc-head .meta').getByRole('img', { name: 'Индексация: в очереди' })).toBeVisible();
  const row = page.locator('nav[aria-label="Дерево заметок"] li').filter({
    has: page.getByRole('link', { name: 'Вторая заметка' }),
  });
  await expect(row.getByRole('img', { name: 'Индексация: в очереди' }).first()).toBeVisible();

  expect(problems, problems.join('\n')).toEqual([]);
});
