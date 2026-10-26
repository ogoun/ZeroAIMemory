// Раскладка ELK для Mermaid: код elkjs (EPL-2.0) — отдельный ленивый чанк,
// грузится только для диаграмм с `layout: elk`.
import { expect, test } from '@playwright/test';
import { ADMIN_STATE } from './session';

test.use({ storageState: ADMIN_STATE });

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};

/** Запросы JS-ассетов страницы (имена файлов). */
function jsRequests(page: import('@playwright/test').Page): string[] {
  const urls: string[] = [];
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (path.endsWith('.js')) urls.push(path);
  });
  return urls;
}

const isElk = (p: string) => /\/elk-[^/]*\.js$/.test(p);

test('mermaid_elk_layout_renders', async ({ page }) => {
  const requests = jsRequests(page);
  const base = env('E2E_BASE_URL');

  // без `layout: elk` (dagre по умолчанию) чанк ELK не запрашивается
  await page.goto(base);
  await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  const home = page.url();
  await page.goto(`${home}/s/knowledge/n/${env('E2E_NOTE_MEDIA')}`);
  const plain = page.locator('.doc .mermaid-diagram svg');
  await expect(plain).toBeVisible({ timeout: 20_000 });
  await expect(page.locator('.doc .mermaid-error')).toHaveCount(0);
  expect(requests.filter(isElk)).toEqual([]);

  // с `layout: elk` — SVG без ошибки и загрузка чанка ELK
  await page.goto(`${home}/s/knowledge/n/${env('E2E_NOTE_ELK')}`);
  const svg = page.locator('.doc .mermaid-diagram svg');
  await expect(svg).toBeVisible({ timeout: 30_000 });
  await expect(svg).toContainText('Старт ELK');
  await expect(page.locator('.doc .mermaid-error')).toHaveCount(0);
  await expect(page.locator('.doc pre.mermaid-src')).toHaveCount(0);
  expect(requests.filter(isElk).length).toBeGreaterThan(0);
});
