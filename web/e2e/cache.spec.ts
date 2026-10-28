// «Кэш выжимок» (§9.4, §15.4; задача 13 M4): выжимка агента видна в списке и в карточке с
// источниками, после правки источника — `stale` с причиной, фильтр статуса, «Превратить в
// заметку». Выжимку и заметку-источник создаёт агент (токен администратора) через REST.
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

const QUERY = 'Как собрать образ для arm64';

test('Кэш выжимок: выжимка видна, stale после правки источника, превращение в заметку', async ({ page, playwright }) => {
  const problems = watch(page);
  const rc = await playwright.request.newContext({ baseURL: base() });
  const auth = { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` };
  // заметка-источник и выжимка агента в Knowledge администратора
  let r = await rc.post('api/v1/nodes', {
    headers: auth,
    data: { space: 'knowledge', path: 'E2E/Кэш/Сборка', kind: 'note', content: '# Сборка\n\nДля arm64 нужен buildx.\n' },
  });
  expect(r.status(), await r.text()).toBe(201);
  const note = (await r.json()) as { id: string; current_rev: number };
  r = await rc.post('api/v1/cache', {
    headers: auth,
    data: { query: QUERY, summary: 'Сборка multi-stage, для arm64 — buildx.', sources: [{ id: note.id }] },
  });
  expect(r.status(), await r.text()).toBe(201);

  await page.goto(base());
  await page.getByRole('link', { name: 'Кэш выжимок' }).click();
  await expect(page).toHaveURL(/#\/cache$/);
  const row = page.locator(`tr[data-cache="${QUERY}"]`);
  await expect(row).toContainText('valid');
  await expect(row).toContainText('admin');
  await row.getByRole('button', { name: QUERY }).click();
  const card = page.getByRole('region', { name: 'Выжимка' });
  await expect(card).toContainText('Сборка multi-stage, для arm64 — buildx.');
  await expect(card).toContainText(`n:${note.id}`);
  await expect(card).not.toContainText('Выжимка устарела');

  // агент правит источник — выжимка stale с причиной
  r = await rc.put(`api/v1/nodes/${note.id}`, {
    headers: { ...auth, 'If-Match': `"${note.current_rev}"` },
    data: { content: '# Сборка\n\nДля arm64 теперь нужен docker buildx bake.\n' },
  });
  expect(r.status(), await r.text()).toBe(200);
  await page.getByLabel('Статус').selectOption('stale');
  await expect(row).toContainText('stale');
  await row.getByRole('button', { name: QUERY }).click();
  await expect(card).toContainText('Выжимка устарела');
  await expect(card).toContainText('источник изменился');
  await page.getByLabel('Статус').selectOption('valid');
  await expect(row).toHaveCount(0);
  await page.getByLabel('Статус').selectOption('');
  // фильтр автора: выжимка администратора
  await page.getByLabel('Автор').selectOption({ label: 'admin' });
  await expect(row).toBeVisible();
  await page.screenshot({ path: `${screens}cache-stale.png`, fullPage: true });

  // «Превратить в заметку»: путь по умолчанию — Выжимки/<запрос>
  await row.getByRole('button', { name: 'Превратить в заметку' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg.getByLabel('Путь заметки')).toHaveValue(`Выжимки/${QUERY}`);
  await dlg.getByLabel('Путь заметки').fill('E2E/Кэш/Из выжимки');
  await dlg.getByRole('button', { name: 'Создать заметку' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: 'создана' })).toHaveText('Заметка «E2E/Кэш/Из выжимки» создана.');
  await expect(row.getByRole('button', { name: 'Превратить в заметку' })).toBeDisabled();
  const link = card.getByRole('link', { name: /^n:/ });
  await link.click();
  await expect(page).toHaveURL(/\/n\/[^/]+$/);
  await expect(page.getByText('docker buildx bake').or(page.getByText('Сборка multi-stage')).first()).toBeVisible();
  r = await rc.get(`api/v1/nodes/by-path?space=knowledge&path=${encodeURIComponent('E2E/Кэш/Из выжимки')}`, {
    headers: auth,
  });
  expect(r.status(), await r.text()).toBe(200);

  // удаление
  await page.goto(`${base()}#/cache`);
  await row.getByRole('button', { name: 'Удалить' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Удалить' }).click();
  await expect(row).toHaveCount(0);
  await rc.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});
