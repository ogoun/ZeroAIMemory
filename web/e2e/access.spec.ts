// Сквозные сценарии доступа в WebUI (M3): одобрение запроса участника в режиме ro и отзыв
// (действуют на следующем же запросе его токена), метки ролей в дереве, человеческий токен
// (axiom — только человеку), настройка rules.agent_write. Данные — seedJournals.
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import { GUEST_NAME, MEMBER_NAME, SOLUTION_GUID, SOLUTION_NAME } from './seed';

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

const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');

/** Запрос по токену без cookie сессии (отдельный контекст `request` не делит cookie страницы). */
function bearer(request: APIRequestContext, token: string) {
  const headers = { Authorization: `Bearer ${token}` };
  return {
    get: (path: string) => request.get(`${base()}api/v1/${path}`, { headers, failOnStatusCode: false }),
    post: (path: string, data: unknown) =>
      request.post(`${base()}api/v1/${path}`, { headers, data, failOnStatusCode: false }),
    patch: (path: string, data: unknown, extra: Record<string, string> = {}) =>
      request.patch(`${base()}api/v1/${path}`, {
        headers: { ...headers, ...extra },
        data,
        failOnStatusCode: false,
      }),
  };
}

async function openAdmin(page: Page, section: string) {
  await page.goto(base());
  await page.getByRole('link', { name: 'Администрирование' }).click();
  await page.getByRole('navigation', { name: 'Администрирование' }).getByRole('link', { name: section }).click();
  await expect(page.getByRole('heading', { level: 2, name: section })).toBeVisible();
}

test('Решения: одобрение ro, метки в дереве, отзыв', async ({ page, playwright }) => {
  const problems = watch(page);
  // свой контекст запросов без storageState: только токен участника
  const request = await playwright.request.newContext();
  const member = bearer(request, env('E2E_MEMBER_TOKEN'));
  const entries = `spaces/${SOLUTION_GUID}/entries?kind=changelog`;

  // до решения — запрос ждёт
  let r = await member.get(entries);
  expect(r.status()).toBe(403);
  expect((await r.json()).state).toBe('pending');

  await openAdmin(page, 'Решения');
  const row = page.locator(`tr[data-solution="${SOLUTION_GUID}"]`);
  await expect(row).toContainText(SOLUTION_NAME);
  await expect(row).toContainText('admin');
  await expect(row.locator('.badge')).toHaveText('2');
  await row.getByRole('button', { name: 'Доступ' }).click();
  const m = page.locator(`tr[data-member="${MEMBER_NAME}"]`);
  await expect(m).toContainText('ждёт решения');
  await page.screenshot({ path: `${screens}solutions-pending.png`, fullPage: true });
  await m.getByRole('button', { name: 'Одобрить ro' }).click();
  await expect(m).toContainText('одобрен');
  await expect(m.locator('td').nth(2)).toHaveText('ro');

  // ro: чтение — да, запись — 403 (сразу, без кэша)
  r = await member.get(entries);
  expect(r.status()).toBe(200);
  expect((await r.json()).items.length).toBe(3);
  r = await member.post(`spaces/${SOLUTION_GUID}/entries`, {
    kind: 'changelog',
    title: 'Попытка участника ro',
    data: { kind: 'other' },
  });
  expect(r.status()).toBe(403);

  // дерево: у владельца — «владелец» и «общее», у участника — «участник ro · admin»
  await page.goto(base());
  const t = tree(page);
  await t.getByRole('button', { name: /^Solutions/ }).first().click();
  const own = t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).first();
  await expect(own).toContainText('владелец');
  await expect(own).toContainText('общее');
  await t.getByRole('button', { name: MEMBER_NAME, exact: true }).click();
  const solGroups = t.getByRole('button', { name: /^Solutions/ });
  await expect(solGroups).toHaveCount(2);
  await solGroups.nth(1).click();
  const foreign = t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).nth(1);
  await expect(foreign).toContainText('участник ro · admin');
  await page.screenshot({ path: `${screens}tree-roles.png`, fullPage: true });

  // отзыв: следующий же запрос участника — 403 solution-access revoked
  await openAdmin(page, 'Решения');
  await row.getByRole('button', { name: 'Доступ' }).click();
  await m.getByRole('button', { name: 'Отозвать' }).click();
  await expect(m).toContainText('отозван');
  r = await member.get(entries);
  expect(r.status()).toBe(403);
  const p = await r.json();
  expect(p.type).toBe('/problems/solution-access');
  expect(p.state).toBe('revoked');

  // отклонение с причиной: окно требует причину, запрос — rejected
  const g = page.locator(`tr[data-member="${GUEST_NAME}"]`);
  await expect(g).toContainText('ждёт решения');
  await g.getByRole('button', { name: 'Отклонить' }).click();
  const dlg = page.getByRole('dialog');
  await dlg.getByLabel('Причина').fill('решение закрытое');
  await dlg.getByRole('button', { name: 'Отклонить' }).click();
  await expect(dlg).toBeHidden();
  await expect(g).toContainText('отклонён');
  await expect(row.locator('.badge')).toHaveCount(0);
  r = await bearer(request, env('E2E_GUEST_TOKEN')).get(entries);
  expect(r.status()).toBe(403);
  expect((await r.json()).state).toBe('rejected');
  await request.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Токены: человеческих нет, axiom — обычным токеном с причиной', async ({ page, playwright }) => {
  const problems = watch(page);
  await openAdmin(page, 'Пользователи');
  const adminRow = page.getByRole('row').filter({ hasText: '(админ)' }).first();
  await adminRow.getByRole('button', { name: 'Токены' }).click();
  // флажка «для человека» больше нет (§11.4, решение владельца 2026-10-26)
  await expect(page.getByLabel('для человека')).toHaveCount(0);
  await page.getByPlaceholder('Имя токена').fill('e2e-agent');
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  const dialog = page.getByRole('dialog');
  const token = await dialog.getByLabel('Токен').inputValue();
  expect(token).toMatch(/^zam_/);
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  await expect(page.getByRole('row', { name: /^e2e-agent / })).not.toContainText('человек');
  await page.screenshot({ path: `${screens}tokens.png`, fullPage: true });

  const request = await playwright.request.newContext();
  const axiom = {
    kind: 'insight',
    title: 'Аксиома из e2e',
    body: 'По указанию пользователя.',
    data: { kind: 'decision' },
    certainty: 'axiom',
    force: true,
  };
  const agent = bearer(request, token);
  let r = await agent.post('spaces/knowledge/entries', axiom);
  expect(r.status()).toBe(201);
  const e = await r.json();
  expect(e.certainty).toBe('axiom');
  expect(e.author.kind).toBe('agent');
  // смена статуса агентом: без If-Match — 428, без причины — 422, с причиной — 200
  r = await agent.patch(`entries/${e.id}`, { certainty: 'hypothesis' });
  expect(r.status()).toBe(428);
  r = await agent.patch(`entries/${e.id}`, { certainty: 'hypothesis' }, { 'If-Match': '1' });
  expect(r.status()).toBe(422);
  expect((await r.json()).code).toBe('reason-required');
  r = await agent.patch(
    `entries/${e.id}`,
    { certainty: 'hypothesis', reason: 'пользователь отменил' },
    { 'If-Match': '1' },
  );
  expect(r.status()).toBe(200);
  await request.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Настройки: rules.agent_write разрешает и запрещает запись агента в Rules', async ({ page, playwright }) => {
  const problems = watch(page);
  const request = await playwright.request.newContext();
  const agent = bearer(request, env('E2E_ADMIN_TOKEN'));
  const note = (name: string) => ({ space: 'rules', path: `e2e-${name}`, kind: 'note', content: `# ${name}\n` });

  expect((await agent.post('nodes', note('до'))).status()).toBe(403);
  await openAdmin(page, 'Настройки');
  const box = page.getByRole('checkbox', { name: /Агенты могут писать в Rules/ });
  await expect(box).not.toBeChecked();
  await box.check();
  await expect(page.getByRole('status')).toHaveText('Запись в Rules по токенам rw разрешена.');
  await page.screenshot({ path: `${screens}settings.png` });
  expect((await agent.post('nodes', note('после'))).status()).toBe(201);

  // после перезагрузки — сохранено; выключение снова запрещает
  await page.reload();
  await expect(box).toBeChecked();
  await box.uncheck();
  await expect(page.getByRole('status')).toHaveText('Запись в Rules — только из WebUI.');
  expect((await agent.post('nodes', note('снова'))).status()).toBe(403);
  await request.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});
