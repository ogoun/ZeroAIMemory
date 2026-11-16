// Сквозные сценарии журнала «Идеи» (M5.7 §10): лента и вкладки по происхождению, создание,
// «Принять как свою», «Начать» (ссылка на задачу; идея Knowledge — в общее решение с
// предупреждением), «Отложить» с датой UTC, «Отказаться», «Вернуть», задача снята, ответ на
// вопрос-предложение в «Вопросах» (и вопрос «вернуть» идею со снятой задачей, задача 7), конфликт
// ревизии, выключатель ideas.proactive, Ctrl+K. Разбор агентом (M6.5 §9, задача 8): чипы статусов
// со счётчиками и «Группировать по статусу», строка решения в ленте, «Разбор агента» и «История
// разборов» в карточке, «Изменить» у возвращённой → «Отправлено на разбор», «Удалить» отклонённой,
// пометка и фильтр «из идеи» в «Задачах», режим ideas.mode в «Подключении агента».
// Данные — seedIdeas (global-setup); сценарии, которые меняют статус, создают свою идею по токену
// администратора (агент: идея человека — `origin: human`, решения — `by: human`).
import { expect, test, type Locator, type Page } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  IDEAS_GUID,
  IDEAS_NAME,
  IDEA_AGENT_TITLE,
  IDEA_BASIS_TASK_TITLE,
  IDEA_HUMAN_TITLE,
  IDEA_KNOWLEDGE_TITLE,
  IDEA_PLANNED_TITLE,
  IDEA_PROPOSED_TITLE,
  IDEA_WORD,
  REVIEW_COMMIT,
  REVIEW_FILE,
  REVIEW_MISSING,
  reviewSummary,
} from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));

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

/** REST по токену администратора (агент). */
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

const SP = `spaces/${IDEAS_GUID}`;
/** Идея человека по токену (агент записал по словам человека). */
const newIdea = (title: string, where = SP) =>
  api<{ id: string; rev: number }>('POST', `${where}/entries`, {
    kind: 'idea',
    title,
    body: `${title}: замысел.`,
    data: { origin: 'human' },
  });
const decide = (id: string, decision: string, extra: Record<string, unknown> = {}) =>
  api<{ entry: Record<string, any>; task?: { id: string; rev: number } }>('POST', `entries/${id}/decide`, {
    decision,
    by: 'human',
    ...extra,
  });

/** Сегодняшняя дата UTC + `days` (`YYYY-MM-DD`). */
const utc = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString().slice(0, 10);

/** Атрибут (`min`) или значение поля — дата «сегодня UTC + days»: страница могла посчитать её по
 * ту сторону полуночи UTC, поэтому годится и значение на день раньше. */
async function expectUtcDay(l: Locator, days: number, attr?: string) {
  const ok = [utc(days - 1), utc(days)];
  const got = attr ? await l.getAttribute(attr) : await l.inputValue();
  expect(ok).toContain(got);
}

/** Прямой SQL в базе пользователя E2E (время «horizon наступил» без ожидания, как в тестах Rust). */
function sql(user: string, query: string, ...args: string[]) {
  const db = new DatabaseSync(fileURLToPath(new URL(`../../.tmp/e2e/data/users/${user}.db`, import.meta.url)));
  try {
    db.exec('PRAGMA busy_timeout = 5000');
    db.prepare(query).run(...args);
  } finally {
    db.close();
  }
}

let adminUser = '';
async function me(page: Page): Promise<string> {
  if (adminUser) return adminUser;
  const r = await page.request.get(`${base()}api/v1/auth/me`);
  adminUser = ((await r.json()) as { user_id: string }).user_id;
  return adminUser;
}

/** Открывает журнал «Идеи» (или запись в нём) пространства `space` по адресу. */
async function openIdeas(page: Page, entry: string | null = null, space = IDEAS_GUID) {
  const u = await me(page);
  await page.goto(`${base()}#/u/${u}/s/${space}/j/ideas${entry ? `/${entry}` : ''}`);
  await expect(page.getByRole('heading', { level: 1, name: 'Идеи' })).toBeVisible();
  if (entry) await expect(card(page)).toBeVisible();
}

const card = (page: Page) => page.getByRole('article', { name: 'Карточка идеи' });
const feed = (page: Page) => page.getByRole('feed', { name: 'Лента идей' });
/** Чипы статусов ленты и чип по подписи (имя кнопки — «<подпись> <счётчик>»). */
const chips = (page: Page) => page.getByRole('group', { name: 'Статус идей' });
const chip = (page: Page, title: string) => chips(page).getByRole('button', { name: new RegExp(`^${title}( \\d+)?$`) });
const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');

test('ideas_create_and_list', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  // журнал «Идеи» — после «Задач» у решения и у Knowledge
  const t = tree(page);
  await t.getByRole('button', { name: /^Solutions/ }).first().click();
  await t.getByRole('button', { name: new RegExp(`^${IDEAS_NAME}`) }).first().click();
  const journals = t.getByRole('list', { name: `Журналы: ${IDEAS_NAME}` });
  await expect(journals.getByRole('link')).toHaveText([
    'Changelog', 'Releases', 'Отчёты', 'Код', 'Insights', 'Словарь', 'Задачи', 'Идеи', 'Вопросы', 'Vault',
  ]);
  await journals.getByRole('link', { name: 'Идеи', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/ideas$`));

  // «Мои» по умолчанию: идеи человека, без идеи агента
  await expect(page.getByRole('tab', { name: 'Мои' })).toHaveAttribute('aria-selected', 'true');
  await expect(feed(page)).toContainText(IDEA_HUMAN_TITLE);
  await expect(feed(page)).not.toContainText(IDEA_AGENT_TITLE);
  const row = feed(page).locator(`[data-idea="${env('E2E_IDEA_HUMAN')}"]`);
  await expect(row).toContainText('высокий');
  await expect(row).toContainText('Непросмотрено');
  await expect(row).toContainText('#ui');

  // создание: заголовок, замысел, приоритет, теги; origin ставит сервер (human)
  await page.getByRole('button', { name: 'Новая идея' }).click();
  const form = page.getByRole('form', { name: 'Новая идея' });
  await form.getByLabel('Заголовок').fill('Идея E2E из формы');
  await form.getByLabel('Замысел').fill('Записать замысел из WebUI.');
  await form.getByLabel('Приоритет').selectOption('low');
  await form.getByLabel('Теги идеи').fill('web, e2e');
  // «не раньше» — не раньше завтрашней даты UTC, подпись UTC рядом
  await expectUtcDay(form.getByLabel('Не раньше'), 1, 'min');
  await expect(form.getByText('UTC', { exact: true })).toBeVisible();
  const sent = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith(`/spaces/${IDEAS_GUID}/entries`));
  await form.getByRole('button', { name: 'Записать' }).click();
  const body = (await sent).postDataJSON();
  expect(body).toMatchObject({ kind: 'idea', title: 'Идея E2E из формы', tags: ['web', 'e2e'], data: { priority: 'low' } });
  expect(body.data.origin).toBeUndefined();
  await expect(card(page)).toContainText('Идея E2E из формы');
  await expect(card(page)).toContainText('приоритет: низкий');
  await expect(page.getByRole('status')).toHaveText('Идея записана.');
  await expect(feed(page)).toContainText('Идея E2E из формы');

  // фильтр приоритета — на сервере
  const filtered = page.waitForRequest((r) => r.url().includes('kind=idea') && r.url().includes('priority=high'));
  await page.getByLabel('Приоритет идей').selectOption('high');
  await filtered;
  await expect(feed(page)).toContainText(IDEA_HUMAN_TITLE);
  await expect(feed(page)).not.toContainText('Идея E2E из формы');
  await page.screenshot({ path: `${screens}ideas-feed.png`, fullPage: true });

  // у Knowledge — тоже
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await expect(t.getByRole('list', { name: 'Журналы: Knowledge' }).getByRole('link')).toHaveText([
    'Insights', 'Словарь', 'Задачи', 'Идеи', 'Вопросы', 'Vault',
  ]);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_agent_filter_and_adopt', async ({ page }) => {
  const problems = watch(page);
  const id = env('E2E_IDEA_AGENT');
  await openIdeas(page);
  const listed = page.waitForRequest((r) => r.url().includes('kind=idea') && r.url().includes('origin=agent'));
  await page.getByRole('tab', { name: 'От агента' }).click();
  await listed;
  const row = feed(page).locator(`[data-idea="${id}"]`);
  await expect(row).toContainText(IDEA_AGENT_TITLE);
  await expect(row).toContainText('от агента');
  await expect(feed(page)).not.toContainText(IDEA_HUMAN_TITLE);

  // у «от агента» нет «Начать» до «Принять как свою»
  await row.getByRole('button', { name: IDEA_AGENT_TITLE }).click();
  await expect(card(page)).toContainText('от агента');
  await expect(card(page).getByRole('button', { name: 'Начать' })).toHaveCount(0);
  await card(page).getByRole('button', { name: 'Принять как свою' }).click();
  await expect(page.getByRole('status')).toHaveText('Идея принята как своя.');
  await expect(card(page).getByRole('button', { name: 'Начать' })).toBeVisible();
  await expect(card(page).locator('header')).not.toContainText('от агента');
  await expect(feed(page)).not.toContainText(IDEA_AGENT_TITLE);

  await page.getByRole('tab', { name: 'Мои' }).click();
  await expect(feed(page).locator(`[data-idea="${id}"]`)).toContainText(IDEA_AGENT_TITLE);
  const e = await api('GET', `entries/${id}`);
  expect(e.data.origin).toBe('human');
  expect(e.data.adopted_at).toBeTruthy();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_start_creates_task_link', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: начать задачей');
  await openIdeas(page, idea.id);
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${idea.id}/decide`));
  await card(page).getByRole('button', { name: 'Начать' }).click();
  expect((await sent).postDataJSON()).toEqual({ decision: 'accept' });
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  const link = card(page).getByTestId('task-link');
  await expect(link).toHaveText(/^Задача k:[0-9A-Z]{26}$/);
  await expect(page.getByRole('status')).toContainText('Идея начата: задача k:');
  const task = (await link.textContent())!.replace('Задача k:', '');
  // ссылка на задачу — и в строке ленты (идея решения: задача в том же решении)
  await chip(page, 'В плане').click();
  const rowLink = feed(page).locator(`[data-idea="${idea.id}"]`).getByRole('link', { name: `задача k:${task}` });
  await expect(rowLink).toHaveAttribute('href', new RegExp(`/s/${IDEAS_GUID}/j/tasks/${task}$`));
  await link.click();
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/tasks/${task}$`));
  const tc = page.getByRole('article', { name: 'Карточка задачи' });
  await expect(tc).toContainText('Идея E2E: начать задачей');
  await expect(tc).toContainText(`Идея: w:${idea.id}`);
  await expect(page.locator(`[data-task="${task}"]`)).toHaveClass(/selected/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_knowledge_start_in_shared_solution_warns', async ({ page }) => {
  const problems = watch(page);
  const id = env('E2E_IDEA_KNOWLEDGE');
  await openIdeas(page, id, 'knowledge');
  await expect(card(page)).toContainText(IDEA_KNOWLEDGE_TITLE);
  await card(page).getByRole('button', { name: 'Начать' }).click();
  const form = page.getByRole('form', { name: 'Начать идею' });
  const where = form.getByLabel('Пространство задачи');
  // без вопроса-предложения — Knowledge, предупреждения нет
  await expect(where).toHaveValue('knowledge');
  await expect(form.getByTestId('shared-warning')).toHaveCount(0);
  await where.selectOption(IDEAS_GUID);
  await expect(form.getByTestId('shared-warning')).toHaveText(
    `Задача появится в решении «${IDEAS_NAME}», её увидят участники.`,
  );
  await page.screenshot({ path: `${screens}ideas-knowledge-start.png`, fullPage: true });
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${id}/decide`));
  await form.getByRole('button', { name: 'Начать' }).click();
  expect((await sent).postDataJSON()).toEqual({ decision: 'accept', space: IDEAS_GUID });
  const link = card(page).getByTestId('task-link');
  await expect(link).toBeVisible();
  // задача — в общем решении, ссылка ведёт на его доску
  await expect(link).toHaveAttribute('href', new RegExp(`/s/${IDEAS_GUID}/j/tasks/`));
  const tasks = await api<{ items: { title: string }[] }>('GET', `${SP}/entries?kind=task`);
  expect(tasks.items.some((x) => x.title === IDEA_KNOWLEDGE_TITLE)).toBe(true);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_defer_with_date_and_decline', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: отложить и отказаться');
  await openIdeas(page, idea.id);
  await card(page).getByRole('button', { name: 'Отложить' }).click();
  const form = page.getByRole('form', { name: 'Отложить идею' });
  const date = form.getByLabel('Отложить до');
  // минимум — завтра по UTC, по умолчанию +30 дней, подпись UTC
  await expectUtcDay(date, 1, 'min');
  await expectUtcDay(date, 30);
  await expect(form.getByText('UTC', { exact: true })).toBeVisible();
  // повторное нажатие той же кнопки карточки открывает форму заново: введённое сброшено
  await date.fill(utc(7));
  await card(page).getByRole('group', { name: 'Решение по идее' }).getByRole('button', { name: 'Отложить' }).click();
  await expectUtcDay(date, 30);
  await date.fill(utc(5));
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${idea.id}/decide`));
  await form.getByRole('button', { name: 'Отложить' }).click();
  expect((await sent).postDataJSON()).toEqual({ decision: 'defer', horizon: utc(5) });
  await expect(card(page).locator('[data-status]')).toHaveText('Отложена');
  await expect(card(page)).toContainText(`Отложена до ${utc(5)} (UTC).`);
  await expect(feed(page).locator(`[data-idea="${idea.id}"]`)).toContainText(`не раньше ${utc(5)}`);

  await card(page).getByRole('button', { name: 'Отказаться' }).click();
  const dec = page.getByRole('form', { name: 'Отказаться от идеи' });
  await dec.getByLabel('Причина (необязательно)').fill('уже не нужно');
  await dec.getByRole('button', { name: 'Отказаться' }).click();
  await expect(card(page).locator('[data-status]')).toHaveText('Отклонена');
  // «Активные» отклонённую не показывают
  await expect(feed(page)).not.toContainText('Идея E2E: отложить и отказаться');
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('declined');
  expect(e.data.horizon).toBeUndefined();
  expect(problems, problems.join('\n')).toEqual([]);
});

// Формы карточки — ленивый чанк `IdeaForm-*.js`. Чанк не пришёл (сервер обновился — у чанков новые
// имена, или нет сети): вместо молчания — сообщение `role="alert"`, «Отмена» возвращает карточку.
test('ideas_form_chunk_load_failure_shows_alert', async ({ page }) => {
  const idea = await newIdea('Идея E2E: форма не загрузилась');
  await page.route('**/IdeaForm-*.js', (route) => route.abort());
  await openIdeas(page, idea.id);
  const actions = card(page).getByRole('group', { name: 'Решение по идее' });
  await actions.getByRole('button', { name: 'Отложить' }).click();
  const alert = card(page).getByRole('alert');
  await expect(alert).toContainText('Форма не загрузилась');
  await expect(alert).toContainText('обновите страницу');
  await expect(page.getByRole('form', { name: 'Отложить идею' })).toHaveCount(0);
  await card(page).getByRole('button', { name: 'Отмена' }).click();
  await expect(card(page).getByRole('alert')).toHaveCount(0);
  await expect(actions.getByRole('button', { name: 'Отложить' })).toBeVisible();
  // и у формы создания — вне карточки
  await page.getByRole('button', { name: 'Новая идея' }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Форма не загрузилась' })).toBeVisible();
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('ideas_reopen_declined_with_reason', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: вернуть отклонённую');
  await decide(idea.id, 'decline');
  await openIdeas(page);
  await chip(page, 'Отклонена').click();
  await feed(page).getByRole('button', { name: 'Идея E2E: вернуть отклонённую' }).click();
  await expect(card(page).locator('[data-status]')).toHaveText('Отклонена');
  await expect(card(page).getByRole('button', { name: 'Начать' })).toHaveCount(0);
  await card(page).getByRole('button', { name: 'Вернуть на разбор' }).click();
  const form = page.getByRole('form', { name: 'Вернуть идею' });
  await expect(form).toContainText('Вернуть на разбор');
  await form.getByLabel('Причина').fill('снова актуально');
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${idea.id}/decide`));
  await form.getByRole('button', { name: 'Вернуть' }).click();
  expect((await sent).postDataJSON()).toEqual({ decision: 'reopen', reason: 'снова актуально' });
  await expect(card(page).locator('[data-status]')).toHaveText('Непросмотрено');
  await expect(card(page).getByRole('button', { name: 'Начать' })).toBeVisible();
  expect((await api('GET', `entries/${idea.id}`)).status).toBe('new');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_accepted_dropped_task_reopen', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: задача снята');
  const r = await decide(idea.id, 'accept');
  const task = r.task!;
  await api('PATCH', `entries/${task.id}`, { status: 'dropped' }, { 'If-Match': `"${task.rev}"` });
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  await expect(card(page).getByTestId('task-released')).toContainText('Задача снята');
  await expect(card(page).getByRole('button', { name: 'Начать' })).toHaveCount(0);
  await expect(card(page).getByRole('button', { name: 'Отказаться' })).toBeVisible();
  await card(page).getByRole('button', { name: 'Вернуть на разбор' }).click();
  const form = page.getByRole('form', { name: 'Вернуть идею' });
  await form.getByLabel('Причина').fill('задачу сняли, вернёмся позже');
  await form.getByRole('button', { name: 'Вернуть' }).click();
  await expect(card(page).locator('[data-status]')).toHaveText('Непросмотрено');
  await expect(card(page).getByTestId('task-released')).toHaveCount(0);
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('new');
  expect(e.data.prev_task_ids).toContain(task.id);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_proposal_answered_in_questions', async ({ page }) => {
  const problems = watch(page);
  const ideaId = env('E2E_IDEA_PROPOSED');
  const qid = env('E2E_IDEA_QUESTION');
  // карточка идеи: вопрос агента с его текстом
  await openIdeas(page, ideaId);
  await expect(card(page).locator('[data-status]')).toHaveText('Предложена');
  await expect(card(page)).toContainText('Сейчас правим граф — экспорт ляжет рядом.');

  const u = await me(page);
  await page.goto(`${base()}#/u/${u}/s/${IDEAS_GUID}/j/questions/${qid}`);
  const q = page.locator(`[data-question="${qid}"]`);
  await expect(q).toContainText(`Начать идею «${IDEA_PROPOSED_TITLE}»?`);
  await expect(q.getByTestId('idea-link')).toHaveText(`Идея w:${ideaId}`);
  // у вопроса-предложения нет ответа текстом: только варианты
  await expect(q.getByRole('form', { name: 'Ответ текстом' })).toHaveCount(0);
  await q.getByRole('button', { name: 'отложить', exact: true }).click();
  const form = q.getByRole('form', { name: 'Отложить идею' });
  const date = form.getByLabel('Отложить до');
  await expectUtcDay(date, 1, 'min');
  await expect(form.getByText('UTC', { exact: true })).toBeVisible();
  await date.fill(utc(10));
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${qid}/answer`));
  await form.getByRole('button', { name: 'Отложить' }).click();
  expect((await sent).postDataJSON()).toEqual({ option: 'defer', horizon: utc(10), channel: 'webui' });
  await expect(q).toContainText('Ответ: отложить');
  const e = await api('GET', `entries/${ideaId}`);
  expect(e.status).toBe('deferred');
  expect(e.data.horizon).toBe(utc(10));
  // ссылка «Идея w:…» ведёт в журнал идей
  await q.getByTestId('idea-link').click();
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/ideas/${ideaId}$`));
  await expect(card(page).locator('[data-status]')).toHaveText('Отложена');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_stale_rev_conflict_rereads', async ({ page }) => {
  // 409 устаревшей ревизии — ожидаемый ответ сценария
  const problems = watch(page, [409]);
  const idea = await newIdea('Идея E2E: конфликт ревизии');
  await openIdeas(page, idea.id);
  await card(page).getByRole('button', { name: 'Изменить' }).click();
  const form = page.getByRole('form', { name: 'Правка идеи' });
  await form.getByLabel('Заголовок').fill('Идея E2E: правка человека');
  // агент дополнил идею, пока форма открыта (дополнение идеи человека токену разрешено)
  await api('PATCH', `entries/${idea.id}`, { append_body: 'Дополнение агента.' }, { 'If-Match': `"${idea.rev}"` });
  const saved = page.waitForResponse((r) => r.url().endsWith(`/entries/${idea.id}`) && r.request().method() === 'PATCH');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  expect((await saved).status()).toBe(409);
  // карточка перечитана: дополнение видно, правка не применена, ошибка с номером ревизии
  await expect(card(page)).toContainText('Дополнение агента.');
  await expect(card(page).locator('h2')).toHaveText('Идея E2E: конфликт ревизии');
  await expect(card(page).getByRole('alert')).toContainText('Запись изменилась');
  await expect(card(page)).toContainText(`ревизия ${idea.rev + 1}`);
  // повтор после перечитывания проходит
  await card(page).getByRole('button', { name: 'Изменить' }).click();
  await form.getByLabel('Заголовок').fill('Идея E2E: правка человека');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  await expect(card(page).locator('h2')).toHaveText('Идея E2E: правка человека');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_edit_with_due_horizon', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: наступивший срок');
  await decide(idea.id, 'defer', { horizon: utc(5) });
  // срок наступил: horizon в прошлом (прямой SQL, без ожидания)
  sql(await me(page), "UPDATE entries SET data = json_set(data, '$.horizon', '2020-01-02') WHERE id = ?", idea.id);
  await openIdeas(page, idea.id);
  await expect(card(page)).toContainText('Отложена до 2020-01-02 (UTC).');
  await card(page).getByRole('button', { name: 'Изменить' }).click();
  const form = page.getByRole('form', { name: 'Правка идеи' });
  const date = form.getByLabel('Не раньше');
  // неизменённая дата не ограничена min (иначе браузер не отправит форму), изменённая — ограничена
  await expect(date).toHaveValue('2020-01-02');
  expect(await date.getAttribute('min')).toBeNull();
  await form.getByLabel('Заголовок').fill('Идея E2E: наступивший срок, правка');
  const saved = page.waitForResponse((r) => r.url().endsWith(`/entries/${idea.id}`) && r.request().method() === 'PATCH');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  const resp = await saved;
  expect(resp.status()).toBe(200);
  expect(resp.request().postDataJSON().data).toBeUndefined();
  await expect(card(page).locator('h2')).toHaveText('Идея E2E: наступивший срок, правка');
  await card(page).getByRole('button', { name: 'Изменить' }).click();
  await date.fill(utc(7));
  await expectUtcDay(date, 1, 'min');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_accepted_deleted_task_decline', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: задача удалена');
  const task = (await decide(idea.id, 'accept')).task!;
  const r = await fetch(`${base()}api/v1/entries/${task.id}`, {
    method: 'DELETE',
    headers: { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`, 'If-Match': `"${task.rev}"` },
  });
  expect(r.ok, await r.text()).toBe(true);
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  await expect(card(page).getByTestId('task-released')).toContainText('Задача удалена');
  await expect(card(page).getByTestId('task-link')).toHaveCount(0);
  await expect(card(page).getByRole('button', { name: 'Вернуть на разбор' })).toBeVisible();
  await card(page).getByRole('button', { name: 'Отказаться' }).click();
  await page.getByRole('form', { name: 'Отказаться от идеи' }).getByRole('button', { name: 'Отказаться' }).click();
  await expect(card(page).locator('[data-status]')).toHaveText('Отклонена');
  expect((await api('GET', `entries/${idea.id}`)).status).toBe('declined');
  expect(problems, problems.join('\n')).toEqual([]);
});

// Задача 7 (§16, п. 6): агент предложил вернуть идею со снятой задачей — пометка в «Идеях»,
// ответ «вернуть» в «Вопросах» (кнопки «вернуть»/«отказаться», без поля даты) → идея `new`.
test('ideas_reopen_proposal_answered_in_questions', async ({ page }) => {
  const problems = watch(page);
  const title = 'Идея E2E: предложено вернуть';
  const idea = await newIdea(title);
  const task = (await decide(idea.id, 'accept')).task!;
  await api('PATCH', `entries/${task.id}`, { status: 'dropped' }, { 'If-Match': `"${task.rev}"` });
  const p = await api<{ entry: Record<string, any>; question: { id: string } }>('POST', `entries/${idea.id}/propose`, {
    text: 'Снова просят экспорт — пора вернуть.',
  });
  expect(p.entry.status).toBe('accepted');
  const qid = p.question.id;
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  const mark = card(page).getByTestId('reopen-proposed');
  await expect(mark).toContainText('Предложено вернуть.');
  await expect(mark).toContainText('Снова просят экспорт — пора вернуть.');
  await expect(mark.getByRole('link', { name: `Вопрос q:${qid}` })).toHaveAttribute('href', new RegExp(`/j/questions/${qid}$`));
  await expect(card(page).getByRole('button', { name: 'Вернуть на разбор' })).toBeVisible();
  await expect(card(page).getByRole('button', { name: 'Отказаться' })).toBeVisible();

  const u = await me(page);
  await page.goto(`${base()}#/u/${u}/s/${IDEAS_GUID}/j/questions/${qid}`);
  const q = page.locator(`[data-question="${qid}"]`);
  await expect(q).toContainText(`Вернуть идею «${title}»?`);
  await expect(q.getByTestId('idea-link')).toHaveText(`Идея w:${idea.id}`);
  await expect(q.getByRole('group', { name: 'Варианты ответа' }).getByRole('button')).toHaveText(['вернуть', 'отказаться']);
  await expect(q.getByRole('form', { name: 'Ответ текстом' })).toHaveCount(0);
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${qid}/answer`));
  await q.getByRole('button', { name: 'вернуть', exact: true }).click();
  expect((await sent).postDataJSON()).toEqual({ option: 'reopen', channel: 'webui' });
  await expect(q).toContainText('Ответ: вернуть');
  await expect(q.getByRole('form', { name: 'Отложить идею' })).toHaveCount(0);
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('new');
  expect(e.data.prev_task_ids).toContain(task.id);
  await q.getByTestId('idea-link').click();
  await expect(card(page).locator('[data-status]')).toHaveText('Непросмотрено');
  await expect(card(page).getByTestId('reopen-proposed')).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_knowledge_proposal_start_in_shared_solution_warns', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Общая идея E2E: предложена в общем решении', 'spaces/knowledge');
  const { question } = await api<{ question: { id: string } }>('POST', `entries/${idea.id}/propose`, {
    text: 'Подходит к текущей работе.',
    space: IDEAS_GUID,
  });
  const u = await me(page);
  await page.goto(`${base()}#/u/${u}/s/knowledge/j/questions/${question.id}`);
  const q = page.locator(`[data-question="${question.id}"]`);
  await expect(q).toContainText('Общая идея E2E: предложена в общем решении');
  let answered = false;
  page.on('request', (r) => {
    if (r.url().endsWith(`/entries/${question.id}/answer`)) answered = true;
  });
  await q.getByRole('button', { name: 'начать', exact: true }).click();
  const confirm = q.getByRole('group', { name: 'Подтверждение «начать»' });
  await expect(confirm.getByTestId('shared-warning')).toHaveText(
    `Задача появится в решении «${IDEAS_NAME}», её увидят участники.`,
  );
  // пока подтверждение открыто, исходная кнопка «начать» неактивна
  await expect(q.getByRole('button', { name: 'начать', exact: true })).toBeDisabled();
  expect(answered).toBe(false);
  // отмена — без ответа
  await confirm.getByRole('button', { name: 'Отмена' }).click();
  await expect(confirm).toHaveCount(0);
  await expect(q.getByRole('button', { name: 'начать', exact: true })).toBeEnabled();
  expect(answered).toBe(false);
  await q.getByRole('button', { name: 'начать', exact: true }).click();
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${question.id}/answer`));
  await confirm.getByRole('button', { name: 'Начать' }).click();
  expect((await sent).postDataJSON()).toEqual({ option: 'accept', channel: 'webui' });
  await expect(q).toContainText('Ответ: начать');
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('accepted');
  const tasks = await api<{ items: { id: string }[] }>('GET', `${SP}/entries?kind=task`);
  expect(tasks.items.some((x) => x.id === e.data.task_id)).toBe(true);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('agent_setup_ideas_toggle_persists', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.goto(`${base()}#/agent-setup`);
  const box = page.getByRole('group', { name: 'Типы подсказок' }).getByLabel(/ideas\.proactive$/);
  await expect(box).toBeChecked();
  await expect(page.getByText('Идеи: раздел дайджеста и подсказки (разбор и предложения)')).toBeVisible();
  try {
    const saved = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await box.uncheck();
    const off = await saved;
    expect(off.request().postDataJSON()).toEqual({ ideas: { proactive: false } });
    expect((await off.json()).ideas.proactive).toBe(false);
    await page.reload();
    await expect(box).not.toBeChecked();
    const mine = await page.request.get(`${base()}api/v1/auth/settings`);
    // режим идей (ideas.mode, M6.5) выключатель не меняет
    expect((await mine.json()).ideas).toEqual({ proactive: false, mode: 'review' });
    const back = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await box.check();
    expect((await (await back).json()).ideas.proactive).toBe(true);
  } finally {
    // умолчание возвращается и при сбое: остальные сценарии и прогоны не видят выключенное
    const { csrf } = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
    const r = await page.request.patch(`${base()}api/v1/auth/settings`, {
      headers: { 'X-CSRF-Token': csrf },
      data: { ideas: { proactive: true } },
    });
    expect(r.status()).toBe(200);
  }
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_tabs_keyboard', async ({ page }) => {
  const problems = watch(page);
  await openIdeas(page);
  const tab = (name: string) => page.getByRole('tab', { name, exact: true });
  await tab('Мои').focus();
  let req = page.waitForRequest((r) => r.url().includes('kind=idea') && r.url().includes('origin=agent'));
  await page.keyboard.press('ArrowRight');
  await req;
  await expect(tab('От агента')).toBeFocused();
  await expect(tab('От агента')).toHaveAttribute('aria-selected', 'true');
  await expect(tab('Мои')).toHaveAttribute('tabindex', '-1');
  req = page.waitForRequest((r) => r.url().includes('kind=idea') && !r.url().includes('origin='));
  await page.keyboard.press('End');
  await req;
  await expect(tab('Все')).toBeFocused();
  await expect(feed(page)).toContainText(IDEA_HUMAN_TITLE);
  await page.keyboard.press('Home');
  await expect(tab('Мои')).toBeFocused();
  await expect(tab('Мои')).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('ArrowLeft');
  await expect(tab('Все')).toBeFocused();
  await expect(page.getByRole('tabpanel')).toHaveAttribute('aria-labelledby', /-tab-all$/);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ctrlk_finds_idea', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await expect(search).toBeFocused();
  await search.fill(IDEA_WORD);
  const option = page.getByRole('option').filter({ hasText: IDEA_HUMAN_TITLE });
  await expect(option).toBeVisible();
  await expect(option).toContainText('идея');
  await expect(option).toContainText(IDEAS_NAME);
  await search.press('Enter');
  await expect(page.getByRole('dialog')).toBeHidden();
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/ideas/${env('E2E_IDEA_HUMAN')}$`));
  await expect(card(page)).toContainText(IDEA_HUMAN_TITLE);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('webui_search_finds_cached_summary_ideas_in_defaults', async ({ page }) => {
  // I9: умолчания поиска WebUI теперь включают вид idea, а ключ кэша выжимок прежний — выжимку
  // агента находит поиск сессии WebUI, Ctrl+K работает как раньше
  const problems = watch(page);
  const src = await api<{ id: string }>('POST', `${SP}/entries`, {
    kind: 'insight',
    title: 'Источник выжимки идей E2E',
    body: 'Граф идей строится пентаграммными рёбрами.',
    data: { kind: 'discovery' },
  });
  const QUERY = 'как строится граф идей';
  const cache = await api<{ id: string }>('POST', 'cache', {
    query: QUERY,
    summary: 'Граф идей — пентаграммными рёбрами.',
    solution: IDEAS_GUID,
    sources: [{ id: `i:${src.id}` }],
  });
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  const { csrf } = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
  const r = await page.request.post(`${base()}api/v1/search`, {
    headers: { 'X-CSRF-Token': csrf },
    data: { query: QUERY, scope: { solution: IDEAS_GUID }, format: 'json' },
  });
  expect(r.status(), await r.text()).toBe(200);
  const items = ((await r.json()) as { items: { kind?: string; id?: string }[] }).items;
  expect(items.some((x) => x.kind === 'cache' && x.id === cache.id)).toBe(true);

  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await search.fill('пентаграммными');
  await expect(page.getByRole('option').filter({ hasText: 'Источник выжимки идей E2E' })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

// ---- разбор идей агентом (M6.5 §9, задача 8) ----

/** Возвращённая идея: идея человека и разбор `return` токеном (агент). */
async function returnedIdea(title: string, missing = REVIEW_MISSING) {
  const idea = await newIdea(title);
  const r = await api<{ entry: { id: string; rev: number; status: string } }>('POST', `entries/${idea.id}/review`, {
    outcome: 'return',
    summary: reviewSummary(title),
    missing,
  });
  expect(r.entry.status).toBe('returned');
  return r.entry;
}

const LABELS = ['Непросмотрено', 'На доработку', 'В плане', 'Отклонена', 'Сделано', 'Отложена', 'Предложена'];
const STATUSES = ['new', 'returned', 'accepted', 'declined', 'done', 'deferred', 'proposed'];

test('ideas_review_chips_and_grouping', async ({ page }) => {
  const problems = watch(page);
  const own = await returnedIdea('Идея E2E: чип «На доработку»');
  await openIdeas(page);
  const group = chips(page);
  // чипы со счётчиками: «Активные», статусы по порядку, «Все»; числа — как у сервера (вкладка «Мои»)
  const c = await api<Record<string, number>>('GET', `${SP}/entries/counts?kind=idea&origin=human`);
  const active = c.new + c.returned + c.proposed + c.deferred;
  const all = STATUSES.reduce((n, s) => n + c[s], 0);
  await expect(group.getByRole('button')).toHaveText([
    `Активные ${active}`,
    ...STATUSES.map((s, i) => `${LABELS[i]} ${c[s]}`),
    `Все ${all}`,
  ]);
  await expect(chip(page, 'Активные')).toHaveAttribute('aria-pressed', 'true');
  expect(c.returned).toBeGreaterThanOrEqual(2);

  // щелчок по «На доработку» — на сервере: в ленте только возвращённые, сколько в чипе
  const req = page.waitForRequest((r) => r.url().includes('kind=idea') && r.url().includes('status=returned'));
  await chip(page, 'На доработку').click();
  await req;
  await expect(chip(page, 'На доработку')).toHaveAttribute('aria-pressed', 'true');
  const rows = feed(page).locator('article.idea');
  await expect(rows).toHaveCount(c.returned);
  await expect(feed(page).locator(`[data-idea="${own.id}"]`)).toContainText('На доработку');
  await expect(feed(page).locator('article.idea [data-status]:not([data-status="returned"])')).toHaveCount(0);
  // строка решения у возвращённой — «Не хватает: <начало missing>»
  // (вопрос длиннее 60 символов — обрезка с «…»)
  const clipped = `${[...REVIEW_MISSING].slice(0, 60).join('').trimEnd()}…`;
  expect([...REVIEW_MISSING].length).toBeGreaterThan(60);
  await expect(feed(page).locator(`[data-idea="${own.id}"]`).getByTestId('review-line')).toHaveText(`Не хватает: ${clipped}`);

  // «Все» + «Группировать по статусу»: разделы — в порядке чипов, только непустые
  await chip(page, 'Все').click();
  await expect(feed(page).locator('article.idea')).toHaveCount(all);
  await page.getByLabel('Группировать по статусу').check();
  // разделы — в порядке чипов; у каждого своя лента (в role=feed — только статьи), общей ленты нет
  const groups = page.locator('[data-group]');
  const expected = STATUSES.filter((s) => c[s] > 0);
  await expect(groups).toHaveCount(expected.length);
  expect(await groups.evaluateAll((els) => els.map((e) => e.getAttribute('data-group')))).toEqual(expected);
  await expect(page.getByRole('feed', { name: 'Лента идей' })).toHaveCount(0);
  const returnedGroup = page.getByRole('region', { name: 'Раздел «На доработку»' });
  // все загружены — в заголовке число сервера
  await expect(returnedGroup.locator('h3')).toHaveText(`На доработку ${c.returned}`);
  await expect(returnedGroup.getByRole('feed', { name: 'Идеи раздела «На доработку»' }).locator(`[data-idea="${own.id}"]`)).toBeVisible();
  await page.screenshot({ path: `${screens}ideas-review-grouped.png`, fullPage: true });
  // в плане — строка «В план → k:<id> «…»» ведёт на задачу
  const planned = page.locator(`[data-idea="${env('E2E_IDEA_PLANNED')}"]`);
  const line = planned.getByTestId('review-line');
  await expect(line).toHaveText(`В план → k:${env('E2E_IDEA_PLAN_TASK')} «${IDEA_PLANNED_TITLE}»`);
  await expect(line).toHaveAttribute('href', new RegExp(`/s/${IDEAS_GUID}/j/tasks/${env('E2E_IDEA_PLAN_TASK')}$`));
  // отклонённая разбором — «Отклонена: <начало разбора>»
  await expect(page.locator(`[data-idea="${env('E2E_IDEA_DECLINED')}"]`).getByTestId('review-line')).toContainText('Отклонена: Идея просит');
  // выключили — снова одна лента
  await page.getByLabel('Группировать по статусу').uncheck();
  await expect(groups).toHaveCount(0);
  await expect(feed(page)).toBeVisible();
  // при поиске чипы без чисел (числа — ленты, не выдачи поиска)
  await page.getByLabel('Поиск по идеям').fill('доработку');
  await page.getByRole('button', { name: 'Применить' }).click();
  await expect(page.getByRole('feed', { name: 'Найденные идеи' })).toBeVisible();
  await expect(chip(page, 'Все')).toHaveText('Все');
  await page.getByRole('button', { name: 'Сбросить' }).click();
  await expect(chip(page, 'Все')).toHaveText(`Все ${all}`);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_review_card_shows_summary_and_history', async ({ page }) => {
  const problems = watch(page);
  const id = env('E2E_IDEA_PLANNED');
  const task = env('E2E_IDEA_PLAN_TASK');
  await openIdeas(page, id);
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  const rv = card(page).getByRole('region', { name: 'Разбор агента' });
  // исход, задача плана, разбор текстом с переносами
  await expect(rv.locator('[data-outcome]')).toHaveText('В план');
  await expect(rv.getByRole('link', { name: `k:${task}` }).first()).toHaveAttribute('href', new RegExp(`/j/tasks/${task}$`));
  const summary = rv.locator('p.body').first();
  await expect(summary).toBeVisible();
  expect(await summary.textContent()).toBe(reviewSummary('показать разбор в карточке'));
  expect(await summary.evaluate((e) => getComputedStyle(e).whiteSpace)).toBe('pre-wrap');
  // «Опирался на»: запись — ссылка в её журнал, файл и коммит — текстом с «копировать»
  const basis = rv.getByTestId('review-basis');
  await expect(basis).toContainText('Опирался на:');
  const basisLink = basis.getByRole('link', { name: `k:${env('E2E_IDEA_BASIS_TASK')}` });
  await expect(basisLink).toHaveAttribute('href', new RegExp(`/s/${IDEAS_GUID}/j/tasks/${env('E2E_IDEA_BASIS_TASK')}$`));
  await expect(basis.locator('code')).toHaveText([`file:${REVIEW_FILE}`, `commit:${REVIEW_COMMIT}`]);
  await expect(basis.getByRole('button', { name: 'копировать' })).toHaveCount(2);
  await expect(basis.getByRole('link', { name: /^file:|^commit:/ })).toHaveCount(0);
  // «Похожие» — идея, ссылка в «Идеи»
  const similar = rv.getByTestId('review-similar');
  await expect(similar.getByRole('link', { name: `w:${env('E2E_IDEA_DECLINED')}` })).toHaveAttribute(
    'href',
    new RegExp(`/j/ideas/${env('E2E_IDEA_DECLINED')}$`),
  );
  // ссылка на задачу в карточке — как у «Начать»
  await expect(card(page).getByTestId('task-link')).toHaveText(`Задача k:${task}`);
  // «История разборов» — свёрнута, раскрывается: прежний разбор «на доработку» с вопросом
  const history = card(page).getByTestId('review-history');
  await expect(history.locator('summary')).toHaveText('История разборов (1)');
  await expect(history.getByText('Что показывать в карточке?')).toBeHidden();
  await history.locator('summary').click();
  await expect(history.locator('[data-outcome]')).toHaveText('На доработку');
  await expect(history.getByText('Что показывать в карточке?')).toBeVisible();
  await page.screenshot({ path: `${screens}ideas-review-card.png`, fullPage: true });
  // ссылка «Опирался на» ведёт в карточку задачи
  await basisLink.click();
  await expect(page.getByRole('article', { name: 'Карточка задачи' })).toContainText(IDEA_BASIS_TASK_TITLE);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_returned_edit_sends_back', async ({ page }) => {
  const problems = watch(page);
  const idea = await returnedIdea('Идея E2E: дописать и отправить');
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('На доработку');
  const ask = card(page).getByTestId('review-missing');
  await expect(ask).toContainText('Агент просит дописать:');
  await expect(ask).toContainText(REVIEW_MISSING);
  // «Изменить» — главное действие; решить самому тоже можно, «Вернуть» нет
  const actions = card(page).getByRole('group', { name: 'Решение по идее' });
  await expect(actions.getByRole('button')).toHaveText(['Изменить', 'Начать', 'Отложить', 'Отказаться']);
  await expect(actions.getByRole('button', { name: 'Изменить' })).toHaveClass(/primary/);
  await actions.getByRole('button', { name: 'Изменить' }).click();
  const form = page.getByRole('form', { name: 'Правка идеи' });
  const body = form.getByLabel('Замысел');
  await body.fill(`${await body.inputValue()}\nСтатусы — все семь, группировка нужна.`);
  const saved = page.waitForResponse((r) => r.url().endsWith(`/entries/${idea.id}`) && r.request().method() === 'PATCH');
  await form.getByRole('button', { name: 'Сохранить' }).click();
  expect((await saved).status()).toBe(200);
  await expect(card(page).getByRole('status')).toHaveText('Отправлено на разбор.');
  await expect(card(page).locator('[data-status]')).toHaveText('Непросмотрено');
  await expect(card(page).getByTestId('review-missing')).toHaveCount(0);
  // разбор остался в карточке, но уже не текущий
  await expect(card(page).getByTestId('review-note')).toHaveText('После разбора статус изменился: Непросмотрено.');
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('new');
  expect(e.data.returned_at).toBeUndefined();
  expect(e.data.reviews).toHaveLength(1);
  expect(problems, problems.join('\n')).toEqual([]);
});

// Решение координатора (задача 8): возвращённую человек может решить сам (§4, §6) — «Начать».
test('ideas_returned_start_by_human', async ({ page }) => {
  const problems = watch(page);
  const idea = await returnedIdea('Идея E2E: начать возвращённую');
  await openIdeas(page, idea.id);
  const sent = page.waitForRequest((r) => r.url().endsWith(`/entries/${idea.id}/decide`));
  await card(page).getByRole('group', { name: 'Решение по идее' }).getByRole('button', { name: 'Начать' }).click();
  expect((await sent).postDataJSON()).toEqual({ decision: 'accept' });
  await expect(card(page).locator('[data-status]')).toHaveText('В плане');
  await expect(card(page).getByTestId('task-link')).toHaveText(/^Задача k:[0-9A-Z]{26}$/);
  // разбор «на доработку» остался в карточке; решение — человека
  await expect(card(page).getByTestId('review-note')).toHaveText('После разбора статус изменился: В плане.');
  const e = await api('GET', `entries/${idea.id}`);
  expect(e.status).toBe('accepted');
  expect(e.data.decided.by).toBe('human');
  expect(e.data.returned_at).toBeUndefined();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('ideas_declined_by_review_delete_to_trash', async ({ page }) => {
  const problems = watch(page);
  const idea = await newIdea('Идея E2E: удалить отклонённую');
  await api('POST', `entries/${idea.id}/review`, { outcome: 'decline', summary: reviewSummary('то, что не нужно') });
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('Отклонена');
  const actions = card(page).getByRole('group', { name: 'Решение по идее' });
  await expect(actions.getByRole('button')).toHaveText(['Вернуть на разбор', 'Удалить']);
  await expect(card(page).getByRole('region', { name: 'Разбор агента' })).toContainText('удалите идею или оставьте её');
  await actions.getByRole('button', { name: 'Удалить' }).click();
  const dlg = page.getByRole('dialog');
  await expect(dlg).toContainText('Удалить идею?');
  const del = page.waitForResponse((r) => r.url().endsWith(`/entries/${idea.id}`) && r.request().method() === 'DELETE');
  await dlg.getByRole('button', { name: 'Удалить' }).click();
  expect((await del).status()).toBeLessThan(300);
  await expect(card(page)).toHaveCount(0);
  await expect(page.getByRole('status')).toHaveText('Идея «Идея E2E: удалить отклонённую» перемещена в корзину.');
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/ideas$`));
  const r = await fetch(`${base()}api/v1/entries/${idea.id}`, { headers: { Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}` } });
  expect(r.status).toBe(404);
  expect(problems, problems.join('\n')).toEqual([]);
});

// Ревью задачи 8 (Minor 3): удаление с устаревшей ревизией (409) — как у остальных действий
// карточки: окно закрывается, лента и карточка перечитаны, ошибка — в карточке.
test('ideas_delete_stale_rev_rereads', async ({ page }) => {
  const problems = watch(page, [409]);
  const idea = await newIdea('Идея E2E: удалить устаревшую');
  await api('POST', `entries/${idea.id}/review`, { outcome: 'decline', summary: reviewSummary('то, что передумали') });
  await openIdeas(page, idea.id);
  await expect(card(page).locator('[data-status]')).toHaveText('Отклонена');
  // пока карточка открыта, человек в чате вернул идею на разбор (агент записал его слова)
  await decide(idea.id, 'reopen', { reason: 'передумал' });
  const actions = card(page).getByRole('group', { name: 'Решение по идее' });
  await actions.getByRole('button', { name: 'Удалить' }).click();
  const del = page.waitForResponse((r) => r.url().endsWith(`/entries/${idea.id}`) && r.request().method() === 'DELETE');
  await page.getByRole('dialog').getByRole('button', { name: 'Удалить' }).click();
  expect((await del).status()).toBe(409);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(card(page).locator('[data-status]')).toHaveText('Непросмотрено');
  await expect(card(page).getByRole('alert')).toBeVisible();
  await expect(actions.getByRole('button', { name: 'Удалить' })).toHaveCount(0);
  expect((await api('GET', `entries/${idea.id}`)).status).toBe('new');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tasks_from_idea_mark_and_filter', async ({ page }) => {
  const problems = watch(page);
  const task = env('E2E_IDEA_PLAN_TASK');
  const idea = env('E2E_IDEA_PLANNED');
  const u = await me(page);
  await page.goto(`${base()}#/u/${u}/s/${IDEAS_GUID}/j/tasks`);
  const board = page.getByRole('group', { name: 'Доска задач' });
  const item = board.locator(`[data-task="${task}"]`);
  // пометка «из идеи w:<id> «…»» со ссылкой на идею; у обычной задачи пометки нет
  const mark = item.getByTestId('from-idea');
  await expect(mark).toHaveText(`из идеи w:${idea} «${IDEA_PLANNED_TITLE}»`);
  await expect(mark).toHaveAttribute('href', new RegExp(`/s/${IDEAS_GUID}/j/ideas/${idea}$`));
  const plain = board.locator(`[data-task="${env('E2E_IDEA_BASIS_TASK')}"]`);
  await expect(plain).toBeVisible();
  await expect(plain.getByTestId('from-idea')).toHaveCount(0);
  // «Из идей» — только задачи из идей (на сервере)
  const req = page.waitForRequest((r) => r.url().includes('kind=task') && r.url().includes('from_idea=1'));
  const toggle = page.getByRole('button', { name: 'Из идей' });
  await toggle.click();
  await req;
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(plain).toHaveCount(0);
  await expect(item).toBeVisible();
  const n = await board.locator('[data-task]').count();
  await expect(board.locator('[data-task] [data-testid="from-idea"]')).toHaveCount(n);
  await page.screenshot({ path: `${screens}tasks-from-ideas.png`, fullPage: true });
  // карточка задачи — «Из идеи:» со ссылкой и статусом идеи
  await item.getByRole('button', { name: IDEA_PLANNED_TITLE }).click();
  const tc = page.getByRole('article', { name: 'Карточка задачи' });
  const ideas = tc.getByTestId('task-ideas');
  await expect(ideas).toContainText('Из идеи:');
  await expect(ideas).toContainText('В плане');
  await ideas.getByRole('link', { name: `w:${idea} «${IDEA_PLANNED_TITLE}»` }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${IDEAS_GUID}/j/ideas/${idea}$`));
  await expect(card(page)).toContainText(IDEA_PLANNED_TITLE);
  // снова все задачи
  await page.goto(`${base()}#/u/${u}/s/${IDEAS_GUID}/j/tasks`);
  await expect(page.getByRole('button', { name: 'Из идей' })).toHaveAttribute('aria-pressed', 'false');
  await expect(board.locator(`[data-task="${env('E2E_IDEA_BASIS_TASK')}"]`)).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('agent_setup_ideas_mode_persists', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.goto(`${base()}#/agent-setup`);
  const mode = page.getByRole('radiogroup', { name: 'Идеи: разбирать сам / предлагать мне' });
  await expect(mode.getByLabel('разбирать сам')).toBeChecked();
  await expect(mode.getByLabel('предлагать мне')).not.toBeChecked();
  try {
    const saved = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await mode.getByLabel('предлагать мне').check();
    const resp = await saved;
    expect(resp.request().postDataJSON()).toEqual({ ideas: { mode: 'propose' } });
    expect((await resp.json()).ideas).toEqual({ proactive: true, mode: 'propose' });
    await page.reload();
    await expect(mode.getByLabel('предлагать мне')).toBeChecked();
    const mine = await page.request.get(`${base()}api/v1/auth/settings`);
    expect((await mine.json()).ideas).toEqual({ proactive: true, mode: 'propose' });
    const back = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
    await mode.getByLabel('разбирать сам').check();
    expect((await (await back).json()).ideas.mode).toBe('review');
    await expect(mode.getByLabel('разбирать сам')).toBeChecked();
  } finally {
    // умолчание возвращается и при сбое: остальные сценарии (разбор, propose — 409) ждут review
    const { csrf } = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
    const r = await page.request.patch(`${base()}api/v1/auth/settings`, {
      headers: { 'X-CSRF-Token': csrf },
      data: { ideas: { mode: 'review' } },
    });
    expect(r.status()).toBe(200);
  }
  expect(problems, problems.join('\n')).toEqual([]);
});
