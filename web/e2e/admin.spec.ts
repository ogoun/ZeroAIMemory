// Сквозной сценарий администратора в WebUI по HTTP через LAN-IP (без secure context).
import { expect, test } from '@playwright/test';
import { pageSnapshots } from './page-snaps';
import { ADMIN_STATE } from './session';

const base = () => {
  const b = process.env.E2E_BASE_URL;
  if (!b) throw new Error('E2E_BASE_URL не задан (global-setup)');
  return b;
};

test('администратор: вход, пользователь, токены, отзыв, F5, выход', async ({ page, context, request }) => {
  const problems: string[] = [];
  // Ошибки JS и CSP — в консоли; сетевые ошибки проверяются по ответам ниже.
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  // Ожидаемые: 401 на auth/me без сессии (восстановление при загрузке), 404 на favicon.ico.
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) ||
      (r.status() === 404 && path.endsWith('/favicon.ico'));
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });

  await page.goto(base());
  // Проверяем именно путь без Clipboard API.
  expect(await page.evaluate(() => window.isSecureContext)).toBe(false);
  expect(await page.evaluate(() => typeof navigator.clipboard)).toBe('undefined');

  // Вход
  await page.getByLabel('Логин').fill('admin');
  await page.getByLabel('Пароль').fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.getByRole('button', { name: 'Войти' }).click();
  // После входа — заметки; пользователи и токены — в разделе «Администрирование».
  await expect(page.getByRole('navigation', { name: 'Дерево заметок' })).toBeVisible();
  await page.getByRole('link', { name: 'Администрирование' }).click();
  await expect(page).toHaveURL(/#\/admin$/);
  await expect(page.getByRole('heading', { name: 'Пользователи' })).toBeVisible();

  // F5 до выхода восстанавливает сессию по cookie
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Пользователи' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();

  // Создание пользователя: в диалоге виден токен
  const user = `e2e-${Date.now()}`;
  await page.getByPlaceholder('Имя нового пользователя').fill(user);
  await page.getByRole('button', { name: 'Создать' }).click();
  // новый пользователь — сразу «Подключение агента» с окном токена (§15.5)
  await expect(page).toHaveURL(/#\/agent-setup$/);
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const field = dialog.getByLabel('Токен');
  const plaintext = await field.inputValue();
  expect(plaintext).toMatch(/^zam_[0-9A-Za-z]{40}$/);

  // «Копировать» по HTTP: запасной путь execCommand('copy'), сообщение внутри диалога
  await dialog.getByRole('button', { name: 'Копировать' }).click();
  await expect(dialog.getByRole('button', { name: 'Скопировано' })).toBeVisible();
  await expect(dialog.getByRole('status')).toHaveText('Токен скопирован в буфер обмена.');
  // Содержимое буфера читаем со страницы в secure context (localhost) того же браузера.
  const port = new URL(base()).port;
  const local = `http://localhost:${port}/`;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(local);
  expect(await probe.evaluate(() => navigator.clipboard.readText())).toBe(plaintext);
  await probe.close();

  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  await expect(dialog).toBeHidden();
  // выбран именно только что созданный пользователь (fix round 1, M-7)
  await expect(page.getByLabel('Пользователь').locator('option:checked')).toHaveText(user);
  await page.getByRole('link', { name: 'Администрирование' }).click();

  // Выпуск ro-токена
  const row = page.getByRole('row').filter({ hasText: user });
  await row.getByRole('button', { name: 'Токены' }).click();
  await page.getByPlaceholder('Имя токена').fill('e2e-ro');
  await page.getByLabel('Доступ').selectOption('ro');
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  await expect(dialog).toBeVisible();
  await expect(page).toHaveURL(/#\/agent-setup$/);
  const ro = await dialog.getByLabel('Токен').inputValue();
  expect(ro).toMatch(/^zam_/);
  expect(ro).not.toBe(plaintext);
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  await page.getByRole('link', { name: 'Администрирование' }).click();
  await row.getByRole('button', { name: 'Токены' }).click();

  // Внутренняя строка таблицы токенов (внешняя строка-контейнер тоже содержит этот текст).
  const roRow = page.getByRole('row', { name: /^e2e-ro / });
  await expect(roRow.getByRole('cell', { name: 'ro', exact: true })).toBeVisible();
  await expect(roRow).toContainText(`zam_${ro.slice(4, 12)}`);

  // Отзыв — с подтверждением (M7.3, §6.14 правило Д: необратимое — окно `danger`)
  await roRow.getByRole('button', { name: 'Отозвать' }).click();
  const revoke = page.getByRole('dialog', { name: 'Отозвать токен?' });
  await revoke.getByRole('button', { name: 'Отозвать', exact: true }).click();
  await expect(revoke).toHaveCount(0);
  await expect(roRow).toContainText('отозван');
  // Отдельный запрос без cookie сессии: отозванный токен отклоняется.
  const api = await request.get(`${base()}api/v1/spaces`, {
    headers: { Authorization: `Bearer ${ro}` },
  });
  expect(api.status()).toBe(401);

  // Выход; F5 после выхода оставляет экран входа
  await page.getByRole('button', { name: 'Выйти' }).click();
  await expect(page.getByRole('button', { name: 'Войти' })).toBeVisible();
  await page.reload();
  await expect(page.getByRole('button', { name: 'Войти' })).toBeVisible();

  expect(problems, problems.join('\n')).toEqual([]);
});

// M7.4 (спецификация §7.7 п. 3, §7.6 п. 1; решение 25 снято): фильтры «Аудита» — запросом к серверу,
// а не по загруженному: действие точно и префиксом, пользователь, период; «Сбросить».
test.describe('аудит', () => {
  test.use({ storageState: ADMIN_STATE });

  // снимки «до/после» (`E2E_FORMS_SNAP`): «Аудит» с полями фильтров сервера
  pageSnapshots('audit', () => '#/admin/audit');

  test('audit_server_filters', async ({ page }) => {
    const problems: string[] = [];
    page.on('pageerror', (e) => problems.push(e.message));
    page.on('response', (r) => {
      if (r.status() >= 400 && new URL(r.url()).pathname.includes('/api/v1/')) problems.push(`HTTP ${r.status()} ${r.url()}`);
    });
    // события своего пользователя: создание, пароль, настройки (сессией администратора)
    const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
    const admin = async <T>(method: string, path: string, data?: unknown): Promise<T> => {
      const r = await page.request.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': me.csrf }, data });
      expect(r.ok(), `${method} ${path}: HTTP ${r.status()}`).toBe(true);
      return (r.status() === 204 ? undefined : await r.json()) as T;
    };
    const name = `e2e-audit-${Date.now().toString(36)}`;
    const u = await admin<{ user: { id: string } }>('POST', 'admin/users', { name });
    await admin('POST', `admin/users/${u.user.id}/password`, { password: ['audit', 'e2e', 'pass', '1'].join('-') });
    await admin('PATCH', `admin/users/${u.user.id}/settings`, { hooks: { prompt: false } });
    // ожидание — ответ сервера с тем же фильтром (страница его только показывает)
    const serverActions = async (q: string) =>
      ((await admin<{ items: { action: string }[] }>('GET', `admin/audit?limit=100&${q}`)).items).map((x) => x.action);
    const shown = () => page.locator('table[aria-label="События аудита"] tbody tr').evaluateAll((rows) => rows.map((r) => r.getAttribute('data-audit')));
    const sent = (part: string) => page.waitForRequest((r) => r.url().includes('/api/v1/admin/audit?') && r.url().includes(part));

    await page.goto(`${base()}#/admin/audit`);
    await expect(page.getByRole('heading', { name: 'Аудит', exact: true })).toBeVisible();
    await expect(page.locator('table[aria-label="События аудита"] tbody tr').first()).toBeVisible();

    // пользователь — фильтр сервера `user=<id>`: только его события
    let req = sent(`user=${u.user.id}`);
    await page.getByLabel('Пользователь').selectOption({ label: name });
    await req;
    const forUser = await serverActions(`user=${u.user.id}`);
    expect(forUser).toEqual(expect.arrayContaining(['user.password.set', 'user.settings.update']));
    await expect.poll(shown).toEqual(forUser);
    await expect(page.getByTestId('audit-count')).toHaveText(`По фильтрам: ${forUser.length}`);

    // действие префиксом (`user.*`): `user.password.set`, `user.settings.update`, но не `user_create`
    req = sent('action=user.*');
    await page.getByLabel('Действие').fill('user.*');
    await page.getByLabel('Действие').press('Enter');
    await req;
    const prefixed = await serverActions(`user=${u.user.id}&action=user.*`);
    expect(prefixed.length).toBeGreaterThanOrEqual(2);
    expect(prefixed.every((a) => a.startsWith('user.'))).toBe(true);
    await expect.poll(shown).toEqual(prefixed);
    // точно — одно действие
    req = sent('action=user.password.set');
    await page.getByLabel('Действие').fill('user.password.set');
    await page.getByRole('button', { name: 'Применить', exact: true }).click();
    await req;
    await expect.poll(shown).toEqual(['user.password.set']);

    // период: «по» — вчера — событий нет; «с» и «по» — сегодня — снова есть
    const day = (d: number) => {
      const x = new Date(Date.now() + d * 86_400_000);
      return `${x.getFullYear()}-${String(x.getMonth() + 1).padStart(2, '0')}-${String(x.getDate()).padStart(2, '0')}`;
    };
    req = sent('until=');
    await page.getByLabel('По', { exact: true }).fill(day(-1));
    await req;
    await expect(page.getByText('Событий по этим фильтрам нет.')).toBeVisible();
    req = sent('since=');
    await page.getByLabel('По', { exact: true }).fill(day(0));
    await page.getByLabel('С', { exact: true }).fill(day(0));
    await req;
    await expect.poll(shown).toEqual(['user.password.set']);

    // «Сбросить» — все события, поля пусты
    req = page.waitForRequest((r) => /\/api\/v1\/admin\/audit\?limit=100$/.test(r.url()));
    await page.getByRole('button', { name: 'Сбросить', exact: true }).click();
    await req;
    await expect(page.getByLabel('Действие')).toHaveValue('');
    await expect(page.getByLabel('Пользователь')).toHaveValue('');
    await expect(page.getByTestId('audit-count')).toContainText('Загружено:');
    expect(problems, problems.join('\n')).toEqual([]);
  });
});
