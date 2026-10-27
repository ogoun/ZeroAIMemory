// Вход пользователя в WebUI (§11.4, §15.1; задача 12 M4): пароль задаёт администратор,
// при первом входе — обязательная смена; у пользователя нет разделов администратора; он
// создаёт insight с пустым certainty (замена записи) и меняет статус без причины (человек);
// свои токены — в «Аккаунте». Вход пользователя — один (лимит 5 в минуту на IP).
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

const FIRST = 'e2e-first-pass-1';
const MINE = 'e2e-my-own-pass-2';

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

/** Администратор по сохранённой cookie-сессии (CSRF — из /auth/me). */
async function adminApi(rc: APIRequestContext) {
  const me = await rc.get(`${base()}api/v1/auth/me`);
  expect(me.ok()).toBe(true);
  const { csrf } = (await me.json()) as { csrf: string };
  return async <T>(method: string, path: string, data?: unknown): Promise<T> => {
    const r = await rc.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
}

test('пользователь: вход, обязательная смена пароля, свои разделы, insight и токены', async ({ page, playwright }) => {
  const problems = watch(page);
  const rc = await playwright.request.newContext({ storageState: ADMIN_STATE });
  const admin = await adminApi(rc);
  const name = `e2e-human-${Date.now()}`;
  const created = await admin<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
  const uid = created.user.id;
  const user = await admin<{ has_password: boolean; must_change_password: boolean }>(
    'POST',
    `admin/users/${uid}/password`,
    { password: FIRST },
  );
  expect(user).toMatchObject({ has_password: true, must_change_password: true });
  // запись агента пользователя: её человек заменит новой без certainty
  const tok = { Authorization: `Bearer ${created.token.plaintext}` };
  const seeded = await rc.post(`${base()}api/v1/spaces/knowledge/entries`, {
    headers: tok,
    data: {
      kind: 'insight',
      title: 'Вывод агента пользователя',
      body: 'Агент заметил закономерность.',
      certainty: 'hypothesis',
      data: { kind: 'discovery' },
      force: true,
    },
  });
  expect(seeded.status()).toBe(201);

  // вход: тот же экран, что у администратора; логин без учёта регистра
  await page.goto(base());
  await page.getByLabel('Логин').fill(name.toUpperCase());
  await page.getByLabel('Пароль').fill(FIRST);
  await page.getByRole('button', { name: 'Войти' }).click();

  // обязательная смена пароля: другие разделы недоступны
  await expect(page.getByRole('heading', { name: 'Смена пароля' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Дерево заметок' })).toHaveCount(0);
  const pw = page.getByRole('form', { name: 'Смена пароля' });
  await pw.getByLabel('Текущий пароль').fill(FIRST);
  await pw.getByLabel('Новый пароль', { exact: true }).fill('short');
  await expect(pw.getByRole('alert')).toHaveText('Новый пароль — не короче 10 символов.');
  await pw.getByLabel('Новый пароль', { exact: true }).fill(MINE);
  await pw.getByLabel('Повтор нового пароля').fill(MINE);
  await page.screenshot({ path: `${screens}user-forced-password.png`, fullPage: true });
  await pw.getByRole('button', { name: 'Сменить пароль' }).click();

  // оболочка пользователя: своя память, без разделов администратора
  const tree = page.getByRole('navigation', { name: 'Дерево заметок' });
  await expect(tree).toBeVisible();
  await expect(tree).toContainText('Моя память');
  await expect(tree).toContainText(name);
  const nav = page.getByRole('navigation', { name: 'Разделы' });
  await expect(nav.getByRole('link')).toHaveText(['Заметки', /Мои решения/, 'Аккаунт']);
  await expect(page.getByRole('link', { name: 'Администрирование' })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Индекс' })).toHaveCount(0);
  // прямой адрес раздела администратора — возврат к своим заметкам
  await page.goto(`${base()}#/admin`);
  await expect(page).toHaveURL(new RegExp(`#/u/${uid}$`));
  await expect(page.getByRole('heading', { name: 'Пользователи' })).toHaveCount(0);

  // Insights своей Knowledge: замена записи новой с пустым certainty
  await page.goto(`${base()}#/u/${uid}/s/knowledge/j/insights`);
  const table = page.getByRole('table', { name: 'Insights' });
  await table.getByRole('button', { name: 'Вывод агента пользователя' }).click();
  const card = page.getByRole('article', { name: 'Карточка insight' });
  await card.getByRole('button', { name: 'Заменить' }).click();
  const form = card.getByRole('form', { name: 'Замена insight' });
  await expect(form.getByLabel('Надёжность новой записи')).toHaveValue('');
  await form.getByLabel('Заголовок').fill('Вывод человека');
  await form.getByRole('button', { name: 'Заменить' }).click();
  await expect(card.getByRole('heading', { name: 'Вывод человека' })).toBeVisible();
  await expect(card.locator('header [data-certainty]')).toHaveCount(0);

  // смена статуса без причины: человеку причина и доказательство необязательны
  await card.getByRole('button', { name: 'Сменить статус надёжности' }).click();
  const cf = card.getByRole('form', { name: 'Статус надёжности' });
  await cf.getByLabel('theorem').check();
  await cf.getByRole('button', { name: 'Сохранить' }).click();
  await expect(card.getByRole('status')).toHaveText('Статус надёжности изменён.');
  await expect(card.locator('header [data-certainty="theorem"]')).toBeVisible();

  // сервер записал человека-автора (сессия пользователя) без причины
  const list = await rc.get(`${base()}api/v1/spaces/knowledge/entries?kind=insight`, { headers: tok });
  const items = ((await list.json()) as { items: { title: string; certainty: string | null; author: { kind: string; user_id: string } }[] }).items;
  const mine = items.find((e) => e.title === 'Вывод человека');
  expect(mine).toMatchObject({ certainty: 'theorem', author: { kind: 'human', user_id: uid } });

  // «Аккаунт»: свой токен — выпуск и отзыв
  await nav.getByRole('link', { name: 'Аккаунт' }).click();
  await expect(page.getByRole('heading', { name: 'Мои токены' })).toBeVisible();
  await page.getByLabel('Имя токена').fill('e2e-laptop');
  await page.getByLabel('Доступ').selectOption('ro');
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  const dialog = page.getByRole('dialog');
  const plain = await dialog.getByLabel('Токен').inputValue();
  expect(plain).toMatch(/^zam_[0-9A-Za-z]{40}$/);
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  const row = page.locator('tr[data-token="e2e-laptop"]');
  await expect(row).toContainText('ro');
  const ok = await rc.get(`${base()}api/v1/spaces`, { headers: { Authorization: `Bearer ${plain}` } });
  expect(ok.status()).toBe(200);
  await row.getByRole('button', { name: 'Отозвать' }).click();
  await expect(row).toContainText('отозван');
  await page.screenshot({ path: `${screens}user-account.png`, fullPage: true });

  // «Мои решения» открываются (решений пока нет)
  await nav.getByRole('link', { name: 'Мои решения' }).click();
  await expect(page.getByRole('heading', { level: 2, name: 'Мои решения' })).toBeVisible();

  await page.getByRole('button', { name: 'Выйти' }).click();
  await expect(page.getByRole('button', { name: 'Войти' })).toBeVisible();
  await rc.dispose();
  expect(problems, problems.join('\n')).toEqual([]);
});
