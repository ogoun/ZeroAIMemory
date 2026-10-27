// Сквозной сценарий администратора в WebUI по HTTP через LAN-IP (без secure context).
import { expect, test } from '@playwright/test';

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

  // Отзыв
  await roRow.getByRole('button', { name: 'Отозвать' }).click();
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
