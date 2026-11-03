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

// Сущности подписей (`#quot;`, `#38;amp#59;`): flowchart при `htmlLabels: false` оставляет их в SVG
// буквально — их расшифровывает hydrate/mermaid.ts; sequence и pie Mermaid расшифровывает сам —
// второй расшифровки нет («&amp;» остаётся «&amp;», а не «&»).
test('mermaid_entities_decoded_once_per_diagram_type', async ({ page }) => {
  const base = env('E2E_BASE_URL');
  const auth = { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` };
  const content = [
    '# Сущности Mermaid',
    '',
    '```mermaid',
    'flowchart LR',
    '  A["say #quot;hi#quot; #38;amp#59;"] --> B["x#58;y"]',
    '```',
    '',
    '```mermaid',
    'sequenceDiagram',
    '  Alice->>Bob: seq #38;amp#59; end',
    '```',
    '',
    '```mermaid',
    'pie title Pie #38;amp#59; T',
    '  "slice #38;amp#59; one" : 1',
    '  "two" : 2',
    '```',
    '',
  ].join('\n');
  const r = await page.request.post(`${base}api/v1/nodes`, {
    headers: auth,
    data: { space: 'knowledge', path: `E2E/Сущности Mermaid ${Date.now()}`, kind: 'note', content },
  });
  expect(r.ok(), await r.text()).toBe(true);
  const node = (await r.json()) as { id: string; current_rev: number };
  try {
    await page.goto(base);
    await expect(page).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
    await page.goto(`${page.url()}/s/knowledge/n/${node.id}`);
    const svgs = page.locator('.doc .mermaid-diagram svg');
    await expect(svgs).toHaveCount(3, { timeout: 30_000 });
    await expect(page.locator('.doc .mermaid-error')).toHaveCount(0);
    const flow = svgs.nth(0);
    // текст подписей без пробелов: Mermaid переносит строки по словам в отдельные <tspan>
    const flat = async (l: typeof flow) => (await l.locator('text').allTextContents()).join('').replace(/\s+/g, '');
    expect(await flow.getAttribute('aria-roledescription')).toBe('flowchart-v2');
    expect(await flat(flow)).toContain('say"hi"&amp;');
    expect(await flat(flow)).toContain('x:y');
    expect(await flat(flow)).not.toContain('&quot;');
    const seq = svgs.nth(1);
    expect(await seq.getAttribute('aria-roledescription')).toBe('sequence');
    expect(await flat(seq)).toContain('seq&amp;end');
    const pie = svgs.nth(2);
    expect(await pie.getAttribute('aria-roledescription')).toBe('pie');
    expect(await flat(pie)).toContain('Pie&amp;T');
    expect(await flat(pie)).toContain('slice&amp;one');
  } finally {
    await page.request.delete(`${base}api/v1/nodes/${node.id}`, {
      headers: { ...auth, 'If-Match': String(node.current_rev) },
    });
  }
});
