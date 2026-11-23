// Копии сервера и журнал аудита в WebUI (M7.2, спецификация M7 §5.6): «Администрирование → Копии»
// (копия в процессе сервера, ход и итог, список, удаление с подтверждением), плашка
// `no_recent_backup` над страницами администратора и блок «Копии» на «Индексе» (у пользователя —
// нет), «Администрирование → Аудит» (лента по 100, «Показать ещё», фильтры по загруженному).
// Каталог копий сервера E2E — `<data>/backups`: сценарии начинают с пустого (готовые копии прежних
// прогонов удаляются через API) и оставляют его пустым.
import { expect, test, type APIRequestContext, type Browser, type Page } from '@playwright/test';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });

/** Пути под общим лимитом входа (5 в минуту на IP): 429 на них — повод для одного повтора. */
const LIMITED = ['/api/v1/auth/login', '/api/v1/auth/password'];

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

/** Адрес приложения (`#/…`), оболочка готова. */
async function open(page: Page, hash = ''): Promise<void> {
  await page.goto(`${base()}${hash}`);
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
}

/** REST сессией администратора (cookie из `ADMIN_STATE`, CSRF — из `/auth/me`). */
async function adminApi(rc: APIRequestContext) {
  const me = await rc.get(`${base()}api/v1/auth/me`);
  expect(me.ok(), `auth/me: HTTP ${me.status()}`).toBe(true);
  const { csrf } = (await me.json()) as { csrf: string };
  return async <T = Record<string, unknown>>(method: string, path: string, data?: unknown): Promise<T> => {
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
}

type Call = Awaited<ReturnType<typeof adminApi>>;

interface BackupsResp {
  running: { started_at: string; by: string } | null;
  items: { name: string }[];
}

/** Каталог копий пуст: дождаться конца идущей копии и удалить готовые копии. */
async function clearCopies(call: Call): Promise<void> {
  await expect
    .poll(async () => (await call<BackupsResp>('GET', 'admin/backups')).running, { timeout: 120_000 })
    .toBeNull();
  for (const c of (await call<BackupsResp>('GET', 'admin/backups')).items) {
    await call('DELETE', `admin/backups/${encodeURIComponent(c.name)}`);
  }
}

/** Окно лимита входа сервера (`auth/rate.rs`: 5 попыток в минуту на IP, скользящее окно), мс. */
const LOGIN_WINDOW_MS = 60_000;
/** Время последнего запроса спеки под лимитом входа (0 — не было). */
let lastLimited = 0;

/** POST под лимитом входа: при 429 — ждать `Retry-After` и повторить один раз. */
async function limited(rc: APIRequestContext, url: string, data: unknown, headers: Record<string, string> = {}) {
  let r = await rc.post(url, { data, headers });
  if (r.status() === 429) {
    const wait = Number(r.headers()['retry-after'] ?? '60');
    await new Promise((res) => setTimeout(res, wait * 1000 + 500));
    r = await rc.post(url, { data, headers });
  }
  lastLimited = Date.now();
  return r;
}

// Лимит входа общий у всех спек прогона, а входы `code.spec.ts`, `reports.spec.ts` и
// `mobile.spec.ts` повтора при 429 не делают: спека возвращает окно свободным — ждёт, пока её
// входы из него выйдут (в прогоне набора `access, admin, backups, code` без ожидания `code.spec.ts`
// получал 429).
test.afterAll(async () => {
  test.setTimeout(LOGIN_WINDOW_MS + 30_000);
  const rest = lastLimited + LOGIN_WINDOW_MS + 1000 - Date.now();
  if (lastLimited && rest > 0) await new Promise((res) => setTimeout(res, rest));
});

/** Сессия пользователя WebUI (не администратора) после обязательной смены пароля. */
async function userState(browser: Browser) {
  const pass = ['e2e-backup-pass-1', 'e2e-backup-pass-2'];
  const ac = await browser.newContext({ storageState: ADMIN_STATE });
  const call = await adminApi(ac.request);
  const name = `e2e-backup-${Date.now()}`;
  const u = await call<{ user: { id: string } }>('POST', 'admin/users', { name });
  await call('POST', `admin/users/${u.user.id}/password`, { password: pass[0] });
  await ac.close();
  const uc = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const login = await limited(uc.request, `${base()}api/v1/auth/login`, { login: name, password: pass[0] });
  expect(login.status(), 'вход пользователя').toBe(200);
  const { csrf } = (await login.json()) as { csrf: string };
  const pw = await limited(uc.request, `${base()}api/v1/auth/password`, { old: pass[0], new: pass[1] }, {
    'X-CSRF-Token': csrf,
  });
  expect(pw.status(), 'смена пароля').toBe(204);
  const state = await uc.storageState();
  await uc.close();
  return state;
}

const subnav = (page: Page) => page.getByRole('navigation', { name: 'Администрирование' });

test('admin_no_recent_backup_banner', async ({ page, browser }) => {
  // копий нет (§5.6 п. 3): плашка «Копий нет» со ссылкой «Копии» над страницами администратора,
  // блок «Копии» на «Индексе», на самих «Копиях» — текстом страницы; у пользователя — ничего
  const user = await userState(browser);
  const problems = watch(page);
  await clearCopies(await adminApi(page.request));
  await open(page, '#/admin');
  const banner = page.getByTestId('backup-banner');
  await expect(banner).toContainText('Копий нет.');
  await expect(banner.getByRole('link', { name: 'Копии' })).toHaveAttribute('href', '#/admin/backups');
  for (const s of ['Решения', 'Настройки', 'Аудит']) {
    await subnav(page).getByRole('link', { name: s }).click();
    await expect(subnav(page).getByRole('link', { name: s })).toHaveAttribute('aria-current', 'page');
    await expect(page.getByTestId('backup-banner')).toContainText('Копий нет.');
  }
  // «Индекс»: блок «Копии» со ссылкой, плашки над страницей нет
  await page.getByRole('link', { name: 'Индекс', exact: true }).click();
  const block = page.getByTestId('index-backups');
  await expect(block).toContainText('Копий нет (no_recent_backup)');
  await expect(block).toContainText('последняя копия: нет');
  await expect(block).toContainText('автокопии: выключены');
  await expect(page.getByTestId('backup-banner')).toHaveCount(0);
  // ссылка плашки ведёт на «Копии»: предупреждение — текстом страницы, плашки нет
  await open(page, '#/admin');
  await page.getByTestId('backup-banner').getByRole('link', { name: 'Копии' }).click();
  await expect(page).toHaveURL(/#\/admin\/backups$/);
  await expect(page.getByRole('heading', { name: 'Копии', exact: true })).toBeVisible();
  await expect(page.getByText('Копий нет (no_recent_backup)')).toBeVisible();
  await expect(page.getByTestId('backup-banner')).toHaveCount(0);

  // пользователь: ни плашки, ни «Копий»; адрес администратора — его заметки
  const uctx = await browser.newContext({ storageState: user });
  const up = await uctx.newPage();
  const uproblems = watch(up);
  await open(up, '#/admin/backups');
  await expect(up).toHaveURL(/#\/u\/[0-9A-Za-z]+$/);
  await expect(up.getByTestId('backup-banner')).toHaveCount(0);
  await expect(up.getByRole('link', { name: 'Копии' })).toHaveCount(0);
  await open(up, '#/account');
  await expect(up.getByTestId('backup-banner')).toHaveCount(0);
  // API копий пользователю — 403
  const me = (await (await up.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
  expect((await up.request.get(`${base()}api/v1/admin/backups`)).status()).toBe(403);
  expect((await up.request.post(`${base()}api/v1/admin/backups`, { headers: { 'X-CSRF-Token': me.csrf } })).status()).toBe(403);
  await uctx.close();
  expect(uproblems, uproblems.join('\n')).toEqual([]);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('admin_backups_run_list_delete', async ({ page }) => {
  // «Снять копию сейчас» (§5.6 п. 2): подтверждение → «идёт с ЧЧ:ММ» (опрос раз в 3 с) → итог
  // строкой отчёта; копия в списке; «Удалить» с подтверждением — копии нет; оба события — в аудите
  test.setTimeout(240_000);
  const problems = watch(page);
  const call = await adminApi(page.request);
  await clearCopies(call);
  await open(page, '#/admin/backups');
  await expect(page.getByRole('heading', { name: 'Копии', exact: true })).toBeVisible();
  await expect(page.getByTestId('backup-last')).toHaveText('копий нет');
  await expect(page.getByTestId('backup-schedule')).toHaveText('выключено');
  await expect(page.getByText('[backup] schedule = "daily"')).toBeVisible();
  await expect(page.getByText('Готовых копий в каталоге копий нет.')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Восстановление' })).toBeVisible();
  await expect(page.getByText('zam-server restore <каталог копии> --config <конфиг>')).toBeVisible();

  await page.getByRole('button', { name: 'Снять копию сейчас' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg.getByRole('heading', { name: 'Снять копию сейчас' })).toBeVisible();
  // отмена — копия не начинается
  await dlg.getByRole('button', { name: 'Отмена' }).click();
  await expect(dlg).toBeHidden();
  await expect(page.getByTestId('backup-running')).toHaveCount(0);
  await page.getByRole('button', { name: 'Снять копию сейчас' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Снять копию', exact: true }).click();
  await expect(page.getByTestId('backup-running')).toContainText(/Копия идёт с \d\d:\d\d \(из WebUI\)/);
  await expect(page.getByRole('button', { name: 'Снять копию сейчас' })).toBeDisabled();
  const last = page.getByTestId('backup-last-run');
  await expect(last).toContainText(/Готово в \d\d:\d\d: копия \d{4}-\d\d-\d\d_\d{6}/, { timeout: 180_000 });
  await expect(last).toContainText(/баз \d+, блобов \d+ \(связано \d+, скопировано \d+\), [\d,]+ [КМГ]?Б/);
  const name = /копия (\d{4}-\d\d-\d\d_\d{6}(?:-\d+)?)/.exec(await last.innerText())![1];
  await expect(page.getByTestId('backup-running')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Снять копию сейчас' })).toBeEnabled();
  const row = page.locator(`tr[data-backup="${name}"]`);
  await expect(row).toBeVisible();
  await expect(row.getByRole('cell').nth(5)).toHaveText('нет');
  await expect(page.getByTestId('backup-last')).toContainText(/\((только что|\d+ мин назад)\)/);
  await expect(page.getByText(/no_recent_backup/)).toHaveCount(0);

  await row.getByRole('button', { name: `Удалить копию ${name}` }).click();
  const del = page.getByRole('dialog');
  await expect(del).toContainText(name);
  await del.getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(page.getByText(`Копия ${name} удалена.`)).toBeVisible();
  await expect(page.locator(`[data-backup="${name}"]`)).toHaveCount(0);
  await expect(page.getByTestId('backup-last')).toHaveText('копий нет');
  await expect(page.getByText('Копий нет (no_recent_backup)')).toBeVisible();

  const audit = await call<{ items: { action: string; target: string | null; details: Record<string, unknown> }[] }>(
    'GET',
    'admin/audit?limit=20',
  );
  const run = audit.items.find((r) => r.action === 'server.backup' && r.target === name);
  expect(run?.details).toMatchObject({ by: 'webui', ok: true, name });
  expect(audit.items.find((r) => r.action === 'server.backup.delete')?.details).toEqual({ name });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('admin_audit_feed', async ({ page }) => {
  // лента по 100 (§5.6 п. 4): выпуск токена — за первой сотней событий, виден после «Показать
  // ещё»; вход администратора (global-setup и прежние спеки) — на одной из следующих страниц;
  // фильтры по действию и пользователю — по загруженному. Своего входа нет: лимит входа общий
  test.setTimeout(120_000);
  const problems = watch(page);
  const call = await adminApi(page.request);
  const name = `e2e-audit-${Date.now()}`;
  const u = await call<{ user: { id: string } }>('POST', 'admin/users', { name });
  // первый токен пользователя выпускается при создании, второй — здесь (`target` — id токена)
  const issued = await call<{ info: { id: string } }>('POST', `admin/users/${u.user.id}/tokens`, {
    name: 'e2e-audit-token',
    access: 'ro',
  });
  // 110 правок имени (событие `user_update` каждая) — выпуск токена уходит за первую сотню
  for (let i = 0; i < 110; i++) {
    await call('PATCH', `admin/users/${u.user.id}`, { name: i % 2 ? name : `${name}-x` });
  }
  await call('PATCH', `admin/users/${u.user.id}`, { name });

  await open(page, '#/admin/audit');
  await expect(page.getByRole('heading', { name: 'Аудит', exact: true })).toBeVisible();
  const rows = page.locator('tr[data-audit]');
  const shown = page.getByRole('status').filter({ hasText: 'загруженных' });
  await expect(rows).toHaveCount(100);
  await expect(shown).toHaveText('показано 100 из 100 загруженных');
  await expect(page.locator('tr[data-audit="token_issue"]').filter({ hasText: name })).toHaveCount(0);
  await page.getByRole('button', { name: 'Показать ещё' }).click();
  // вторая страница — до 100 событий (сколько их было до сценария, зависит от прогона)
  await expect.poll(() => rows.count()).toBeGreaterThan(100);
  const total = await rows.count();
  expect(total).toBeLessThanOrEqual(200);
  await expect(shown).toHaveText(`показано ${total} из ${total} загруженных`);
  await expect(page.locator('tr[data-audit="token_issue"]').filter({ hasText: name })).toHaveCount(2);
  const token = page.locator('tr[data-audit="token_issue"]').filter({ hasText: issued.info.id });
  await expect(token).toHaveCount(1);
  await expect(token).toContainText(name);
  // вход администратора — «Показать ещё», пока не найдётся (лента новые первыми)
  const more = page.getByRole('button', { name: 'Показать ещё' });
  const logins = page.locator('tr[data-audit="admin_login"]');
  for (let i = 0; i < 50 && !(await logins.count()); i++) {
    await expect(more).toBeEnabled();
    const before = await rows.count();
    await more.click();
    await expect.poll(() => rows.count()).toBeGreaterThan(before);
  }
  await expect(logins.first()).toBeVisible();
  const loaded = await rows.count();
  // детали — свёрнуты, раскрываются
  const details = token.locator('details');
  await expect(details).not.toHaveAttribute('open', '');
  await details.locator('summary').click();
  await expect(details.locator('pre')).toContainText('"token"');

  // фильтр по действию: только выпуски токенов, затем все
  const actionSel = page.getByRole('combobox', { name: /^Действие/ });
  const userSel = page.getByRole('combobox', { name: /^Пользователь/ });
  await actionSel.selectOption('token_issue');
  await expect(rows.first()).toBeVisible();
  const issues = await rows.count();
  expect(issues).toBeGreaterThanOrEqual(2);
  await expect(page.locator('tr[data-audit]:not([data-audit="token_issue"])')).toHaveCount(0);
  await expect(shown).toHaveText(`показано ${issues} из ${loaded} загруженных`);
  // и по пользователю: выпуски токенов нового пользователя — при создании и явный
  await userSel.selectOption({ label: name });
  await expect(rows).toHaveCount(2);
  await actionSel.selectOption('');
  // все события пользователя: создание, выпуски токенов и правки имени
  await expect(page.locator('tr[data-audit]:not([data-audit="user_update"]):not([data-audit="token_issue"]):not([data-audit="user_create"])')).toHaveCount(0);
  await userSel.selectOption('');
  await expect(rows).toHaveCount(loaded);
  expect(problems, problems.join('\n')).toEqual([]);
});
