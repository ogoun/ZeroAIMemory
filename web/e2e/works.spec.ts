// «Внутренние работы» (M7.3, спецификация §6.9 п. 1, §6.1–§6.3; решения 65, 67): режим исполнителя
// и подключение модели — запись личного Vault уходит как `v:<id>`, её адрес и класс видны до
// сохранения; «Проверить подключение» на подставной модели (`mock-model.ts`, 127.0.0.1 при
// `ZAM_WORKS__ALLOW_LOOPBACK=true`); плашка «вне LAN» на TEST-NET-3 (`https://203.0.113.10/v1`, без
// соединения — «Проверить подключение» не нажимается); журнал отправок без текста; пауза
// `vault_changed` после правки `data.url` по токену — точка у «Внутренних работ», «был → стал»,
// «Подтвердить адрес» с `confirm_url` = «стал» (полный адрес) и новыми «был → стал» при 409.
// Плашка «вне LAN» — только по полю сервера `works.model.external` (ревью r1 задачи 8, I1): и после
// перезагрузки, и у имени (ответ подставлен); текст — по действующему режиму, «Отключить модель».
// Каждый сценарий готовит своё подключение сам (ревью r1, Minor 11).
// M7.4 (§7.7 п. 2): `works_admin_block` — сводка `GET /admin/works` у администратора (таблица, на
// телефоне — карточки), имя пользователя — его вид страницы (`#/works?user=<id>`): запросы — его.
// M7.6 (§9.2 п. 6; решения 166–170): `works_vault_choices_new_record_fix_url` — в выборе только
// записи с адресом API («<имя> — <адрес>»), запись из окна создаётся и выбирается, адрес без /v1 —
// предупреждение, «Исправить адрес» правит запись без смены ключа (проверка проходит);
// `works_test_errors_401_404` — понятные причины и адрес запроса; `works_protocol_anthropic` —
// выбор протокола, «Поле предела» скрыто, проверка подключения на заглушке `/v1/messages`, телефон.
import { expect, test, type Page, type Request } from '@playwright/test';
import { MOCK_MODELS, MOCK_REPLY_MARK, type MockRequest } from './mock-model';
import { pageSnapshots } from './page-snaps';
import { SOLUTION_GUID } from './seed';
import { MOCK_KEY, agent, human, resetWorks } from './seed-internal';
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

// снимки «до/после» (`E2E_FORMS_SNAP`): «Внутренние работы» (без подключения и с подключением вне
// LAN на паузе «адрес изменён» — пауза подставлена в ответ настроек) и Insights с фильтром «Шумные»
pageSnapshots('works', () => '#/works', { prepare: (p) => resetWorks(p) });
pageSnapshots('works-external', () => '#/works', {
  // настоящая пауза (ревью r2, N1): подключение в LAN, адрес записи сменён на внешний, проверка
  // подключения ставит паузу `vault_changed` (409 — ожидаемо)
  prepare: async (p) => {
    await setMoveUrl(`${mock()}/v1`);
    await resetWorks(p);
    await connect(p, env('E2E_VAULT_MOVE'));
    await setMoveUrl(EXT_URL);
    await human(p, 'POST', 'works/model/test').catch(() => undefined);
  },
});
// M7.4 (§7.7 п. 2): страница пользователя глазами администратора (блок «Все пользователи» — и на
// снимке `works`)
pageSnapshots('works-user', () => `#/works?user=${env('E2E_MEMBER_ID')}`);
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

/** Внешний адрес (TEST-NET-3): классификация без DNS и соединения. */
const EXT_URL = 'https://203.0.113.10/v1';

/** Адрес записи Vault «смена адреса» — правкой агента по токену (как смена `data.url` вне WebUI). */
const setMoveUrl = (url: string) =>
  agent('PATCH', `vault/${env('E2E_VAULT_MOVE')}`, { data: { type: 'token', url, description: 'e2e: подключённая модель' } });

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
  await d.getByLabel('Запись Vault').selectOption({ label: `E2E модель (LAN) — ${mock()}/v1` });
  await expect(d.getByTestId('works-address')).toContainText(`${mock()}/v1`);
  await expect(d.getByTestId('works-address')).toContainText('эта же машина');
  // без сохранённого подключения проверять нечего — кнопка «Сохранить и проверить» (ревью r1, Minor 1)
  await expect(d.getByRole('button', { name: 'Сохранить и проверить', exact: true })).toBeVisible();
  await expect(d.getByRole('button', { name: 'Проверить подключение', exact: true })).toHaveCount(0);
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
  // сервер e2e — с `[works] check = "off"` (global-setup): выключатель неактивен с причиной, а не
  // обещает проверку, которой не будет (финальное ревью M7.3, Minor 12)
  const night = page.getByRole('region', { name: 'Ночь' });
  await expect(night.getByLabel('Ночная проверка моей памяти')).toBeDisabled();
  await expect(night.getByTestId('works-check-off')).toContainText('На сервере ночная проверка выключена администратором');
  // сервер проводит проверку (ответ `usage` подставлен) — выключатель действует; его итог — у него
  // же, в разделе «Ночь» (ревью r1, Minor 8)
  await page.route(/\/api\/v1\/works\/usage$/, async (r) => {
    const resp = await r.fetch();
    const j = (await resp.json()) as { check: Record<string, unknown> };
    j.check = { ...j.check, server: 'nightly', reason: null };
    return r.fulfill({ response: resp, json: j });
  });
  await page.reload();
  await expect(night.getByTestId('works-check-off')).toHaveCount(0);
  await night.getByLabel('Ночная проверка моей памяти').uncheck();
  await expect(night.getByRole('status')).toContainText('Ночная проверка вашей памяти выключена');
  await night.getByLabel('Ночная проверка моей памяти').check();
  await expect(night.getByRole('status')).toContainText('Ночная проверка вашей памяти включена');
  await page.unroute(/\/api\/v1\/works\/usage$/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_journal_no_duration_dash', async ({ page }) => {
  // финальное ревью M7.3, Minor 2: строки журнала без длительности (`aborted`, `internal`,
  // `limit`, `unavailable` без соединения — `ms: null`) — «—» с подсказкой, а не «0 мс»; строки
  // подставлены (оборванный вызов на сервере e2e не подготовить без остановки сервера)
  const problems = watch(page);
  const row = (outcome: string, ms: number | null) => ({
    ts: new Date().toISOString(),
    work: null,
    kind: 'probes',
    host: '127.0.0.1:9',
    model: 'e2e-mock',
    input_tokens: 10,
    output_tokens: 0,
    estimated: true,
    ms,
    outcome,
    http_status: null,
    external: false,
    items: 1,
    run: 'manual:e2e',
  });
  await page.route(/\/api\/v1\/works\/calls(\?.*)?$/, (r) =>
    r.fulfill({ json: { days: 7, calls: [row('aborted', null), row('internal', null), row('ok', 85)] } }),
  );
  await open(page);
  const journal = page.getByRole('table', { name: 'Журнал отправок' });
  const ms = journal.getByTestId('call-ms');
  await expect(ms).toHaveCount(3);
  await expect(ms.nth(0)).toHaveText('—');
  await expect(ms.nth(0)).toHaveAttribute('title', /ответа не было/);
  await expect(ms.nth(1)).toHaveText('—');
  await expect(ms.nth(2)).toHaveText('85');
  await expect(ms.nth(2)).not.toHaveAttribute('title', /.+/);
  await page.unroute(/\/api\/v1\/works\/calls(\?.*)?$/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_external_banner_testnet', async ({ page }) => {
  const problems = watch(page);
  await resetWorks(page);
  await connect(page, env('E2E_VAULT_LAN'));
  await open(page);
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  const d = dialog(page, 'Подключение модели');
  await d.getByLabel('Запись Vault').selectOption({ label: 'E2E модель (вне LAN) — https://203.0.113.10/v1' });
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
  // класс — поле сервера в ответе настроек (по закреплённому адресу, без DNS при чтении)
  const read = await human<{ works: { model: { host: string; external: boolean } } }>(page, 'GET', 'auth/settings');
  expect([read.works.model.host, read.works.model.external]).toEqual(['203.0.113.10', true]);
  // ни один вид не в режиме «модель» — данные не уходят: «уйдут, когда …» (ревью r1, Minor 7)
  await human(page, 'PATCH', 'auth/settings', { works: { mode: 'off' } });
  await page.reload();
  const banner2 = page.getByTestId('works-external');
  await expect(banner2).toContainText('Подключённая модель — вне локальной сети: 203.0.113.10');
  await expect(banner2).toContainText('уйдут за пределы локальной сети, когда вид работ будет в режиме «модель»');
  // «Отключить модель» — окно-подтверждение (опасное действие): подключение удаляется, плашки нет
  await page.getByRole('button', { name: 'Отключить модель', exact: true }).click();
  const off = dialog(page, 'Отключить модель?');
  await expect(off).toBeVisible();
  await off.getByRole('button', { name: 'Отключить', exact: true }).click();
  await expect(off).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: 'Модель отключена' })).toBeVisible();
  await expect(page.getByTestId('works-external')).toHaveCount(0);
  expect((await human<{ works: { model: unknown } }>(page, 'GET', 'auth/settings')).works.model).toBeNull();
  // модель в LAN: плашки нет
  await connect(page, env('E2E_VAULT_LAN'));
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Подключённая модель' })).toBeVisible();
  await expect(page.getByTestId('works-external')).toHaveCount(0);
  // плашка — только по полю сервера: имя, закреплённое во внешний адрес (ответ подставлен — DNS в
  // e2e нет; правило сервера — тест Rust `model_external_field_in_settings`), и после перезагрузки
  await page.route(/\/api\/v1\/auth\/settings$/, async (r) => {
    if (r.request().method() !== 'GET') return r.continue();
    const resp = await r.fetch();
    const j = (await resp.json()) as { works: { model: Record<string, unknown> } };
    j.works.model = { ...j.works.model, host: 'models.example.com', external: true };
    return r.fulfill({ response: resp, json: j });
  });
  await page.reload();
  await expect(page.getByTestId('works-external')).toContainText('Данные памяти уходят за пределы локальной сети: models.example.com');
  await page.reload();
  await expect(page.getByTestId('works-external')).toContainText('models.example.com');
  await page.unroute(/\/api\/v1\/auth\/settings$/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_external_banner_real_pause', async ({ page }) => {
  // ревью r2 задачи 8 (N1): настоящая пауза `vault_changed` на внешний адрес при выбранном режиме
  // «модель» (действующий на паузе — «выкл»): плашка — «уйдут на <адрес> сразу после «Подтвердить
  // адрес»», и после перезагрузки; после подтверждения — «уходят»
  const problems = watch(page, (path, st) => st === 409 && (path.endsWith('/auth/settings') || path.endsWith('/works/model/test')));
  await setMoveUrl(`${mock()}/v1`);
  await resetWorks(page);
  await connect(page, env('E2E_VAULT_MOVE'));
  try {
    await setMoveUrl(EXT_URL);
    await open(page);
    // закреплён адрес в LAN — плашки нет
    await expect(page.getByTestId('works-external')).toHaveCount(0);
    await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: 'Адрес подключения изменён' })).toBeVisible();
    const banner = page.getByTestId('works-external');
    const pending = `Данные памяти уйдут за пределы локальной сети на ${EXT_URL} сразу после «Подтвердить адрес»`;
    await expect(banner).toContainText(pending);
    await expect(page.getByTestId('works-effective-probes')).toContainText('действует: выкл');
    await page.reload();
    await expect(page.getByTestId('works-external')).toContainText(pending);
    // подтверждение — данные уходят на новый адрес
    await page.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
    const d = dialog(page, 'Подтвердить новый адрес подключения?');
    await expect(d.getByTestId('works-confirm-now')).toHaveText(EXT_URL);
    await d.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
    await expect(d).toBeHidden();
    await expect(page.getByTestId('works-external')).toContainText('Данные памяти уходят за пределы локальной сети: 203.0.113.10');
    await expect(page.getByTestId('works-effective-probes')).toHaveText('действует: модель');
  } finally {
    await setMoveUrl(`${mock()}/v1`);
    await resetWorks(page);
  }
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_send_journal_no_text', async ({ page }) => {
  test.setTimeout(120_000);
  const problems = watch(page);
  // свои источники проб (термины Knowledge): кандидаты пакета не зависят от сценариев раньше —
  // пробы агента в `check.spec` разбирают записи прогона, и при выборочном запуске модели может
  // быть нечего спросить (запуск кончается без обращения)
  const stamp = Date.now().toString(36);
  for (const i of [1, 2]) {
    await agent('POST', 'spaces/knowledge/entries', {
      kind: 'term',
      title: `Журнал отправок ${i} ${stamp}`,
      body: '',
      data: { definition: `Определение для пробы подключённой модели номер ${i}.`, aliases: [], code: [] },
    });
  }
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
  // фокус — на «Отмена» (финальное ревью M7.3, Minor 10): Enter не отправляет ключ и пакеты на
  // новый адрес без сверки «был / стал»; основная кнопка — не красная
  await expect(d.getByRole('button', { name: 'Отмена', exact: true })).toBeFocused();
  await expect(d.getByRole('button', { name: 'Подтвердить адрес', exact: true })).not.toHaveClass(/danger/);
  // адреса — моноширинно, каждый своей строкой (ревью r1, Minor 2)
  await expect(d.getByTestId('works-confirm-was')).toHaveText(`${mock()}/v1`);
  await expect(d.getByTestId('works-confirm-now')).toHaveText(`${mock()}/v2`);
  expect(await d.getByTestId('works-confirm-now').evaluate((e) => getComputedStyle(e).fontFamily)).toMatch(/mono|Consolas/i);
  await d.getByRole('button', { name: 'Подтвердить адрес', exact: true }).click();
  await expect(d.getByRole('alert')).toContainText(`Адрес снова изменился: был ${mock()}/v1 → стал ${mock()}/v3`);
  await expect(d.getByTestId('works-confirm-now')).toHaveText(`${mock()}/v3`);
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

test('works_admin_block', async ({ page }) => {
  const problems = watch(page);
  const member = env('E2E_MEMBER_ID');
  const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { user_id: string };
  // сводка — тем же кодом, что у сервера: строки пользователей и их логины
  const summary = await human<{ users: { user: string; login: string }[] }>(page, 'GET', 'admin/works');
  const login = summary.users.find((u) => u.user === member)!.login;
  const own = summary.users.find((u) => u.user === me.user_id)!.login;
  await page.goto(`${base()}#/works`);
  const block = page.getByTestId('works-admin');
  await expect(block.getByRole('heading', { name: 'Все пользователи', exact: true })).toBeVisible();
  const table = block.getByRole('table', { name: 'Сводка по пользователям' });
  await expect(table.locator('tbody tr')).toHaveCount(summary.users.length);
  // своя строка — показана сейчас; у строки — режимы, очередь, проверка
  const ownRow = table.locator(`tr[data-user="${me.user_id}"]`);
  await expect(ownRow.getByRole('link', { name: own, exact: true })).toHaveAttribute('aria-current', 'page');
  await expect(ownRow).toContainText('пробы: ');
  const row = table.locator(`tr[data-user="${member}"]`);
  await expect(row).toContainText('кандидаты в Rules: ');
  await expect(row).toContainText(/пусто|в очереди|идёт/);

  // имя — вид страницы пользователя: адрес `?user=`, запросы очереди, токенов, журнала и настроек — его
  const asked: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname.includes('/api/v1/')) asked.push(`${r.method()} ${u.pathname.replace(/^.*\/api\/v1\//, '')}${u.search}`);
  });
  await row.getByRole('link', { name: login, exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`#/works\\?user=${member}$`));
  await expect(page.getByRole('heading', { name: `Внутренние работы: ${login}`, exact: true })).toBeVisible();
  await expect(page.getByTestId('works-viewing')).toContainText(`Страница пользователя ${login}`);
  await expect(page.getByLabel(`Ночная проверка памяти пользователя ${login}`)).toBeVisible();
  await expect(table.locator(`tr[data-user="${member}"]`).getByRole('link', { name: login, exact: true })).toHaveAttribute('aria-current', 'page');
  await expect.poll(() => asked.filter((a) => a.startsWith('GET ')).sort()).toEqual(
    expect.arrayContaining([
      `GET admin/users/${member}/settings`,
      `GET works?limit=30&user=${member}`,
      `GET works/usage?user=${member}`,
      `GET works/calls?days=7&user=${member}`,
    ]),
  );
  // своих настроек и очереди страница пользователя не читает
  expect(asked.filter((a) => a === 'GET auth/settings' || (a.startsWith('GET works?limit=30') && !a.includes('user=')))).toEqual([]);

  // своя страница — по ссылке, точки паузы чужой страницы шапка не берёт
  await page.getByRole('link', { name: 'Свои внутренние работы', exact: true }).click();
  await expect(page).toHaveURL(/#\/works$/);
  await expect(page.getByRole('heading', { name: 'Внутренние работы', exact: true })).toBeVisible();
  await expect(page.getByTestId('works-viewing')).toHaveCount(0);

  // телефон — карточки
  await page.setViewportSize({ width: 390, height: 844 });
  const cards = page.getByTestId('works-admin').getByRole('list', { name: 'Сводка по пользователям' });
  await expect(cards).toBeVisible();
  await expect(cards.locator(`li[data-user="${member}"]`).getByRole('link', { name: login })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Запись личного Vault по REST (сессия человека): адрес `url`, значение — ключ подставной модели. */
const vaultItem = (page: Page, title: string, url: string) =>
  human<{ id: string }>(page, 'POST', 'spaces/knowledge/vault', { title, data: { type: 'token', url }, secret: MOCK_KEY });

/** Подписи пунктов выбора записи в окне подключения. */
const choiceLabels = (page: Page) =>
  dialog(page, 'Подключение модели')
    .getByLabel('Запись Vault')
    .locator('option')
    .allTextContents();

test('works_vault_choices_new_record_fix_url', async ({ page }) => {
  const problems = watch(page);
  await resetWorks(page);
  const sent = patches(page);
  await open(page);
  await page.getByRole('button', { name: 'Подключить модель', exact: true }).click();
  const d = dialog(page, 'Подключение модели');
  await expect(d).toBeVisible();
  // решение 166: в выборе — записи с адресом API, пункт «<имя> — <адрес>»; хоста с описанием нет
  const labels = await choiceLabels(page);
  expect(labels).toContain(`E2E модель (LAN) — ${mock()}/v1`);
  expect(labels.some((l) => l.includes('E2E хост') || l.includes('SSH'))).toBe(false);
  expect(labels.every((l) => / — https?:\/\//.test(l))).toBe(true);
  await expect(d).toContainText('Запись вашего Vault, где хранятся адрес API модели и ключ доступа. Выберите сервер модели или создайте запись');

  // решение 167: «Новая запись…» — окно записи поверх окна подключения; после «Создать» запись выбрана
  const title = `E2E модель без пути ${Date.now().toString(36)}`;
  await d.getByRole('button', { name: 'Новая запись…', exact: true }).click();
  const v = dialog(page, 'Новая запись Vault');
  await expect(v).toBeVisible();
  await expect(v.getByLabel('Название')).toBeFocused();
  await v.getByLabel('Название').fill(title);
  await v.getByLabel('Адрес API').fill(mock());
  await v.getByLabel('Ключ доступа').fill(MOCK_KEY);
  await v.getByRole('button', { name: 'Создать', exact: true }).click();
  await expect(v).toBeHidden();
  await expect(d).toBeVisible();
  await expect(d.getByLabel('Запись Vault').locator('option:checked')).toHaveText(`${title} — ${mock()}`);
  await expect(d.getByRole('status')).toContainText(`Запись «${title}» сохранена и выбрана.`);
  // запись с ключом — подключение с ключом (без ключа «без ключа» ставится само)
  await expect(d.getByLabel(/^Ключ/)).toHaveValue('key');
  // решение 168: адрес без /v1 — предупреждение с полным адресом запроса
  const warn = d.getByTestId('works-path-warning');
  await expect(warn).toContainText(`Адрес без /v1 — запрос уйдёт на ${mock()}/chat/completions; обычно нужен ${mock()}/v1`);
  await d.getByLabel('Имя модели').fill('e2e-mock');
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  // решение 169: проверка — «по адресу нет API» и полный адрес запроса
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  const err = page.getByRole('alert').filter({ hasText: 'Подключение не работает' });
  await expect(err).toContainText('по адресу нет API');
  await expect(err).toContainText(`Адрес запроса: ${mock()}/chat/completions`);

  // «Исправить адрес»: окно записи с адресом + /v1, ключ не трогается — проверка проходит
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  await d.getByTestId('works-path-warning').getByRole('button', { name: 'Исправить адрес', exact: true }).click();
  const e = dialog(page, 'Изменить запись Vault');
  await expect(e).toBeVisible();
  await expect(e.getByLabel('Адрес API')).toHaveValue(`${mock()}/v1`);
  await expect(e.getByLabel('Ключ доступа')).toHaveValue('');
  await e.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(e).toBeHidden();
  await expect(d.getByTestId('works-path-warning')).toHaveCount(0);
  await expect(d.getByTestId('works-request-url')).toContainText(`${mock()}/v1/chat/completions`);
  const before = (await mockRequests()).length;
  await d.getByRole('button', { name: 'Сохранить и проверить', exact: true }).click();
  await expect(d.getByRole('status')).toContainText('Подключение работает');
  const reqs = (await mockRequests()).slice(before);
  expect(reqs.map((r) => [r.path, r.auth])).toEqual([['/v1/chat/completions', true]]);
  // окно записи поверх окна подключения: Esc закрывает только его, фокус — на «Новая запись…»
  const add = d.getByRole('button', { name: 'Новая запись…', exact: true });
  await add.click();
  await expect(v).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(v).toBeHidden();
  await expect(d).toBeVisible();
  await expect(add).toBeFocused();
  await d.getByRole('button', { name: 'Отмена', exact: true }).click();
  await expect(d).toBeHidden();
  // телефон: «Назад» закрывает только верхнее окно, страница без горизонтальной прокрутки
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  await expect(d).toBeVisible();
  await add.click();
  await expect(v).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.goBack({ waitUntil: 'commit' });
  await expect(v).toBeHidden();
  await expect(d).toBeVisible();
  await page.goBack({ waitUntil: 'commit' });
  await expect(d).toBeHidden();
  await expect(page).toHaveURL(/#\/works$/);
  await page.setViewportSize({ width: 1280, height: 900 });
  // значение ключа ни в теле PATCH подключения, ни на странице
  expect(JSON.stringify(sent)).not.toContain(MOCK_KEY);
  expect(await page.content()).not.toContain(MOCK_KEY);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_test_errors_401_404', async ({ page }) => {
  const problems = watch(page);
  await resetWorks(page);
  const lan = env('E2E_VAULT_LAN');
  const conn = (model: string) =>
    human(page, 'PATCH', 'auth/settings', { works: { model: { protocol: 'openai', vault: `v:${lan}`, model, auth: 'key', max_tokens: 4000 } } });
  await conn('e2e-401');
  await open(page);
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  const err = page.getByRole('alert').filter({ hasText: 'Подключение не работает' });
  await expect(err).toContainText('ключ не подошёл');
  await expect(err).toContainText(`Адрес запроса: ${mock()}/v1/chat/completions`);
  // модели нет: список моделей сервера
  await conn('e2e-missing');
  await page.reload();
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(err).toContainText(`по адресу отвечает API, но модели «e2e-missing» на нём нет — есть: ${MOCK_MODELS.join(', ')}`);
  await resetWorks(page);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('works_protocol_anthropic', async ({ page }) => {
  const problems = watch(page);
  await resetWorks(page);
  const title = `E2E Anthropic ${Date.now().toString(36)}`;
  const item = await vaultItem(page, title, mock());
  const sent = patches(page);
  await open(page);
  await page.getByRole('button', { name: 'Подключить модель', exact: true }).click();
  const d = dialog(page, 'Подключение модели');
  await d.getByLabel('Протокол').selectOption({ label: 'Anthropic Messages' });
  await d.getByLabel('Запись Vault').selectOption({ label: `${title} — ${mock()}` });
  // у Anthropic: поля предела нет, адрес без /v1 — норма, запрос — <адрес>/v1/messages
  await expect(d.getByLabel('Поле предела')).toHaveCount(0);
  await expect(d.getByTestId('works-path-warning')).toHaveCount(0);
  await expect(d.getByTestId('works-request-url')).toContainText(`${mock()}/v1/messages`);
  await d.getByLabel('Имя модели').fill('claude-e2e');
  await d.getByRole('button', { name: 'Меньше размышлений (effort: low)', exact: true }).click();
  await expect(d.getByRole('textbox', { name: /^body_extra/ })).toHaveValue(/"output_config"/);
  // параметры выборки Anthropic не принимает — ошибка до сохранения
  await d.getByRole('textbox', { name: /^body_extra/ }).fill('{"temperature": 0.2}');
  await expect(d.getByRole('alert')).toContainText('temperature');
  await d.getByRole('textbox', { name: /^body_extra/ }).fill('{"output_config": {"effort": "low"}}');
  await d.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(d).toBeHidden();
  const model = (sent.at(-1)!.works as { model: Record<string, unknown> }).model;
  expect(model).toMatchObject({ protocol: 'anthropic', vault: `v:${item.id}`, token_field: 'auto', body_extra: { output_config: { effort: 'low' } } });
  await expect(page.getByRole('region', { name: 'Подключённая модель' })).toContainText('Anthropic Messages');
  // проверка подключения — протоколом подключения на заглушке
  const before = (await mockRequests()).length;
  await page.getByRole('button', { name: 'Проверить подключение', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: /^Подключение работает: ответ за \d+ мс\.$/ })).toBeVisible();
  const reqs = (await mockRequests()).slice(before);
  expect(reqs.map((r) => [r.protocol, r.kind, r.path, r.model, r.auth])).toEqual([['anthropic', 'test', '/v1/messages', 'claude-e2e', true]]);
  // телефон: окно без горизонтальной прокрутки, «Поле предела» скрыто
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Изменить подключение', exact: true }).click();
  await expect(d).toBeVisible();
  await expect(d.getByLabel('Протокол')).toHaveValue('anthropic');
  await expect(d.getByLabel('Поле предела')).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await d.getByRole('button', { name: 'Закрыть', exact: true }).first().click();
  await resetWorks(page);
  expect(problems, problems.join('\n')).toEqual([]);
});
