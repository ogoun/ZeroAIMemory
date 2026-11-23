// Импорт архива пространства из WebUI (M7.2, спецификация M7 §5.7): пункт «Импорт (.zip)…» меню
// пространства — у своих пространств и у администратора в пространства любого пользователя, у
// участника решения его нет; диалог — файл, флаги, «Проверить без записи» и «Импортировать», ход
// задания (опрос раз в секунду) и итог; отказы приёма — понятным текстом по коду ответа; после
// итога без `dry_run` дерево и числа пространства перечитываются. Архив — экспорт решения E2E,
// скачанный сессией администратора в `.tmp/e2e/`; цель — Knowledge нового пользователя.
import { mkdirSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { expect, test, type APIRequestContext, type Page, type Route } from '@playwright/test';
import { loginChangingPassword } from './login';
import { SOLUTION_GUID } from './seed';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, minGap, minTarget, noHorizontalScroll, touch } from './viewports';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
/** Временные файлы прогона — в `.tmp/e2e/` проекта (под .gitignore). */
const TMP = fileURLToPath(new URL('../../.tmp/e2e/', import.meta.url));

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы; `allowed` — коды ответов, подставленных
 * сценарием (`route.fulfill`). */
function watch(page: Page, allowed: number[] = []): string[] {
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
      allowed.includes(r.status());
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

/** Приложение открыто, оболочка готова. */
async function open(page: Page): Promise<void> {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
}

/** REST сессией (cookie контекста, CSRF — из `/auth/me`); `user` — `X-Zam-User` администратора. */
async function sessionApi(rc: APIRequestContext) {
  const me = await rc.get(`${base()}api/v1/auth/me`);
  expect(me.ok(), `auth/me: HTTP ${me.status()}`).toBe(true);
  const { csrf, user_id } = (await me.json()) as { csrf: string; user_id: string };
  const call = async <T = Record<string, unknown>>(method: string, path: string, data?: unknown, user?: string): Promise<T> => {
    const headers: Record<string, string> = { 'X-CSRF-Token': csrf };
    if (user) headers['X-Zam-User'] = user;
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
  return { call, userId: user_id };
}

/** REST по токену (агент или пользователь). */
async function bearer(token: string, method: string, path: string, body?: unknown): Promise<Response> {
  return fetch(`${base()}api/v1/${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

interface Counts {
  journals: Record<string, number>;
  notes: number;
}

interface Job {
  state: 'running' | 'done' | 'failed';
  dry_run: boolean;
  report: { created: number; conflicts: { reason: string }[]; conflicts_total: number } | null;
}

const fmt = (n: number) => new Intl.NumberFormat('ru-RU').format(n);
const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');
/** Узел пользователя в дереве (с его пространствами). */
const userItem = (page: Page, name: string) =>
  tree(page).getByRole('listitem').filter({ has: page.getByRole('button', { name, exact: true }) });

/** Пункты меню пространства `space` внутри `scope` (дерево или узел пользователя). */
async function menuOf(page: Page, scope: ReturnType<typeof tree>, space: string) {
  await scope.getByRole('button', { name: `Действия: ${space}` }).click();
  return page.getByRole('menu').getByRole('menuitem');
}

/** Закрыть меню пространства (Esc — фокус в меню). */
async function closeMenu(page: Page) {
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
}

const IMPORT_ITEM = 'Импорт (.zip)…';

/** Ответ на опрос задания `/imports/*` подставляет сценарий (`route`). */
const isPoll = (u: URL) => /\/api\/v1\/spaces\/[^/]+\/imports\/[^/]+$/.test(u.pathname);
const isImport = (u: URL) => /\/api\/v1\/spaces\/[^/]+\/import$/.test(u.pathname);

test('import_dialog_dry_run_then_import', async ({ page }) => {
  test.setTimeout(180_000);
  const problems = watch(page);
  const { call, userId: adminId } = await sessionApi(page.request);
  const run = Date.now().toString(36);
  const name = `e2e-import-${run}`;
  const u = await call<{ user: { id: string } }>('POST', 'admin/users', { name });
  // термин решения E2E — чтобы у архива был Словарь (Insights решения — из seed.ts)
  const term = `Перенос архива ${run}`;
  const tr = await bearer(env('E2E_ADMIN_TOKEN'), 'POST', `spaces/${SOLUTION_GUID}/entries`, {
    kind: 'term',
    title: term,
    body: '',
    data: { definition: 'Термин для проверки импорта из WebUI', aliases: [], code: [] },
  });
  expect(tr.ok, await tr.text()).toBe(true);
  const src = await call<Counts>('GET', `spaces/${SOLUTION_GUID}/counts`);
  expect(src.journals.insights).toBeGreaterThan(0);
  expect(src.journals.glossary).toBeGreaterThan(0);
  // архив экспорта решения E2E — сессией администратора (с отчётами), в .tmp/e2e/
  const ex = await page.request.get(`${base()}api/v1/spaces/${SOLUTION_GUID}/export?user=${adminId}`);
  expect(ex.status()).toBe(200);
  mkdirSync(TMP, { recursive: true });
  const zip = `${TMP}import-${run}.zip`;
  writeFileSync(zip, await ex.body());
  const target = () => call<Counts>('GET', 'spaces/knowledge/counts', undefined, u.user.id);
  expect((await target()).journals.insights).toBe(0);

  await open(page);
  const item = userItem(page, name);
  await item.getByRole('button', { name: `Развернуть ${name}` }).click();
  await item.getByRole('button', { name: 'Knowledge', exact: true }).click();
  const journals = item.getByRole('list', { name: 'Журналы: Knowledge' });
  await expect(journals.locator('[data-count="insights"]')).toHaveText('0');
  await expect(journals.locator('[data-count="glossary"]')).toHaveText('0');
  await expect(item.getByText('Пусто')).toBeVisible();

  // администратор — в Knowledge другого пользователя
  const items = await menuOf(page, item, 'Knowledge');
  await expect(items).toHaveText(['Новая заметка', 'Новая папка', 'Экспорт (.zip)', IMPORT_ITEM]);
  await page.getByRole('menuitem', { name: IMPORT_ITEM }).click();
  const dlg = page.getByRole('dialog', { name: 'Импорт архива' });
  await expect(dlg).toContainText(`«Knowledge» пользователя ${name}`);
  await expect(dlg).toContainText('Отчёты переносятся только импортом из WebUI.');
  await expect(dlg).toContainText('Закрытие окна не отменяет импорт');
  await dlg.getByLabel('Архив (.zip)').setInputFiles(zip);

  // «Проверить без записи»: ход этапов — первые два опроса подставлены (архив проверяется быстрее
  // секунды), третий и дальше — ответы сервера; подставленный ответ ждёт, пока сценарий увидит
  // предыдущий этап
  let polls = 0;
  let last: Job | null = null;
  let seen1!: () => void;
  let seen2!: () => void;
  const gate1 = new Promise<void>((r) => (seen1 = r));
  const gate2 = new Promise<void>((r) => (seen2 = r));
  await page.route(isPoll, async (route: Route) => {
    const n = ++polls;
    if (n === 2) await gate1;
    if (n >= 3) await gate2;
    const real = await route.fetch();
    const j = (await real.json()) as Job & Record<string, unknown>;
    if (n === 1) {
      return route.fulfill({
        response: real,
        json: { ...j, state: 'running', stage: 'queued', queue: 1, progress: { done: 0, total: 0 }, report: null },
      });
    }
    if (n === 2) {
      return route.fulfill({
        response: real,
        json: { ...j, state: 'running', stage: 'writing', queue: null, progress: { done: 2, total: 5 }, report: null },
      });
    }
    last = j;
    return route.fulfill({ response: real });
  });
  const dryPost = page.waitForRequest((r) => isImport(new URL(r.url())) && r.method() === 'POST');
  await dlg.getByRole('button', { name: 'Проверить без записи' }).click();
  const q = new URL((await dryPost).url()).searchParams;
  expect(Object.fromEntries(q)).toEqual({ own: 'false', on_conflict: 'skip', dry_run: 'true' });
  const stage = dlg.getByTestId('import-stage');
  await expect(stage).toHaveText('Этап: в очереди: перед вами 1 задание');
  seen1();
  await expect(stage).toHaveText('Этап: запись — 2 из 5');
  seen2();
  const report = dlg.getByRole('region', { name: 'Итог импорта' });
  await expect(report.getByRole('heading', { level: 3 })).toHaveText('Проверка без записи', { timeout: 60_000 });
  await expect(report).toContainText('проверка');
  await expect(stage).toHaveCount(0);
  const dry = last as Job | null;
  expect(dry?.state).toBe('done');
  expect(dry?.dry_run).toBe(true);
  expect(dry!.report!.created).toBeGreaterThan(0);
  await expect(report).toContainText(`будет создано: ${fmt(dry!.report!.created)}`);
  // отчёты решения Knowledge не принимает — конфликт с причиной
  expect(dry!.report!.conflicts.some((c) => c.reason === 'report-space')).toBe(true);
  await expect(report).toContainText('отчёты — только в пространство решения');
  // в журнале ничего нет
  const afterDry = await target();
  expect(afterDry.journals.insights).toBe(0);
  expect(afterDry.journals.glossary).toBe(0);
  expect(afterDry.notes).toBe(0);
  await page.unroute(isPoll);

  // «Импортировать»: итог «создано N», числа и дерево перечитаны при открытом диалоге
  const done = page.waitForResponse(
    async (r) => isPoll(new URL(r.url())) && ((await r.json()) as Job).state !== 'running',
  );
  await dlg.getByRole('button', { name: 'Импортировать' }).click();
  const fin = (await (await done).json()) as Job;
  expect(fin.state).toBe('done');
  expect(fin.dry_run).toBe(false);
  expect(fin.report!.created).toBe(dry!.report!.created);
  await expect(report.getByRole('heading', { level: 3 })).toHaveText('Импорт завершён');
  await expect(report).toContainText(`создано: ${fmt(fin.report!.created)}`);
  await expect(report).not.toContainText('будет создано');
  await expect(journals.locator('[data-count="insights"]')).toHaveText(fmt(src.journals.insights));
  await expect(journals.locator('[data-count="glossary"]')).toHaveText(fmt(src.journals.glossary));
  const row = item.locator('.tree-row.space').filter({ has: page.getByRole('button', { name: 'Knowledge', exact: true }) });
  await expect(row.locator('[data-count="notes"]')).toHaveText(fmt(src.notes));
  await expect(item.getByText('Пусто')).toHaveCount(0);

  // записи видны в журналах Knowledge
  await dlg.getByRole('button', { name: 'Закрыть' }).click();
  await expect(dlg).toHaveCount(0);
  await journals.getByRole('link', { name: 'Insights', exact: true }).click();
  await expect(page.getByRole('table', { name: 'Insights' })).toContainText('CRLF ломает хэш содержимого');
  await journals.getByRole('link', { name: 'Словарь', exact: true }).click();
  await expect(page.getByRole('list', { name: 'Термины' })).toContainText(term);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('import_menu_hidden_for_member', async ({ page, browser }) => {
  test.setTimeout(150_000);
  const problems = watch(page);
  const { call } = await sessionApi(page.request);
  const run = Date.now().toString(36);
  const ownerName = `e2e-import-owner-${run}`;
  const memberName = `e2e-import-member-${run}`;
  type Created = { user: { id: string }; token: { plaintext: string } };
  const owner = await call<Created>('POST', 'admin/users', { name: ownerName });
  const member = await call<Created>('POST', 'admin/users', { name: memberName });
  // своё решение владельца, участник `rw` (одобрен администратором)
  const guid = randomUUID();
  const sol = `Импорт E2E ${run}`;
  const reg = await bearer(owner.token.plaintext, 'POST', 'solutions', { agent_solution_id: guid, name: sol });
  expect(reg.ok, await reg.text()).toBe(true);
  const req = await bearer(member.token.plaintext, 'POST', 'solutions', { agent_solution_id: guid, name: sol });
  expect(req.status, await req.text()).toBe(202);
  await call('POST', `solutions/${guid}/access/${member.user.id}/approve`, { access: 'rw' });
  const pass = ['e2e-import-pass-1', 'e2e-import-pass-2'];
  await call('POST', `admin/users/${member.user.id}/password`, { password: pass[0] });

  // участник: у своего Knowledge пункт есть, у чужого решения — нет
  const uc = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  await loginChangingPassword(uc.request, base(), memberName, pass[0], pass[1]);
  const up = await uc.newPage();
  const uproblems = watch(up);
  await open(up);
  const t = tree(up);
  await expect(await menuOf(up, t, 'Knowledge')).toHaveText(['Новая заметка', 'Новая папка', 'Экспорт (.zip)', IMPORT_ITEM]);
  await closeMenu(up);
  await t.getByRole('button', { name: /^Solutions/ }).click();
  await expect(t.getByRole('button', { name: new RegExp(`^${sol}`) })).toContainText('участник rw');
  await expect(await menuOf(up, t, sol)).toHaveText(['Новая заметка', 'Новая папка', 'Экспорт (.zip)']);
  await closeMenu(up);
  await uc.close();
  expect(uproblems, uproblems.join('\n')).toEqual([]);

  // администратор: в пространства любого пользователя — есть (решение владельца — тоже), в
  // решение у участника — нет (сервер ответил бы 403)
  await open(page);
  const m = userItem(page, memberName);
  await m.getByRole('button', { name: `Развернуть ${memberName}` }).click();
  await expect(await menuOf(page, m, 'Knowledge')).toContainText([IMPORT_ITEM]);
  await closeMenu(page);
  await m.getByRole('button', { name: /^Solutions/ }).click();
  await expect(await menuOf(page, m, sol)).toHaveText(['Новая заметка', 'Новая папка', 'Экспорт (.zip)']);
  await closeMenu(page);
  const o = userItem(page, ownerName);
  await o.getByRole('button', { name: `Развернуть ${ownerName}` }).click();
  await o.getByRole('button', { name: /^Solutions/ }).click();
  await expect(await menuOf(page, o, sol)).toHaveText(['Новая заметка', 'Новая папка', 'Экспорт (.zip)', IMPORT_ITEM]);
  await closeMenu(page);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('import_refusal_text_403', async ({ page }) => {
  // отказы приёма подставлены (`route.fulfill`): архив на сервер не уходит
  const problems = watch(page, [403, 408, 413, 503, 507]);
  const { userId } = await sessionApi(page.request);
  mkdirSync(TMP, { recursive: true });
  const zip = `${TMP}import-refusal.zip`;
  const bytes = Buffer.from('PK\u0005\u0006'.padEnd(22, '\u0000'), 'latin1');
  writeFileSync(zip, bytes);
  const cases: { status: number; headers?: Record<string, string>; text: string }[] = [
    { status: 403, text: 'импорт — только владельцу пространства' },
    { status: 503, headers: { 'Retry-After': '30' }, text: 'сервер занят, повторите через 30 с' },
    { status: 413, text: 'архив больше предела сервера (import.max_bytes)' },
    { status: 507, text: 'мало места на сервере' },
    { status: 408, text: 'отправка слишком медленная' },
  ];
  let reply = cases[0];
  const seen: { url: URL; headers: Record<string, string> }[] = [];
  await page.route(isImport, (route) => {
    const r = route.request();
    seen.push({ url: new URL(r.url()), headers: r.headers() });
    return route.fulfill({
      status: reply.status,
      headers: { 'Content-Type': 'application/problem+json', ...reply.headers },
      body: JSON.stringify({ type: 'about:blank', title: 'отказ', status: reply.status, detail: `отказ ${reply.status}` }),
    });
  });
  await open(page);
  await (await menuOf(page, tree(page), 'Knowledge')).filter({ hasText: IMPORT_ITEM }).click();
  const dlg = page.getByRole('dialog', { name: 'Импорт архива' });
  await dlg.getByLabel('Архив (.zip)').setInputFiles(zip);
  const alert = dlg.getByRole('alert');
  for (const [i, c] of cases.entries()) {
    reply = c;
    await dlg.getByRole('button', { name: i % 2 ? 'Импортировать' : 'Проверить без записи' }).click();
    await expect(alert).toContainText(c.text);
    await expect(dlg.getByRole('button', { name: 'Импортировать' })).toBeEnabled();
  }
  // запрос: CSRF, выбор пользователя, размер файла; флаги — из формы (тело-файл перехват не
  // показывает — его принимает настоящий сервер в import_dialog_dry_run_then_import)
  expect(seen).toHaveLength(cases.length);
  const first = seen[0];
  expect(first.headers['content-type']).toBe('application/zip');
  expect(first.headers['x-zam-expected-bytes']).toBe(String(bytes.length));
  expect(first.headers['x-csrf-token']).toBeTruthy();
  expect(first.headers['x-zam-user']).toBe(userId);
  expect(Object.fromEntries(first.url.searchParams)).toEqual({ own: 'false', on_conflict: 'skip', dry_run: 'true' });
  expect(seen[1].url.searchParams.get('dry_run')).toBe('false');
  // «новой ревизией», «оставить»; «Это мой архив» — аксиомы как есть (`keep`), выбор недоступен;
  // «понизить» (по умолчанию) параметра не передаёт: сервер понижает только у чужого архива
  await dlg.getByRole('radio', { name: 'новой ревизией' }).check();
  await dlg.getByRole('radio', { name: 'оставить' }).check();
  await dlg.getByRole('button', { name: 'Импортировать' }).click();
  await expect.poll(() => seen.length).toBe(cases.length + 1);
  expect(Object.fromEntries(seen.at(-1)!.url.searchParams)).toEqual({
    own: 'false',
    on_conflict: 'revision',
    axioms: 'keep',
    dry_run: 'false',
  });
  await dlg.getByRole('checkbox', { name: 'Это мой архив (перенос с другого сервера)' }).check();
  await expect(dlg.getByRole('radio', { name: 'оставить' })).toBeDisabled();
  await dlg.getByRole('button', { name: 'Проверить без записи' }).click();
  await expect.poll(() => seen.length).toBe(cases.length + 2);
  expect(Object.fromEntries(seen.at(-1)!.url.searchParams)).toEqual({
    own: 'true',
    on_conflict: 'revision',
    axioms: 'keep',
    dry_run: 'true',
  });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('import_poll_stops_on_close', async ({ page }) => {
  // задание подставлено: опрос раз в секунду, пока открыт диалог; закрытие опрос останавливает
  const problems = watch(page);
  mkdirSync(TMP, { recursive: true });
  const zip = `${TMP}import-poll.zip`;
  writeFileSync(zip, Buffer.from('PK\u0005\u0006'.padEnd(22, '\u0000'), 'latin1'));
  const job = '01M7POLL000000000000000000';
  await page.route(isImport, (route) =>
    route.fulfill({ status: 202, json: { job, status: `/spaces/knowledge/imports/${job}` } }),
  );
  const at: number[] = [];
  await page.route(isPoll, (route) => {
    at.push(Date.now());
    return route.fulfill({
      json: { job, state: 'running', stage: 'validating', queue: null, dry_run: false, progress: { done: 0, total: 0 }, report: null, error: null },
    });
  });
  await open(page);
  await (await menuOf(page, tree(page), 'Knowledge')).filter({ hasText: IMPORT_ITEM }).click();
  const dlg = page.getByRole('dialog', { name: 'Импорт архива' });
  await dlg.getByLabel('Архив (.zip)').setInputFiles(zip);
  await dlg.getByRole('button', { name: 'Импортировать' }).click();
  await expect(dlg.getByTestId('import-stage')).toHaveText('Этап: проверка архива');
  await expect.poll(() => at.length, { timeout: 10_000 }).toBeGreaterThanOrEqual(3);
  // раз в секунду, не чаще
  for (let i = 1; i < at.length; i++) expect(at[i] - at[i - 1]).toBeGreaterThanOrEqual(900);
  await dlg.getByRole('button', { name: 'Закрыть' }).click();
  await expect(dlg).toHaveCount(0);
  const n = at.length;
  await page.waitForTimeout(2500);
  expect(at.length).toBe(n);
  expect(problems, problems.join('\n')).toEqual([]);
});

test.describe('телефон, сенсорный ввод', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('import_dialog_phone', async ({ page }) => {
    // окно во всю ширину без прокрутки вбок, цели касания — 44 × 44 с зазором 8 px (§5.1 пп. 2, 7)
    const problems = watch(page);
    await open(page);
    await page.getByRole('button', { name: 'Показать дерево' }).click();
    await (await menuOf(page, tree(page), 'Knowledge')).filter({ hasText: IMPORT_ITEM }).click();
    const dlg = page.getByRole('dialog', { name: 'Импорт архива' });
    await expect(dlg).toBeVisible();
    const box = (await dlg.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(-0.5);
    expect(box.x + box.width).toBeLessThanOrEqual(VIEWPORTS.phone.width + 0.5);
    expect(await noHorizontalScroll(page)).toBe(true);
    const actions = dlg.getByRole('button', { name: /^(Проверить без записи|Импортировать|Закрыть)$/ });
    await expect(actions).toHaveCount(3);
    await minTarget(actions, 44);
    expect(await minGap(actions, 8)).toBeGreaterThan(0);
    await minTarget(dlg.locator('label.check'), 44);
    await dlg.getByRole('button', { name: 'Закрыть' }).click();
    await expect(dlg).toHaveCount(0);
    expect(problems, problems.join('\n')).toEqual([]);
  });
});
