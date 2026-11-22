// «Сессии» (M6 §8, задача 8b): выданное сессии агента по событию — «событие · вызовов · токенов»
// и доля «уже выдавалось» (повторное чтение `/read` той же ревизии); плашка `rules_heavy` у Rules
// в дереве и на странице «Индекс». Данные создаёт агент (токен администратора) через REST.
// M7.2 (задача 9, §5.8 п. 3): блок «Без сессии» (выдача MCP без сессии агента), субагенты под
// родителем, на телефоне — карточка вместо списка.
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, minTarget, noHorizontalScroll, touch } from './viewports';

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

const SNAP_SESSION = 'e2e-snapshot-17c';

// M6 §8а (задача 17c): блок «Снимок сессии» выводит текст снимка как текст — `<script>` и
// `<img onerror>` из журнала агента не исполняются и не становятся элементами страницы.
test('snapshot text is not html', async ({ page, playwright }) => {
  const problems = watch(page);
  page.on('dialog', (d) => {
    problems.push(`dialog: ${d.message()}`);
    void d.dismiss();
  });
  const rc = await playwright.request.newContext({ baseURL: base() });
  const a = agent(rc);
  let r = await a.post('sessions', { id: SNAP_SESSION, agent: 'claude-code' });
  expect(r.status(), await r.text()).toBe(201);
  const script = '<script>window.__zamXss = 1</script>';
  const img = '<img src=x onerror="window.__zamXss = 2">';
  const headers = { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`, 'X-Zam-Session': SNAP_SESSION };
  r = await rc.put(`api/v1/sessions/${SNAP_SESSION}/snapshot`, {
    headers,
    failOnStatusCode: false,
    data: {
      taken_at: new Date().toISOString(),
      trigger: 'session_end',
      agent: 'claude',
      status: 'complete',
      skipped: 0,
      data: {
        compact_summary: `Current Work: ${script}`,
        prompts: [`почини ${img}`],
        files: ['web/<b>x</b>.ts'],
        commands: [{ cmd: `echo ${img}`, failed: true }],
      },
    },
  });
  expect(r.status(), await r.text()).toBe(204);

  await page.goto(base());
  await page.getByRole('link', { name: 'Сессии' }).click();
  const row = page.locator(`tr[data-session="${SNAP_SESSION}"]`);
  await expect(row).toContainText('снимок');
  await row.getByRole('button', { name: SNAP_SESSION }).click();
  const block = page.getByRole('region', { name: 'Снимок сессии' });
  await expect(block.locator('[data-part="summary"]')).toHaveText(`Current Work: ${script}`);
  await expect(block.locator('[data-part="prompts"]')).toContainText(img);
  await expect(block.locator('[data-part="files"]')).toContainText('web/<b>x</b>.ts');
  await expect(block.locator('[data-part="commands"]')).toContainText(`echo ${img}`);
  await expect(block.locator('[data-part="commands"]')).toContainText('ошибка');
  await expect(block.locator('img, script, b')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __zamXss?: number }).__zamXss)).toBeUndefined();
  await page.screenshot({ path: `${screens}sessions-snapshot.png` });
  // ключи снимка — на странице «Подключение агента → Подсказки хуков»
  await page.getByRole('link', { name: 'Подключение агента' }).click();
  const keys = page.getByRole('region', { name: 'Снимок сессии' });
  for (const k of ['hooks.session_snapshot', 'snapshot_tail_kb', 'snapshot_ms', 'PostCompact', 'SessionEnd']) {
    await expect(keys).toContainText(k);
  }
  await rc.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Сессия `parent` и три её субагента `<parent>:a1..a3` (хук `subagent-start`) с учётом хуков:
 * у родителя 10 токенов, у субагентов 7 + 5 + 3. */
async function seedSubagents(rc: APIRequestContext, parent: string): Promise<{ own: number; all: number }> {
  const a = agent(rc);
  const bodies = [
    { id: parent, agent: 'claude-code', tokens: { pre_tool: 10 } },
    { id: `${parent}:a1`, parent_id: parent, agent_type: 'Explore', tokens: { subagent_local: 7 } },
    { id: `${parent}:a2`, parent_id: parent, agent_type: 'Plan', tokens: { pre_tool: { calls: 2, tokens: 5 } } },
    { id: `${parent}:a3`, parent_id: parent, agent_type: 'Explore', tokens: { post_tool_local: 3 } },
  ];
  for (const b of bodies) {
    const r = await a.post('sessions', b);
    expect(r.status(), await r.text()).toBe(201);
  }
  return { own: 10, all: 25 };
}

type Unattributed = { items: { day: string; event: string; calls: number; tokens: number }[] };

/** Выдача без сессии за `days` суток по событию `event` (REST, как страница). */
async function unattributedOf(rc: APIRequestContext, days: number, event: string) {
  const r = await rc.get(`api/v1/sessions/unattributed?days=${days}`, {
    headers: { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` },
  });
  expect(r.status(), await r.text()).toBe(200);
  const rows = ((await r.json()) as Unattributed).items.filter((x) => x.event === event);
  return { calls: rows.reduce((n, x) => n + x.calls, 0), tokens: rows.reduce((n, x) => n + x.tokens, 0), rows };
}

/** Вызов инструмента MCP без сессии агента: без `X-Zam-Session` и `session_id` — как `.mcp.json`
 * с HTTP вместо `zam mcp` (`crates/zam-server/tests/mcp.rs`, `implicit_session_no_marks_goes_unattributed`). */
async function mcpWithoutSession(rc: APIRequestContext, name: string, args: unknown) {
  const r = await rc.post('mcp', {
    headers: {
      Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`,
      Accept: 'application/json, text/event-stream',
    },
    data: { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } },
    failOnStatusCode: false,
  });
  expect(r.status(), await r.text()).toBe(200);
  expect(((await r.json()) as { error?: unknown }).error, 'ответ MCP').toBeUndefined();
}

const num = (n: number) => n.toLocaleString('ru-RU');

// M7.2 §5.8 п. 3: выдача MCP без сессии видна в блоке «Без сессии» за 7 сут. (по событиям и по
// суткам — раскрытием, числа — как у сервера); сессии субагентов свёрнуты под родителем
// («субагенты: 3»), у родителя — строка «с субагентами»; раскрытие дочитывает `?parent=`.
test('sessions_unattributed_and_subagents_folded', async ({ page, playwright }) => {
  const problems = watch(page);
  const rc = await playwright.request.newContext({ baseURL: base() });
  const parent = 'e2e-sub-parent-9';
  const sums = await seedSubagents(rc, parent);
  // выдача MCP без сессии: учёт пишется в фоне после ответа — ждём, пока он не дойдёт
  const before = await unattributedOf(rc, 7, 'mcp');
  await mcpWithoutSession(rc, 'memory_search', { query: 'выданное без сессии' });
  await expect.poll(async () => (await unattributedOf(rc, 7, 'mcp')).calls, { timeout: 10_000 }).toBe(before.calls + 1);
  const mcp = await unattributedOf(rc, 7, 'mcp');
  expect(mcp.tokens).toBeGreaterThan(before.tokens);
  const mcp30 = await unattributedOf(rc, 30, 'mcp');

  // 30 суток — к ответу сервера строка двадцатидневной давности (в e2e вся выдача без сессии
  // свежая: иначе числа за 7 и 30 суток совпали бы); ответ на 7 суток ждёт сигнала — пока его нет,
  // прежние строки под новой подписью не показываются (ревью задачи 9, Minor 3)
  const old = new Date(Date.now() - 20 * 86_400_000).toISOString().slice(0, 10);
  const isUn = (u: URL, d: string) =>
    u.pathname.endsWith('/api/v1/sessions/unattributed') && u.searchParams.get('days') === d;
  await page.route(
    (u) => isUn(u, '30'),
    async (route) => {
      const r = await route.fetch();
      const j = (await r.json()) as Unattributed;
      j.items.push({ day: old, event: 'mcp', calls: 4, tokens: 100 });
      await route.fulfill({ response: r, json: j });
    },
  );
  let release7: () => void = () => {};
  const gate7 = new Promise<void>((res) => (release7 = res));
  await page.route(
    (u) => isUn(u, '7'),
    async (route) => {
      await gate7;
      await route.continue();
    },
  );

  await page.goto(base());
  await page.getByRole('link', { name: 'Сессии' }).click();
  await expect(page).toHaveURL(/#\/sessions$/);

  // без сессии: по умолчанию 30 суток, затем 7 — у каждого периода свои числа; по суткам —
  // раскрытием
  const block = page.getByRole('region', { name: 'Без сессии' });
  await expect(block).toContainText('без привязки к сессии агента');
  await expect(block.getByRole('link', { name: 'подключения агента' })).toHaveAttribute('href', '#/agent-setup');
  await expect(block).toContainText('zam mcp');
  const table = block.getByRole('table', { name: 'Без сессии по событиям' });
  const ev = table.locator('tr[data-event="mcp"] td');
  await expect(block.getByRole('button', { name: '30 сут.' })).toHaveAttribute('aria-pressed', 'true');
  await expect(ev.nth(1)).toHaveText(num(mcp30.calls + 4));
  await expect(ev.nth(2)).toHaveText(num(mcp30.tokens + 100));
  const d7 = block.getByRole('button', { name: '7 сут.' });
  await d7.click();
  await expect(d7).toHaveAttribute('aria-pressed', 'true');
  await expect(block.getByRole('button', { name: '30 сут.' })).toHaveAttribute('aria-pressed', 'false');
  await expect(block.locator('[data-unattributed-loading]')).toBeVisible();
  await expect(table).toHaveCount(0);
  release7();
  await expect(ev.nth(1)).toHaveText(num(mcp.calls));
  await expect(ev.nth(2)).toHaveText(num(mcp.tokens));
  expect(mcp.calls).not.toBe(mcp30.calls + 4);
  await expect(table.locator('tfoot')).toContainText('Итого за 7 сут.');
  const byDay = block.getByRole('table', { name: 'Без сессии по суткам' });
  await expect(byDay).toBeHidden();
  await block.getByText('По суткам').click();
  const today = new Date().toISOString().slice(0, 10);
  const todayRow = mcp.rows.find((x) => x.day === today) ?? mcp.rows[0];
  await expect(byDay.locator(`tr[data-day="${todayRow.day}"][data-event="mcp"] td`).nth(2)).toHaveText(num(todayRow.calls));

  // субагенты — свёрнуты под родителем, строка «с субагентами» — у родителя
  const list = page.getByRole('table', { name: 'Список сессий' });
  const row = list.locator(`tr[data-session="${parent}"]`);
  await expect(row.locator('td').last()).toHaveText(num(sums.own));
  await expect(list.locator(`tr[data-session^="${parent}:"]`)).toHaveCount(0);
  const group = list.locator(`tr[data-subagents-of="${parent}"]`);
  const toggle = group.getByRole('button', { name: 'субагенты: 3' });
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(group.locator('[data-with-subagents]')).toHaveText(`с субагентами: ${num(sums.all)}`);
  // раскрытие — полный список субагентов с сервера (`?parent=`)
  const asked = page.waitForResponse((r) => r.url().includes(`parent=${encodeURIComponent(parent)}`) && r.ok());
  await toggle.click();
  await asked;
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  const subs = list.locator(`tr[data-session^="${parent}:"]`);
  await expect(subs).toHaveCount(3);
  await expect(subs.first()).toContainText('субагент');
  await expect(list.locator(`tr[data-session="${parent}:a2"] td`).last()).toHaveText('5');
  await page.screenshot({ path: `${screens}sessions-subagents.png`, fullPage: true });

  // карточка субагента — с ссылкой на родителя; у родителя — число субагентов и сумма
  await list.locator(`tr[data-session="${parent}:a1"]`).getByRole('button', { name: `${parent}:a1` }).click();
  await expect(page).toHaveURL(new RegExp(`#/sessions/${encodeURIComponent(`${parent}:a1`)}$`));
  const sub = page.getByRole('region', { name: `Сессия ${parent}:a1` });
  await expect(sub).toContainText('Субагент Explore сессии');
  await sub.getByRole('link', { name: parent }).click();
  const card = page.getByRole('region', { name: `Сессия ${parent}` });
  await expect(card.locator('[data-card-subagents]')).toHaveText(`Субагенты: 3 · с субагентами — ${num(sums.all)} токенов`);

  // сессия из адреса вне окна списка (ревью задачи 9, Minor 4): список без неё и её субагентов —
  // как при 200 более свежих сессиях; карточка — по id, потомки — по `?parent=`
  await page.route(
    (u) => u.pathname.endsWith('/api/v1/sessions') && !u.searchParams.has('parent'),
    async (route) => {
      const r = await route.fetch();
      const j = (await r.json()) as { items: { id: string }[] };
      await route.fulfill({ response: r, json: { items: j.items.filter((x) => !x.id.startsWith(parent)) } });
    },
  );
  await page.reload();
  await expect(card).toBeVisible();
  await expect(list.locator(`tr[data-session="${parent}"]`)).toHaveCount(0);
  await expect(card.locator('[data-card-subagents]')).toHaveText(`Субагенты: 3 · с субагентами — ${num(sums.all)} токенов`);
  await expect(card.getByRole('table', { name: 'Выданное сессии' }).locator('tfoot')).toContainText(num(sums.own));
  // сессии нет у пользователя — сообщение, а не молча список
  const none = 'e2e-no-such-session-9';
  await page.goto(`${base()}#/sessions/${none}`);
  await expect(page.getByRole('region', { name: `Сессия ${none}` }).locator('[data-session-missing]')).toContainText(
    'Такой сессии нет',
  );
  await rc.dispose();
  // ожидаемые 404: сессия, которой нет, и снимок сессии вне окна (у неё `snapshot_at` неизвестен)
  const expected = (p: string) =>
    p === `HTTP 404 GET /api/v1/sessions/${none}` || p === `HTTP 404 GET /api/v1/sessions/${parent}/snapshot`;
  const rest = problems.filter((p) => !expected(p));
  expect(rest, rest.join('\n')).toEqual([]);
});

// §5.1 на телефоне: таблицы — в `.table-wrap` (страница вбок не прокручивается), карточка сессии —
// вместо списка, «← Сессии» и «назад» браузера возвращают список; цели касания — 44 px.
test.describe('телефон', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('sessions_phone_card_replaces_list', async ({ page, playwright }) => {
    const problems = watch(page);
    const rc = await playwright.request.newContext({ baseURL: base() });
    const parent = 'e2e-sub-parent-9-phone';
    await seedSubagents(rc, parent);
    await page.goto(`${base()}#/sessions`);
    const list = page.getByRole('table', { name: 'Список сессий' });
    await expect(list.locator(`tr[data-session="${parent}"]`)).toBeVisible();
    await expect(page.getByRole('table', { name: 'Без сессии по событиям' }).or(page.locator('[data-unattributed-empty]'))).toBeVisible();
    expect(await noHorizontalScroll(page), 'горизонтальная прокрутка списка').toBe(true);
    const group = list.locator(`tr[data-subagents-of="${parent}"]`);
    await minTarget(group.getByRole('button', { name: 'субагенты: 3' }));
    await minTarget(page.getByRole('group', { name: 'Период' }).getByRole('button'));

    await list.locator(`tr[data-session="${parent}"]`).getByRole('button', { name: parent }).click();
    const card = page.getByRole('region', { name: `Сессия ${parent}` });
    await expect(card).toBeVisible();
    await expect(page.locator('.jsplit')).toHaveAttribute('data-layout', 'stack');
    await expect(list).toBeHidden();
    await expect(card.getByRole('heading', { name: `Сессия ${parent}` })).toBeFocused();
    expect(await noHorizontalScroll(page), 'горизонтальная прокрутка карточки').toBe(true);
    await page.screenshot({ path: `${screens}sessions-phone-card.png`, fullPage: true });
    await page.getByRole('link', { name: '← Сессии' }).click();
    await expect(page).toHaveURL(/#\/sessions$/);
    await expect(list).toBeVisible();
    await expect(card).toHaveCount(0);
    // «назад» браузера — тоже к списку
    await list.locator(`tr[data-session="${parent}"]`).getByRole('button', { name: parent }).click();
    await expect(card).toBeVisible();
    await page.goBack();
    await expect(list).toBeVisible();
    await expect(page).toHaveURL(/#\/sessions$/);
    await rc.dispose();
    expect(problems, problems.join('\n')).toEqual([]);
  });
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
