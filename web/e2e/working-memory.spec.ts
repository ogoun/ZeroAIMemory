// Сквозные сценарии «Рабочей памяти» (M4.5, задача 14): Словарь, Задачи, Вопросы, поля ADR,
// пометка «возможно устарело», окружение в Vault, переключатели подсказок хуков.
// Данные создаёт сам сценарий через REST по токену администратора (агент) в решении E2E из
// global-setup; заголовки уникальны, чтобы сценарии не мешали друг другу и журналам M3.
import { expect, test, type Page } from '@playwright/test';
import { ADMIN_STATE } from './session';
import { SOLUTION_GUID } from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });

/** Ошибки JS, CSP и неожиданные HTTP-ответы страницы; `allow` — ожидаемые коды. */
function watch(page: Page, allow: number[] = []): string[] {
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
      allow.includes(r.status());
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

/** REST по токену администратора (агент); `session` — заголовок `X-Zam-Session`. */
async function api<T = Record<string, any>>(
  method: string,
  path: string,
  body?: unknown,
  extra: Record<string, string> = {},
): Promise<T> {
  const r = await fetch(`${base()}api/v1/${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...extra,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as T;
}

const SP = `spaces/${SOLUTION_GUID}`;
const entry = (body: Record<string, unknown>) => api<{ id: string; rev: number }>('POST', `${SP}/entries`, body);
const patch = (id: string, rev: number, body: Record<string, unknown>) =>
  api<{ id: string; rev: number }>('PATCH', `entries/${id}`, body, { 'If-Match': `"${rev}"` });
const hex = (c: string) => c.repeat(64);

/** Открывает журнал решения E2E по адресу; возвращает id пользователя. */
async function openJournal(page: Page, journal: string, entryId = ''): Promise<string> {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  const user = new URL(page.url()).hash.split('/')[2];
  await page.goto(`${base()}#/u/${user}/s/${SOLUTION_GUID}/j/${journal}${entryId ? `/${entryId}` : ''}`);
  return user;
}

test('glossary_create_alias_conflict', async ({ page }) => {
  const problems = watch(page, [409]);
  await entry({
    kind: 'term',
    title: 'Выжимка WM',
    body: '',
    data: { definition: 'Краткое изложение найденного', aliases: ['summary-wm'], code: ['digest_wm'] },
  });
  await openJournal(page, 'glossary');
  await expect(page.getByRole('heading', { level: 1, name: 'Словарь' })).toBeVisible();
  const list = page.getByRole('list', { name: 'Термины' });
  await expect(list).toContainText('Выжимка WM');
  // поиск по псевдониму и по определению
  const find = page.getByRole('searchbox', { name: 'Поиск по словарю' });
  await find.fill('summary-wm');
  await expect(list.getByRole('listitem')).toHaveCount(1);
  await find.fill('изложение');
  await expect(list.getByRole('listitem')).toHaveCount(1);
  await find.fill('нет-такого-слова');
  await expect(list.getByRole('listitem')).toHaveCount(0);
  await find.fill('');

  // карточка: псевдонимы и код
  await list.getByRole('button', { name: 'Выжимка WM' }).click();
  const card = page.getByRole('article', { name: 'Карточка термина' });
  await expect(card).toContainText('summary-wm');
  await expect(card).toContainText('digest_wm');

  // новый термин с занятым псевдонимом: 409 и ссылка на занявший термин
  await page.getByRole('button', { name: 'Новый термин' }).click();
  const form = page.getByRole('form', { name: 'Новый термин' });
  await form.getByLabel('Имя').fill('Сводка WM');
  await form.getByLabel('Определение').fill('То же, но другое имя');
  await form.getByLabel(/Псевдонимы/).fill('summary-wm');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  const conflict = form.getByTestId('term-conflicts');
  await expect(conflict).toContainText('summary-wm');
  await expect(conflict.getByRole('link', { name: '«Выжимка WM»' })).toBeVisible();

  // другой псевдоним — сохраняется, термин появляется в алфавитном списке
  await form.getByLabel(/Псевдонимы/).fill('digest-wm-2');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  await expect(list).toContainText('Сводка WM');
  await expect(page.getByRole('article', { name: 'Карточка термина' })).toContainText('digest-wm-2');

  // правка с If-Match: определение меняется, ревизия растёт
  await page.getByRole('button', { name: 'Править' }).click();
  const edit = page.getByRole('form', { name: 'Правка термина' });
  await edit.getByLabel('Определение').fill('Новое определение сводки');
  const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().includes('/entries/'));
  await edit.getByRole('button', { name: 'Сохранить' }).click();
  const resp = await saved;
  expect(resp.request().headers()['if-match']).toBeTruthy();
  await expect(page.getByRole('article', { name: 'Карточка термина' })).toContainText('Новое определение сводки');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tasks_board_claim_shown', async ({ page }) => {
  const problems = watch(page);
  await entry({ kind: 'task', title: 'Задача WM открытая', body: '', data: { next_step: 'прочитать код' } });
  const claimed = await entry({ kind: 'task', title: 'Задача WM захвачена', body: '' });
  const p1 = await patch(claimed.id, claimed.rev, { status: 'in_progress' });
  await api('POST', `entries/${p1.id}/claim`, { ttl_min: 60, agent: 'claude-wm' }, { 'X-Zam-Session': 'e2e-wm-session' });
  const done = await entry({ kind: 'task', title: 'Задача WM выполнена', body: '' });
  const d1 = await patch(done.id, done.rev, { status: 'in_progress' });
  await patch(done.id, d1.rev, { status: 'done' });

  await openJournal(page, 'tasks');
  const board = page.getByRole('group', { name: 'Доска задач' });
  const col = (name: string) => board.getByRole('region', { name: `Задачи: ${name}` });
  await expect(col('открыт')).toContainText('Задача WM открытая');
  await expect(col('открыт')).toContainText('Дальше: прочитать код');
  await expect(col('в работе')).toContainText('Задача WM захвачена');
  await expect(col('в работе')).toContainText('в работе у claude-wm, до');
  await expect(col('выполнен')).toContainText('Задача WM выполнена');
  await expect(col('выполнен')).toContainText('длительность');

  // следующий шаг и смена статуса человеком
  await col('открыт').getByRole('button', { name: 'Задача WM открытая' }).click();
  const card = page.getByRole('article', { name: 'Карточка задачи' });
  await card.getByLabel('Следующий шаг').fill('написать тест');
  await card.getByRole('button', { name: 'Сохранить шаг' }).click();
  await expect(col('открыт')).toContainText('Дальше: написать тест');
  await card.getByRole('button', { name: '→ заблокирован' }).click();
  await card.getByLabel('Что мешает').fill('ждём ответа владельца');
  await card.getByRole('button', { name: 'Применить' }).click();
  await expect(col('заблокирован')).toContainText('Блокер: ждём ответа владельца');

  // снятие захвата человеком
  await col('в работе').getByRole('button', { name: 'Задача WM захвачена' }).click();
  await expect(card.getByTestId('claim')).toContainText('в работе у claude-wm');
  await card.getByRole('button', { name: 'Снять захват' }).click();
  await expect(card.getByTestId('claim')).toHaveCount(0);
  await expect(col('в работе')).not.toContainText('в работе у claude-wm');

  // переоткрытие выполненной — с причиной
  await col('выполнен').getByRole('button', { name: 'Задача WM выполнена' }).click();
  await card.getByRole('button', { name: 'Переоткрыть' }).click();
  await card.getByLabel('Причина переоткрытия').fill('тесты красные');
  await card.getByRole('button', { name: 'Применить' }).click();
  await expect(col('открыт')).toContainText('Задача WM выполнена');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('question_answer_one_click_creates_decision', async ({ page }) => {
  const problems = watch(page);
  const q = await entry({
    kind: 'question',
    title: 'Вопрос WM: какой формат?',
    body: 'Нужно выбрать формат выгрузки.',
    data: { options: [{ key: 'a', text: 'Формат А' }, { key: 'b', text: 'Формат Б' }], recommendation: 'b' },
  });
  await openJournal(page, 'questions');
  const card = page.locator(`[data-question="${q.id}"]`);
  await expect(card).toContainText('Вопрос WM: какой формат?');
  // рекомендация отмечена
  await expect(card.getByRole('button', { name: /Формат Б.*рекомендуется/ })).toBeVisible();
  const answered = page.waitForRequest((r) => r.url().includes(`/entries/${q.id}/answer`));
  await card.getByRole('button', { name: 'Формат А', exact: true }).click();
  const sent = (await answered).postDataJSON();
  expect(sent).toMatchObject({ option: 'a', channel: 'webui' });
  await expect(card).toContainText('Ответ: Формат А');
  // ссылка на созданное решение ведёт в Insights
  await card.getByTestId('decision-link').click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/insights/[0-9A-Za-z]+$`));
  await expect(page.getByRole('article', { name: 'Карточка insight' })).toContainText('Формат А');
  const decided = await api<{ items: { data: { decision?: { decided_by?: string } } }[] }>(
    'GET',
    `${SP}/entries?kind=insight&data_kind=decision&decided_by=human`,
  );
  expect(decided.items.some((e) => e.data.decision?.decided_by === 'human')).toBe(true);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('decision_adr_fields', async ({ page }) => {
  const problems = watch(page);
  const adr = {
    context: 'Нужен формат хранения черновиков',
    options: ['SQLite', 'файлы JSON'],
    chosen: 'SQLite',
    rationale: 'транзакции и поиск',
    consequences: 'нужна миграция',
    revisit_if: 'черновиков станет больше миллиона',
  };
  const human = await entry({
    kind: 'insight',
    title: 'Решение WM человека',
    body: 'Хранение черновиков.',
    data: { kind: 'decision', decision: { ...adr, decided_by: 'human' } },
  });
  await entry({
    kind: 'insight',
    title: 'Решение WM агента',
    body: 'Выбор кэша.',
    data: { kind: 'decision', decision: { ...adr, chosen: 'файлы JSON', decided_by: 'agent' } },
  });
  await openJournal(page, 'insights');
  const table = page.getByRole('table', { name: 'Insights' });
  await expect(table).toContainText('Решение WM человека');
  await expect(table).toContainText('Решение WM агента');

  // фильтр «решения человека»
  const filtered = page.waitForRequest((r) => r.url().includes('decided_by=human'));
  await page.getByLabel('Решения человека').check();
  await filtered;
  await expect(table).toContainText('Решение WM человека');
  await expect(table).not.toContainText('Решение WM агента');

  // карточка: поля ADR
  await table.getByRole('button', { name: 'Решение WM человека' }).click();
  const card = page.getByRole('article', { name: 'Карточка insight' });
  await expect(card.locator('dl.adr')).toContainText('Нужен формат хранения черновиков');
  await expect(card.locator('dl.adr')).toContainText('файлы JSON');
  await expect(card.locator('dl.adr')).toContainText('транзакции и поиск');
  await expect(card.locator('dl.adr')).toContainText('человек');
  await expect(card.locator('dl.adr')).toContainText('черновиков станет больше миллиона');

  // правка ADR на месте: If-Match, новая ревизия
  await card.getByRole('button', { name: 'Править ADR' }).click();
  const form = card.getByRole('form', { name: 'Поля ADR' });
  await form.getByLabel('Выбрано').fill('PostgreSQL');
  const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().includes(`/entries/${human.id}`));
  await form.getByRole('button', { name: 'Сохранить' }).click();
  expect((await saved).request().headers()['if-match']).toBeTruthy();
  await expect(card.locator('dl.adr')).toContainText('PostgreSQL');
  const now = await api<{ rev: number; data: { decision: { chosen: string; decided_by: string } } }>('GET', `entries/${human.id}`);
  expect(now.rev).toBeGreaterThan(human.rev);
  expect(now.data.decision).toMatchObject({ chosen: 'PostgreSQL', decided_by: 'human' });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('stale_ref_badge', async ({ page }) => {
  const problems = watch(page);
  const path = 'src/wm_stale.rs';
  await entry({
    kind: 'insight',
    title: 'Знание WM о файле wm_stale',
    body: 'Описание поведения файла wm_stale.',
    data: { kind: 'discovery' },
    refs: { files: [{ path, hash: hex('a') }] },
  });
  const note = await api<{ id: string }>('POST', 'nodes', {
    space: SOLUTION_GUID,
    path: 'E2E/WM заметка о файле',
    kind: 'note',
    content: `---\nrefs:\n  - ${path}\nverified_at: 2026-10-01\n---\n# WM заметка о файле\n\nПроверенная команда: cargo test.\n`,
  });
  // файл изменился: ссылка с хэшем — stale; у заметки хэша не было, первый отчёт его запоминает
  await api('POST', 'files/changed', { solution: SOLUTION_GUID, files: [{ path, hash: hex('b') }] });
  await api('POST', 'files/changed', { solution: SOLUTION_GUID, files: [{ path, hash: hex('c') }] });

  const user = await openJournal(page, 'insights');
  const table = page.getByRole('table', { name: 'Insights' });
  const row = table.locator('tr', { hasText: 'Знание WM о файле wm_stale' });
  await expect(row).toContainText('возможно устарело');
  await row.getByRole('button', { name: 'Знание WM о файле wm_stale' }).click();
  const card = page.getByRole('article', { name: 'Карточка insight' });
  await expect(card.getByTestId('stale-note')).toContainText(`возможно устарело: изменился ${path}`);
  await expect(card.getByTestId('file-refs')).toContainText(path);

  // просмотр заметки: пометка, ссылки и дата проверки
  await page.goto(`${base()}#/u/${user}/s/${SOLUTION_GUID}/n/${note.id}`);
  await expect(page.getByTestId('stale-note')).toContainText(`возможно устарело: изменился ${path}`);
  await expect(page.getByTestId('file-refs')).toContainText('проверено 2026-10-01');

  // поиск Ctrl+K: пометка в карточке записи
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.keyboard.press('Control+k');
  await page.getByRole('combobox', { name: 'Поиск по заметкам' }).fill('wm_stale');
  const hit = page.getByRole('option').filter({ hasText: 'Знание WM о файле wm_stale' });
  await expect(hit).toBeVisible();
  await expect(hit).toContainText(`возможно устарело: изменился ${path}`);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('vault_host_fields', async ({ page }) => {
  const problems = watch(page);
  await api('POST', `${SP}/vault`, {
    title: 'WM стенд приёмки',
    data: {
      type: 'host',
      purpose: 'приёмочный стенд',
      address: '10.0.0.7:22',
      limits: '2 ядра, 2 ГБ',
      notes: 'перезагружать только ночью',
    },
  });
  await openJournal(page, 'vault');
  const item = page.locator('li.item', { hasText: 'WM стенд приёмки' });
  await expect(item).toContainText('хост');
  const fields = item.locator('dl.open');
  await expect(fields).toContainText('приёмочный стенд');
  await expect(fields).toContainText('10.0.0.7:22');
  await expect(fields).toContainText('2 ядра, 2 ГБ');
  await expect(fields).toContainText('перезагружать только ночью');
  // секрет у окружения необязателен: «Показать» сообщает, что значения нет
  await item.getByRole('button', { name: 'Показать' }).click();
  await expect(item.getByRole('status')).toContainText('Значение не задано');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('hook_toggles_new_keys', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.goto(`${base()}#/agent-setup`);
  const flags = page.getByRole('group', { name: 'Типы подсказок' });
  const keys = ['subagent_start', 'file_context', 'session_journal', 'recovery'];
  for (const k of keys) {
    await expect(flags.getByLabel(new RegExp(`hooks\\.${k}$`))).toBeChecked();
  }
  await expect(page.getByText('hooks.recovery не действует на memory_recall event=recovery')).toBeVisible();

  for (const k of keys) {
    const box = flags.getByLabel(new RegExp(`hooks\\.${k}$`));
    const saved = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await box.uncheck();
    expect((await (await saved).json()).hooks[k]).toBe(false);
  }
  await page.reload();
  for (const k of keys) {
    await expect(flags.getByLabel(new RegExp(`hooks\\.${k}$`))).not.toBeChecked();
  }
  const mine = await page.request.get(`${base()}api/v1/auth/settings`);
  expect((await mine.json()).hooks).toMatchObject({
    subagent_start: false,
    file_context: false,
    session_journal: false,
    recovery: false,
    prompt: true,
  });
  // возвращаем умолчание: остальные сценарии и прогоны не должны видеть выключенное
  for (const k of keys) {
    const saved = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await flags.getByLabel(new RegExp(`hooks\\.${k}$`)).check();
    expect((await (await saved).json()).hooks[k]).toBe(true);
  }
  expect(problems, problems.join('\n')).toEqual([]);
});
