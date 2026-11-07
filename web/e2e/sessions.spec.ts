// «Сессии» (M6 §8, задача 8b): выданное сессии агента по событию — «событие · вызовов · токенов»
// и доля «уже выдавалось» (повторное чтение `/read` той же ревизии); плашка `rules_heavy` у Rules
// в дереве и на странице «Индекс». Данные создаёт агент (токен администратора) через REST.
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
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

/** Запросы агента: токен администратора, `X-Zam-Session` — сессия `session`. */
function agent(rc: APIRequestContext, session?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` };
  if (session) headers['X-Zam-Session'] = session;
  return {
    post: (path: string, data: unknown) => rc.post(`api/v1/${path}`, { headers, data, failOnStatusCode: false }),
    del: (path: string, rev: number) =>
      rc.delete(`api/v1/${path}`, { headers: { ...headers, 'If-Match': String(rev) }, failOnStatusCode: false }),
  };
}

const SESSION = 'e2e-sessions-8b';

test('sessions page shows tokens and reread share', async ({ page, playwright }) => {
  const problems = watch(page);
  const rc = await playwright.request.newContext({ baseURL: base() });
  const a = agent(rc);
  const s = agent(rc, SESSION);
  let r = await a.post('nodes', {
    space: 'knowledge',
    path: 'E2E/Сессии/Учёт',
    kind: 'note',
    content: '# Учёт\n\nВыданное сессии считается по событию.\n',
  });
  expect(r.status(), await r.text()).toBe(201);
  const note = (await r.json()) as { id: string };
  // хук с локальным выводом, поиск и два чтения той же ревизии: второе — «уже выдавалось»
  r = await a.post('sessions', { id: SESSION, agent: 'claude', tokens: { pre_tool: { calls: 2, tokens: 30 } } });
  expect(r.status(), await r.text()).toBe(201);
  r = await s.post('search', { query: 'выданное сессии', mode: 'brief' });
  expect(r.status(), await r.text()).toBe(200);
  for (let i = 0; i < 2; i++) {
    r = await s.post('read', { items: [{ id: note.id }] });
    expect(r.status(), await r.text()).toBe(200);
  }

  await page.goto(base());
  await page.getByRole('link', { name: 'Сессии' }).click();
  await expect(page).toHaveURL(/#\/sessions$/);
  const row = page.locator(`tr[data-session="${SESSION}"]`);
  await expect(row).toContainText('claude');
  await row.getByRole('button', { name: SESSION }).click();
  const card = page.getByRole('region', { name: `Сессия ${SESSION}` });
  const table = card.getByRole('table', { name: 'Выданное сессии' });
  const ev = (e: string) => table.locator(`tr[data-event="${e}"] td`);
  await expect(ev('pre_tool').nth(1)).toHaveText('2');
  await expect(ev('pre_tool').nth(2)).toHaveText('30');
  await expect(ev('search').nth(1)).toHaveText('1');
  await expect(ev('read').nth(1)).toHaveText('2');
  await expect(ev('read').nth(3)).toHaveText('1');
  await expect(card.locator('[data-share]')).toContainText('чтения — 1 из 2 (50 %)');
  // сводка по сессиям — тот же учёт среди прочих сессий
  await expect(page.getByRole('table', { name: 'Выданное по событиям' }).locator('tr[data-event="read"]')).toBeVisible();
  await page.screenshot({ path: `${screens}sessions.png` });
  await rc.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('rules heavy banner', async ({ page, playwright }) => {
  const problems = watch(page);
  const rc = await playwright.request.newContext({ baseURL: base() });
  const a = agent(rc);
  // запись агента в Rules — на время теста (настройка `rules.agent_write`, как в access.spec)
  const toggle = async (on: boolean) => {
    await page.goto(base());
    await page.getByRole('link', { name: 'Администрирование' }).click();
    await page.getByRole('navigation', { name: 'Администрирование' }).getByRole('link', { name: 'Настройки' }).click();
    const box = page.getByRole('checkbox', { name: /Агенты могут писать в Rules/ });
    if (on) await box.check();
    else await box.uncheck();
    await expect(page.getByRole('status')).toHaveText(
      on ? 'Запись в Rules по токенам rw разрешена.' : 'Запись в Rules — только из WebUI.',
    );
  };
  await toggle(true);
  const ids: { id: string; current_rev: number }[] = [];
  try {
    for (let i = 0; i < 11; i++) {
      const r = await a.post('nodes', {
        space: 'rules',
        path: `e2e-heavy/правило-${i}`,
        kind: 'note',
        content: `# Правило ${i}\n\nПравило номер ${i} для проверки плашки.\n`,
      });
      expect(r.status(), await r.text()).toBe(201);
      ids.push((await r.json()) as { id: string; current_rev: number });
    }
    // дерево: у Rules своего пользователя — плашка, в развёрнутом — пояснение
    await page.goto(base());
    const rules = page.getByRole('button', { name: /^Rules/ }).first();
    await expect(rules.locator('.tag.heavy')).toHaveText('много правил');
    await rules.click();
    await expect(page.getByRole('note').filter({ hasText: 'Правил много' })).toContainText('объедините или сократите');
    await page.screenshot({ path: `${screens}rules-heavy-tree.png` });
    // «Индекс»: плашка по базе пользователя
    await page.getByRole('link', { name: 'Индекс' }).click();
    await expect(page.getByRole('note', { name: 'Много правил' })).toContainText('Правил много');
    await page.screenshot({ path: `${screens}rules-heavy-index.png` });
  } finally {
    for (const n of ids) await a.del(`nodes/${n.id}`, n.current_rev);
    await toggle(false);
    await rc.dispose();
  }
  expect(problems, problems.join('\n')).toEqual([]);
});
