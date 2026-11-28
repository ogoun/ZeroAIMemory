// «Администрирование → Внутренние работы» (M7.6, спецификация §9.9, решения 171–175): форма
// «Настройки сервера» — действующее значение и источник у каждого поля, правка сильнее файла,
// «Вернуть к файлу», применение без перезапуска (потолок на странице пользователя), 409 без потери
// набранного; предел токенов — свой (окно на «Внутренних работах», не больше потолка) и
// пользователя (таблица администратора); адрес подключения, запрещённый администратором, — плашка
// у пользователя, подключение на месте. Каждый сценарий возвращает настройки сервера к файлу:
// e2e-сервер общий у всех файлов.
import { expect, test, type Page } from '@playwright/test';
import { pageSnapshots } from './page-snaps';
import { human, resetWorks } from './seed-internal';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });
test.describe.configure({ mode: 'serial' });

// снимки «после» (`E2E_FORMS_SNAP`): страница в трёх ширинах
pageSnapshots('admin-works', () => '#/admin/works');

interface Settings {
  rev: number;
  fields: Record<string, { value: unknown; file: unknown; source: string }>;
}

/** Все значения из WebUI — к файлу; личные пределы — сняты. */
async function resetServer(page: Page): Promise<void> {
  const s = await human<Settings>(page, 'GET', 'admin/works/settings');
  const values = Object.fromEntries(
    Object.entries(s.fields)
      .filter(([, f]) => f.source === 'webui')
      .map(([k]) => [k, null]),
  );
  if (Object.keys(values).length) await human(page, 'PATCH', 'admin/works/settings', { rev: s.rev, values });
  await human(page, 'PATCH', 'auth/settings', { works: { limits: null } });
  await human(page, 'PATCH', `admin/users/${env('E2E_MEMBER_ID')}/settings`, { works: { limits: null } });
}

/** Ошибки JS и неожиданные HTTP-ответы; `allow` — ожидаемые. */
function watch(page: Page, allow: (path: string, status: number) => boolean = () => false): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) || (r.status() === 404 && path.endsWith('/favicon.ico')) || allow(path, r.status());
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

const form = (page: Page) => page.getByRole('form', { name: 'Настройки сервера' });
const meta = (page: Page, key: string) => page.getByTestId(`srv-meta-${key}`);

test.afterEach(async ({ page }) => {
  await resetServer(page);
});

test('works_server_settings_form', async ({ page }) => {
  const problems = watch(page, (path, st) => st === 409 && path.endsWith('/admin/works/settings'));
  await resetServer(page);
  await page.goto(`${base()}#/admin/works`);
  await expect(page.getByRole('heading', { name: 'Настройки сервера', exact: true })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Администрирование' }).getByRole('link', { name: 'Внутренние работы' })).toHaveAttribute(
    'aria-current',
    'page',
  );
  // у каждого поля — действующее значение и источник; пояснение «куда можно отправлять»
  await expect(meta(page, 'max_daily_tokens')).toContainText(/Действует: 2\s000\s000 · файл конфигурации/);
  await expect(form(page)).toContainText('Куда можно отправлять данные памяти модели: куда угодно / только в локальную сеть / никуда');
  await expect(form(page).getByRole('button', { name: /^Вернуть к файлу/ })).toHaveCount(0);

  // правка: потолок и доступ — одной правкой, источник «задано в WebUI», без перезапуска
  await form(page).getByLabel('Потолок токенов в сутки', { exact: true }).fill('150000');
  await form(page).getByLabel('Куда можно отправлять данные памяти', { exact: true }).selectOption('lan');
  await expect(form(page)).toContainText('есть несохранённые изменения');
  await form(page).getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(form(page).getByRole('status')).toContainText('Сохранено: Куда можно отправлять данные памяти, Потолок токенов в сутки');
  await expect(meta(page, 'max_daily_tokens')).toContainText('задано в WebUI');
  await expect(meta(page, 'max_daily_tokens')).toContainText('в файле: 2');
  await expect(meta(page, 'model_access')).toContainText('только в локальную сеть');
  const s = await human<Settings>(page, 'GET', 'admin/works/settings');
  expect(s.fields.max_daily_tokens).toMatchObject({ value: 150000, file: 2000000, source: 'webui' });
  // страница пользователя — новый потолок сразу
  const usage = await human<{ ceiling: { daily: number } }>(page, 'GET', 'works/usage');
  expect(usage.ceiling.daily).toBe(150000);

  // 409: другой администратор успел раньше — значения перечитаны, набранное цело
  await form(page).getByLabel('Записей в одном пакете', { exact: true }).fill('20');
  await human(page, 'PATCH', 'admin/works/settings', { rev: s.rev, values: { window_min: 180 } });
  await form(page).getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(form(page).getByRole('alert')).toContainText('изменил другой администратор');
  await expect(form(page).getByLabel('Записей в одном пакете', { exact: true })).toHaveValue('20');
  await expect(form(page).getByLabel('Окно ночи, минут', { exact: true })).toHaveValue('180');
  await form(page).getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(form(page).getByRole('status')).toContainText('Сохранено: Записей в одном пакете');

  // «Вернуть к файлу» — сразу, прочие набранные поля остаются
  await form(page).getByLabel('Срок одного обращения, секунд', { exact: true }).fill('600');
  await form(page).getByRole('button', { name: 'Вернуть к файлу: Куда можно отправлять данные памяти' }).click();
  await expect(form(page).getByRole('status')).toContainText('снова из файла конфигурации: куда угодно');
  await expect(meta(page, 'model_access')).toContainText('файл конфигурации');
  await expect(form(page).getByLabel('Срок одного обращения, секунд', { exact: true })).toHaveValue('600');
  // диапазон — ошибка у формы, ничего не уходит
  await form(page).getByLabel('Одновременных обращений к модели', { exact: true }).fill('9');
  await form(page).getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(form(page).getByRole('alert')).toContainText('от 1 до 4');
  expect((await human<Settings>(page, 'GET', 'admin/works/settings')).fields.max_parallel.source).toBe('config');

  // телефон — без горизонтальной прокрутки
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('list', { name: 'Пределы токенов пользователей' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  // несохранённое — уход спросит; отпустить, чтобы закончить сценарий
  page.once('dialog', (d) => void d.accept());
  await page.evaluate(() => (location.hash = '#/works'));
  await expect(page.getByRole('heading', { name: 'Внутренние работы', exact: true })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_user_limit', async ({ page }) => {
  const problems = watch(page, (path, st) => st === 422 && path.endsWith('/auth/settings'));
  await resetServer(page);
  // свой предел — окно на «Внутренних работах»
  await page.goto(`${base()}#/works`);
  await expect(page.getByTestId('works-limit')).toContainText('Мой предел: нет (потолок сервера)');
  await page.getByRole('button', { name: 'Изменить предел', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'Предел токенов', exact: true });
  await expect(d).toBeVisible();
  await expect(d.getByLabel('В сутки (UTC), токенов')).toBeFocused();
  // выше потолка — ошибка в окне, поля целы
  await d.getByLabel('В сутки (UTC), токенов').fill('3000000');
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d.getByRole('alert')).toContainText('не больше потолка сервера');
  await expect(d.getByLabel('В сутки (UTC), токенов')).toHaveValue('3000000');
  await d.getByLabel('В сутки (UTC), токенов').fill('400000');
  await d.getByLabel('В месяц (UTC), токенов').fill('900000');
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByTestId('works-limit')).toContainText('Мой предел: 400');
  await expect(page.getByTestId('works-day')).toContainText('из 400');
  const own = await human<{ works: { limits: unknown } }>(page, 'GET', 'auth/settings');
  expect(own.works.limits).toEqual({ daily_tokens: 400000, monthly_tokens: 900000 });

  // пользователь — таблица администратора, «Изменить»
  const member = env('E2E_MEMBER_ID');
  await page.goto(`${base()}#/admin/works`);
  const table = page.getByRole('table', { name: 'Пределы токенов пользователей' });
  const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { user_id: string };
  await expect(table.locator(`tr[data-user="${me.user_id}"]`)).toContainText('400');
  const row = table.locator(`tr[data-user="${member}"]`);
  await expect(row).toContainText('нет (потолок сервера)');
  await row.getByRole('button', { name: /^Изменить предел: / }).click();
  await expect(d).toBeVisible();
  await d.getByLabel('В сутки (UTC), токенов').fill('1000');
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(row).toContainText(/1\s000 в сутки/);
  const m = await human<{ works: { limits: unknown } }>(page, 'GET', `admin/users/${member}/settings`);
  expect(m.works.limits).toEqual({ daily_tokens: 1000, monthly_tokens: 30000000 });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_blocked_by_admin_banner', async ({ page }) => {
  const problems = watch(page);
  await resetServer(page);
  await resetWorks(page);
  // подключение к подставной модели (127.0.0.1: петля разрешена файлом e2e-сервера)
  await human(page, 'PATCH', 'auth/settings', {
    works: { model: { protocol: 'openai', vault: `v:${env('E2E_VAULT_LAN')}`, model: 'e2e-mock', auth: 'key', max_tokens: 4000 }, mode: 'model' },
  });
  await page.goto(`${base()}#/works`);
  await expect(page.getByRole('heading', { name: 'Подключённая модель' })).toBeVisible();
  await expect(page.getByTestId('works-blocked')).toHaveCount(0);
  // администратор снял адреса машины сервера — плашка у пользователя, подключение на месте
  const s = await human<Settings>(page, 'GET', 'admin/works/settings');
  await human(page, 'PATCH', 'admin/works/settings', { rev: s.rev, values: { allow_loopback: false } });
  await page.reload();
  const banner = page.getByTestId('works-blocked');
  await expect(banner).toContainText('Администратор запретил отправку на этот адрес');
  await expect(banner).toContainText('Подключение сохранено');
  await expect(page.getByRole('button', { name: 'Изменить подключение' })).toBeVisible();
  await expect(page.getByTestId('works-effective-probes')).toContainText('администратор запретил отправку');
  // разрешил снова — плашки нет
  await resetServer(page);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Подключённая модель' })).toBeVisible();
  await expect(page.getByTestId('works-blocked')).toHaveCount(0);
  await resetWorks(page);
  expect(problems, problems.join('\n')).toEqual([]);
});
