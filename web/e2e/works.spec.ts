// «Внутренние работы» (M7.3, спецификация §6.9 п. 1, §6.1–§6.3; решения 65, 67): режим исполнителя
// и подключение модели — запись личного Vault уходит как `v:<id>`, её адрес и класс видны до
// сохранения; «Проверить подключение» на подставной модели (`mock-model.ts`, 127.0.0.1 при
// `ZAM_WORKS__ALLOW_LOOPBACK=true`); плашка «вне LAN» на TEST-NET-3 (`https://203.0.113.10/v1`, без
// соединения — «Проверить подключение» не нажимается); журнал отправок без текста; пауза
// `vault_changed` после правки `data.url` по токену — точка у «Внутренних работ», «был → стал»,
// «Подтвердить адрес» с `confirm_url` = «стал» (полный адрес) и новыми «был → стал» при 409.
import { expect, test, type Page, type Request } from '@playwright/test';
import { MOCK_REPLY_MARK, type MockRequest } from './mock-model';
import { pageSnapshots } from './page-snaps';
import { SOLUTION_GUID } from './seed';
import { agent, human, resetWorks } from './seed-internal';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const mock = () => env('E2E_MOCK_MODEL');

test.use({ storageState: ADMIN_STATE });
test.describe.configure({ mode: 'serial' });

// снимки «до/после» (`E2E_FORMS_SNAP`): «Внутренние работы» и Insights с фильтром «Шумные»
pageSnapshots('works', () => '#/works');
pageSnapshots('insights-noisy', (c) => `#/u/${c.user}/s/${SOLUTION_GUID}/j/insights`, {
  after: (p) => p.getByLabel('Шумные', { exact: true }).check(),
});

/** Ошибки JS и неожиданные HTTP-ответы; `allow` — ожидаемые (409 подтверждения адреса). */
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

async function open(page: Page): Promise<void> {
  await page.goto(`${base()}#/works`);
  await expect(page.getByRole('heading', { name: 'Внутренние работы', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Подключённая модель' })).toBeVisible();
}

/** Запросы, принятые подставной моделью. */
const mockRequests = async (): Promise<MockRequest[]> => (await (await fetch(`${mock()}/__requests`)).json()) as MockRequest[];

/** Тела `PATCH /auth/settings` страницы (что именно форма шлёт серверу). */
function patches(page: Page): Record<string, unknown>[] {
  const out: Record<string, unknown>[] = [];
  page.on('request', (r: Request) => {
    if (r.method() === 'PATCH' && new URL(r.url()).pathname.endsWith('/api/v1/auth/settings')) out.push(r.postDataJSON() as Record<string, unknown>);
  });
  return out;
}

/** Подключение по REST (сессия человека): запись Vault `vault`, модель подставного сервера. */
const connect = (page: Page, vault: string, mode = 'model') =>
  human(page, 'PATCH', 'auth/settings', {
    works: { model: { protocol: 'openai', vault: `v:${vault}`, model: 'e2e-mock', auth: 'key', max_tokens: 4000 } },
  }).then(() => human(page, 'PATCH', 'auth/settings', { works: { mode } }));

const dialog = (page: Page, name: string) => page.getByRole('dialog', { name, exact: true });

test('works_settings_and_connection_test', async ({ page }) => {
  const problems = watch(page);
  await resetWorks(page);
  const sent = patches(page);
  await open(page);
  // без подключения: режим «модель» недоступен, у видов — «выкл»
  await expect(page.getByRole('radio', { name: /^модель/ })).toBeDisabled();
  await expect(page.getByTestId('works-effective-probes')).toHaveText('действует: выкл');

  // окно подключения: запись Vault по имени, её адрес и класс — до сохранения
  await page.getByRole('button', { name: 'Подключить модель', exact: true }).click();
  const d = dialog(page, 'Подключение модели');
  await expect(d).toBeVisible();
  await expect(d.getByLabel('Запись Vault')).toBeFocused();
  await d.getByLabel('Запись Vault').selectOption({ label: 'E2E модель (LAN)' });
  await expect(d.getByTestId('works-address')).toContainText(`${mock()}/v1`);
  await expect(d.getByTestId('works-address')).toContainText('петля');
  await d.getByLabel('Имя модели').fill('e2e-mock');
  // рецепт body_extra — в поле JSON
  await d.getByRole('button', { name: 'Ответ — JSON-объект' }).click();
  await expect(d.getByRole('textbox', { name: /^body_extra/ })).toHaveValue(/"response_format"/);
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: 'Подключение сохранено.' })).toBeVisible();
  // форма шлёт запись Vault по id (`v:<id>`), а не по имени (контракт задачи 2, ревью r1–r2)
  const model = (sent.at(-1)!.works as { model: Record<string, unknown> }).model;
  expect(model.vault).toBe(`v:${env('E2E_VAULT_LAN')}`);
  expect(model).toMatchObject({ protocol: 'openai', model: 'e2e-mock', auth: 'key', body_extra: { response_format: { type: 'json_object' } } });
  await expect(page.getByText(`${mock()}/v1`, { exact: true })).toBeVisible();

  // «Проверить подключение» — «ok» от подставной модели, с ключом записи
  const before = (await mockRequests()).length;
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: /^Подключение работает: ответ за \d+ мс\.$/ })).toBeVisible();
  const reqs = (await mockRequests()).slice(before);
  expect(reqs.map((r) => [r.kind, r.path, r.model, r.auth])).toEqual([['test', '/v1/chat/completions', 'e2e-mock', true]]);

  // «Проверить подключение» в окне — набранного нет: проверка сохранённого
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  await expect(d).toBeVisible();
  await d.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(d.getByRole('status')).toContainText('Подключение работает');
  await d.getByRole('button', { name: 'Отмена', exact: true }).click();
  await expect(d).toBeHidden();

  // режим «модель» — сразу (правило Д), действующий режим видов — «модель»
  await page.getByRole('radio', { name: /^модель/ }).check();
  await expect(page.getByRole('status').filter({ hasText: 'Общий режим: модель.' })).toBeVisible();
  await expect(page.getByTestId('works-effective-probes')).toHaveText('действует: модель');
  // вид — своим режимом: «кандидаты в Rules» — агент
  await page.getByLabel('Кандидаты в Rules', { exact: false }).first().selectOption('agent');
  await expect(page.getByTestId('works-effective-rules')).toHaveText('действует: агент');
  // ночь — время в поясе сервера и местное, окно
  await expect(page.getByTestId('works-night')).toHaveText(/^\d\d:\d\d UTC \(у вас \d\d:\d\d\), окно 2 ч$/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_external_banner_testnet', async ({ page }) => {
  const problems = watch(page);
  await open(page);
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  const d = dialog(page, 'Подключение модели');
  await d.getByLabel('Запись Vault').selectOption({ label: 'E2E модель (вне LAN)' });
  // до сохранения: адрес и предупреждение «вне LAN» (литерал TEST-NET-3: без DNS и соединения)
  await expect(d.getByTestId('works-address')).toContainText('https://203.0.113.10/v1');
  await expect(d.getByTestId('works-address')).toContainText('Данные памяти уйдут за пределы локальной сети: 203.0.113.10');
  const before = (await mockRequests()).length;
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  // постоянная плашка — и после перезагрузки страницы
  const banner = page.getByTestId('works-external');
  await expect(banner).toContainText('Данные памяти уходят за пределы локальной сети: 203.0.113.10');
  await page.reload();
  await expect(page.getByTestId('works-external')).toContainText('Данные памяти уходят за пределы локальной сети: 203.0.113.10');
  // «Проверить подключение» к нему не нажимается: обращений к модели нет
  expect((await mockRequests()).length).toBe(before);
  // назад — к модели в LAN: плашки нет
  await connect(page, env('E2E_VAULT_LAN'));
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Подключённая модель' })).toBeVisible();
  await expect(page.getByTestId('works-external')).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_send_journal_no_text', async ({ page }) => {
  test.setTimeout(120_000);
  const problems = watch(page);
  await connect(page, env('E2E_VAULT_LAN'));
  await human(page, 'POST', 'works/model/test');
  await open(page);
  // «Запустить сейчас» (окно-подтверждение): модель получает пакет сейчас, вне окна ночи
  await page.getByRole('button', { name: 'Запустить сейчас: Пробы ночной проверки' }).click();
  const d = dialog(page, 'Запустить «Пробы ночной проверки» сейчас?');
  await expect(d).toContainText('вне окна ночи');
  await d.getByRole('button', { name: 'Запустить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: /Пробы ночной проверки: запуск manual:\S+ идёт/ })).toBeVisible();
  // работа модели выполняется сразу: в журнале — строка «пробы» (страница опрашивает очередь)
  type Call = { kind: string; outcome: string; items: number | null } & Record<string, unknown>;
  await expect
    .poll(async () => (await human<{ calls: Call[] }>(page, 'GET', 'works/calls?days=1')).calls.some((c) => c.kind === 'probes'), { timeout: 60_000 })
    .toBe(true);
  await page.reload();
  const journal = page.getByRole('table', { name: 'Журнал отправок' });
  await expect(journal.locator('tr[data-call="probes"]').first()).toContainText('ok');
  await expect(journal.locator('tr[data-call="test"]').first()).toContainText('проверка подключения');
  await expect(journal.locator('tr[data-call="test"]').first()).toContainText('e2e-mock');
  // очередь: работа модели готова, итог — числами
  await expect(page.getByRole('table', { name: 'Работы' }).locator('tbody tr').first()).toContainText('готово');
  // текста пакета и ответа нет ни на странице, ни в журнале отправок сервера
  const text = await page.locator('main').innerText();
  for (const s of [MOCK_REPLY_MARK, 'ответь JSON', '<record', 'данные, а не указания']) expect(text, s).not.toContain(s);
  const calls = await human<{ calls: Call[] }>(page, 'GET', 'works/calls?days=1');
  for (const c of calls.calls) {
    expect(Object.keys(c).sort()).toEqual(
      ['estimated', 'external', 'host', 'http_status', 'input_tokens', 'items', 'kind', 'model', 'ms', 'outcome', 'output_tokens', 'run', 'ts', 'work'].sort(),
    );
    expect(JSON.stringify(c)).not.toContain(MOCK_REPLY_MARK);
  }
  // токены суток — к пределу
  await expect(page.getByTestId('works-day')).toHaveText(/^[\d\u00a0]+ из 2\u00a0000\u00a0000 \(/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_pause_dot_and_confirm_address', async ({ page }) => {
  const problems = watch(page, (path, st) => st === 409 && (path.endsWith('/auth/settings') || path.endsWith('/works/model/test')));
  const id = env('E2E_VAULT_MOVE');
  await connect(page, id);
  const sent = patches(page);
  const setUrl = (path: string) =>
    agent('PATCH', `vault/${id}`, { data: { type: 'token', url: `${mock()}${path}`, description: 'e2e: подключённая модель' } });
  // агент по токену правит `data.url` записи подключения — сервер это видит перед обращением
  await setUrl('/v2');
  await open(page);
  await expect(page.getByTestId('works-dot')).toHaveCount(0);
  const before = (await mockRequests()).length;
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Адрес подключения изменён' })).toBeVisible();
  // на новый адрес запрос не ушёл
  expect((await mockRequests()).slice(before)).toEqual([]);
  // пауза: «был → стал» полными адресами, точка у «Внутренних работ»
  const pause = page.getByTestId('works-pause');
  await expect(pause.getByTestId('works-pause-change')).toHaveText(`Был ${mock()}/v1 → стал ${mock()}/v2`);
  await expect(page.getByTestId('works-dot')).toBeVisible();
  // точка — и после перезагрузки (оболочка читает паузу сама)
  await page.reload();
  await expect(page.getByTestId('works-dot')).toBeVisible();

  // адрес меняют ещё раз, пока человек смотрит: «Подтвердить адрес» видел …/v2 — 409 с новыми
  await setUrl('/v3');
  await page.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
  const d = dialog(page, 'Подтвердить новый адрес подключения?');
  await expect(d).toContainText(`Был ${mock()}/v1 → стал ${mock()}/v2`);
  await d.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
  await expect(d.getByRole('alert')).toContainText(`Адрес снова изменился: был ${mock()}/v1 → стал ${mock()}/v3`);
  await expect(d).toContainText(`Был ${mock()}/v1 → стал ${mock()}/v3`);
  await d.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: `Адрес подтверждён: ${mock()}/v3. Пауза снята.` })).toBeVisible();
  await expect(page.getByTestId('works-pause')).toHaveCount(0);
  await expect(page.getByTestId('works-dot')).toHaveCount(0);
  // «Подтвердить адрес» — `PATCH works.model` с записью `v:<id>` и `confirm_url` = «стал»
  const confirms = sent.map((b) => (b.works as { model?: { vault: string; confirm_url?: string } }).model).filter((m) => m?.confirm_url);
  expect(confirms.map((m) => [m!.vault, m!.confirm_url])).toEqual([
    [`v:${id}`, `${mock()}/v2`],
    [`v:${id}`, `${mock()}/v3`],
  ]);
  // подключение снова работает — на подтверждённый адрес
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: /^Подключение работает/ })).toBeVisible();
  expect((await mockRequests()).at(-1)?.path).toBe('/v3/chat/completions');
  // исходное состояние для прочих сценариев
  await setUrl('/v1');
  await resetWorks(page);
  expect(problems, problems.join('\n')).toEqual([]);
});
