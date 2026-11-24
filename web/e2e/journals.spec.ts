// Сквозные сценарии журналов решения в WebUI (M3): Changelog с фильтрами, сравнение релизов,
// замещение insight, повышение до theorem с доказательством, раскрытие секрета Vault,
// вкладка «Связи» с обратной ссылкой, карточки записей в поиске Ctrl+K.
// Данные — seedJournals (global-setup). Снимки — в .tmp/e2e/screens/ (под .gitignore).
import { expect, test, type Browser, type Locator, type Page } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { loginChangingPassword } from './login';
import { pageCtx } from './mobile-pages';
import { ADMIN_STATE } from './session';
import { SOLUTION_GUID, SOLUTION_NAME, VAULT_SECRET } from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));

test.use({ storageState: ADMIN_STATE });

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

const tree = (page: Page) => page.locator('nav[aria-label="Дерево заметок"]');

/** Открывает журнал решения E2E через дерево. */
async function openJournal(page: Page, journal: string) {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  const t = tree(page);
  await t.getByRole('button', { name: /^Solutions/ }).first().click();
  const sol = t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).first();
  await expect(sol).toContainText('владелец');
  await sol.click();
  // журналы — над деревом заметок решения
  const journals = t.getByRole('list', { name: `Журналы: ${SOLUTION_NAME}` });
  await expect(journals.getByRole('link')).toHaveText(['Changelog', 'Releases', 'Отчёты', 'Код', 'Insights', 'Словарь', 'Задачи', 'Идеи', 'Вопросы', 'Vault']);
  await journals.getByRole('link', { name: journal, exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/${journal.toLowerCase()}`));
  await expect(page.getByRole('heading', { level: 1, name: journal })).toBeVisible();
}

test('Changelog: лента и фильтры по виду и области', async ({ page }) => {
  const problems = watch(page);
  await openJournal(page, 'Changelog');
  const feed = page.getByRole('feed', { name: 'Лента changelog' });
  const cards = feed.locator('article');
  await expect(cards).toHaveCount(3);
  // новые — первыми
  await expect(cards.first()).toContainText('Описан формат журналов');
  await page.screenshot({ path: `${screens}changelog.png`, fullPage: true });

  // вид фильтрует сервер (data_kind), а не клиент по загруженной странице
  const filtered = page.waitForRequest((r) => r.url().includes('data_kind=fix'));
  await page.getByLabel('Вид записи').selectOption('fix');
  await filtered;
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText('Исправлен CRLF в хэше');

  await page.getByLabel('Вид записи').selectOption('');
  await page.getByLabel('Область').fill('web');
  await page.getByRole('button', { name: 'Применить' }).click();
  await expect(cards).toHaveCount(2);
  await expect(feed).not.toContainText('Исправлен CRLF');

  // Knowledge: без Changelog и Releases
  const t = tree(page);
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await expect(t.getByRole('list', { name: 'Журналы: Knowledge' }).getByRole('link')).toHaveText([
    'Insights',
    'Словарь',
    'Задачи',
    'Идеи',
    'Вопросы',
    'Vault',
  ]);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Releases: список и сравнение с предыдущим', async ({ page }) => {
  const problems = watch(page);
  await openJournal(page, 'Releases');
  const list = page.getByRole('list', { name: 'Релизы' });
  await expect(list.getByRole('button')).toHaveCount(2);
  await expect(page.getByRole('heading', { name: 'Релиз 0.2.0' })).toBeVisible();
  await expect(page.getByText('Интервал: 0.1.0 → 0.2.0')).toBeVisible();
  // в интервале — fix и docs, feat — до 0.1.0
  await expect(page.locator('[data-group="fix"]')).toContainText('Исправлен CRLF в хэше');
  await expect(page.locator('[data-group="docs"]')).toContainText('Описан формат журналов');
  await expect(page.locator('[data-group="feat"]')).toHaveCount(0);
  // дифф текста релиза и коммиты
  await expect(page.locator('.diff .add').filter({ hasText: 'Исправления хранилища.' })).toHaveCount(1);
  await expect(page.locator('.diff .del').filter({ hasText: 'Экспорт заметок.' })).toHaveCount(1);
  await expect(page.locator('.commits')).toContainText('a1b2c3d4e5');
  await expect(page.locator('.commits')).toContainText('fix(store): CRLF');
  await page.screenshot({ path: `${screens}releases-compare.png`, fullPage: true });

  // первый релиз — от начала журнала
  await list.getByRole('button', { name: /0\.1\.0/ }).click();
  await expect(page.getByRole('heading', { name: 'Релиз 0.1.0' })).toBeVisible();
  await expect(page.getByText('Интервал: начало журнала → 0.1.0')).toBeVisible();
  await expect(page.locator('[data-group="feat"]')).toContainText('Добавлен экспорт в Markdown');

  // «Сравнить с» другим релизом: 0.1.0 от 0.2.0 — интервал пуст
  await page.getByLabel('Сравнить с').selectOption({ label: '0.2.0' });
  await expect(page.getByText('Интервал: 0.2.0 → 0.1.0')).toBeVisible();
  await expect(page.getByText('Записей changelog в интервале нет.')).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Insights: замена записи и повышение до theorem с доказательством', async ({ page }) => {
  const problems = watch(page);
  await openJournal(page, 'Insights');
  const table = page.getByRole('table', { name: 'Insights' });
  await expect(table.locator('tbody tr')).toHaveCount(3);
  await expect(table.locator('[data-certainty="theorem"]')).toHaveCount(1);
  await expect(table.locator('[data-certainty="hypothesis"]')).toHaveCount(2);

  // фильтр по надёжности — на сервере
  await page.getByLabel('Надёжность', { exact: true }).selectOption('theorem');
  await expect(table.locator('tbody tr')).toHaveCount(1);
  await page.getByLabel('Надёжность', { exact: true }).selectOption('');
  await expect(table.locator('tbody tr')).toHaveCount(3);

  // Замена
  await table.getByRole('button', { name: 'Старый вывод о Qdrant' }).click();
  const card = page.getByRole('article', { name: 'Карточка insight' });
  await expect(card.getByRole('heading', { name: 'Старый вывод о Qdrant' })).toBeVisible();
  await card.getByRole('button', { name: 'Заменить' }).click();
  // формы карточки — окна (M7.3, §6.14 правило Б)
  const form = page.getByRole('dialog', { name: 'Заменить запись' });
  // новая запись человека — без статуса по умолчанию (аксиому человек ставит явно)
  await expect(form.getByLabel('Надёжность новой записи')).toHaveValue('');
  await form.getByLabel('Заголовок').fill('Новый вывод о Qdrant');
  await form.getByRole('button', { name: 'Заменить' }).click();
  await expect(card.getByRole('heading', { name: 'Новый вывод о Qdrant' })).toBeVisible();
  await expect(card.getByRole('status')).toHaveText('Запись заменена: прежняя помечена superseded.');
  await expect(page).toHaveURL(/\/j\/insights\/[0-9A-Za-z]+$/);
  await expect(page).not.toHaveURL(new RegExp(env('E2E_INSIGHT_OLD')));
  const chain = card.locator('.chain');
  await expect(chain).toContainText('Старый вывод о Qdrant');
  await expect(chain).toContainText('заменён');
  await expect(table.locator('tbody tr')).toHaveCount(4);
  await expect(table.locator('tr', { hasText: 'Старый вывод о Qdrant' })).toContainText('заменён');

  // Повышение гипотезы до theorem: человеку причина и доказательство необязательны (R8)
  await table.getByRole('button', { name: 'Кэш эмбеддингов ускоряет переиндексацию' }).click();
  await expect(page).toHaveURL(new RegExp(`/j/insights/${env('E2E_INSIGHT_HYP')}$`));
  await card.getByRole('button', { name: 'Сменить статус надёжности' }).click();
  const cf = page.getByRole('dialog', { name: 'Статус надёжности' });
  await cf.getByLabel('theorem').check();
  await cf.getByRole('button', { name: 'Сохранить' }).click();
  await expect(card.getByRole('status')).toHaveText('Статус надёжности изменён.');
  await expect(card.locator('header [data-certainty="theorem"]')).toBeVisible();
  await expect(card.locator('.evidence')).toHaveCount(0);
  // доказательство можно дописать позже той же формой
  await card.getByRole('button', { name: 'Сменить статус надёжности' }).click();
  await expect(cf.getByLabel('theorem')).toBeChecked();
  await cf.getByLabel(/^Причина/).fill('замерено на реальной базе');
  await cf.getByLabel('Проверено').fill('время переиндексации 10 тыс. заметок');
  await cf.getByLabel('Метод').fill('reindex без full и с full');
  await cf.getByLabel('Результат').fill('в 8 раз быстрее из кэша');
  await page.screenshot({ path: `${screens}insight-theorem-form.png`, fullPage: true });
  await cf.getByRole('button', { name: 'Сохранить' }).click();
  await expect(card.getByRole('status')).toHaveText('Статус надёжности изменён.');
  await expect(card.locator('header [data-certainty="theorem"]')).toBeVisible();
  await expect(card.locator('.evidence')).toContainText('в 8 раз быстрее из кэша');
  await expect(table.locator('[data-certainty="theorem"]')).toHaveCount(2);

  // Администратор — человек: axiom доступна, причина необязательна
  await card.getByRole('button', { name: 'Сменить статус надёжности' }).click();
  await cf.getByLabel('axiom').check();
  await cf.getByRole('button', { name: 'Сохранить' }).click();
  await expect(card.locator('header [data-certainty="axiom"]')).toBeVisible();
  // доказательство не удаляется, но подписано как прежнее
  await expect(card.getByText('Прежнее доказательство')).toBeVisible();
  await expect(card.locator('.evidence')).toContainText('в 8 раз быстрее из кэша');
  await page.screenshot({ path: `${screens}insights.png`, fullPage: true });

  // Пересмотр и возврат
  await card.getByRole('button', { name: 'Пересмотреть' }).click();
  await expect(card.getByRole('status')).toHaveText('Запись отправлена на пересмотр.');
  await expect(card.locator('header .status')).toHaveText('на пересмотре');
  await card.getByRole('button', { name: 'Вернуть в действующие' }).click();
  await expect(card.getByRole('status')).toHaveText('Запись снова действует.');
  await expect(card.locator('header .status')).toHaveText('действует');

  // Триггеры: добавить, сохранить, удалить
  await card.getByRole('button', { name: 'Триггеры' }).click();
  const tr = page.getByRole('dialog', { name: 'Триггеры' });
  await tr.getByLabel('Тип триггера').selectOption('file_glob');
  await tr.getByLabel('Шаблон триггера').fill('**/qdrant/*.rs');
  await tr.getByLabel('Инструменты').fill('Write, Edit');
  await tr.getByRole('button', { name: 'Добавить' }).click();
  await tr.getByRole('button', { name: 'Сохранить триггеры' }).click();
  await expect(card.getByRole('status')).toHaveText('Триггеры сохранены.');
  const saved = await page.request.get(`${base()}api/v1/entries/${env('E2E_INSIGHT_HYP')}`);
  expect((await saved.json()).triggers).toMatchObject([
    { type: 'file_glob', pattern: '**/qdrant/*.rs', tools: ['Write', 'Edit'] },
  ]);
  await card.getByRole('button', { name: 'Триггеры' }).click();
  await expect(tr.locator('.triggers li')).toHaveCount(1);
  await tr.getByRole('button', { name: 'Удалить триггер **/qdrant/*.rs' }).click();
  await tr.getByRole('button', { name: 'Сохранить триггеры' }).click();
  await expect(card.getByRole('status')).toHaveText('Триггеры сохранены.');
  const cleared = await page.request.get(`${base()}api/v1/entries/${env('E2E_INSIGHT_HYP')}`);
  expect((await cleared.json()).triggers).toEqual([]);

  // Отзыв заменившей записи: причина обязательна, запись остаётся в истории
  await table.getByRole('button', { name: 'Новый вывод о Qdrant' }).click();
  await expect(card.getByRole('heading', { name: 'Новый вывод о Qdrant' })).toBeVisible();
  await card.getByRole('button', { name: 'Отозвать' }).click();
  const rf = page.getByRole('dialog', { name: 'Отозвать запись' });
  await rf.getByRole('button', { name: 'Отозвать' }).click();
  await expect(rf.getByRole('alert')).toHaveText('Укажите причину отзыва.');
  await rf.getByLabel('Причина').fill('Qdrant заменён в задаче 4');
  await rf.getByRole('button', { name: 'Отозвать' }).click();
  await expect(card.getByRole('status')).toHaveText('Запись отозвана.');
  await expect(card.locator('header .status')).toHaveText('отозван');
  await expect(card).toContainText('Отозвано: Qdrant заменён в задаче 4');
  await expect(card.getByRole('button', { name: 'Заменить' })).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Vault: маска, раскрытие по «Показать», скрытие', async ({ page }) => {
  const problems = watch(page);
  await openJournal(page, 'Vault');
  const item = page.locator('li.item', { hasText: 'E2E токен деплоя' });
  await expect(item).toContainText('deploy-bot');
  await expect(item.getByLabel('Значение скрыто')).toBeVisible();
  await expect(page.getByText('каждое раскрытие записывается в аудит', { exact: false })).toBeVisible();
  // значения нет на странице до раскрытия
  expect(await page.content()).not.toContain(VAULT_SECRET);

  const reveal = page.waitForResponse((r) => r.url().includes('reveal=1'));
  await item.getByRole('button', { name: 'Показать' }).click();
  const resp = await reveal;
  expect(resp.headers()['cache-control']).toBe('no-store');
  await expect(item.getByLabel('Значение E2E токен деплоя')).toHaveValue(VAULT_SECRET);
  await page.screenshot({ path: `${screens}vault-revealed.png` });
  await item.getByRole('button', { name: 'Скрыть' }).click();
  await expect(item.getByLabel('Значение скрыто')).toBeVisible();
  expect(await page.content()).not.toContain(VAULT_SECRET);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('журнал по прямому адресу у Rules: понятное сообщение', async ({ page }) => {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  const user = new URL(page.url()).hash.split('/')[2];
  await page.goto(`${base()}#/u/${user}/s/rules/j/insights`);
  await expect(page.getByRole('alert')).toContainText('У пространства «Rules» нет журнала Insights');
  await expect(page.getByRole('table', { name: 'Insights' })).toHaveCount(0);
});

test('Vault: заблокирован — список виден, «Показать» неактивна, баннер', async ({ page }) => {
  // ответ сервера без мастер-ключа подменяется: сам сервер E2E с ключом
  const reason = 'Vault заблокирован: мастер-ключ не подходит к ключам пользователей';
  await page.route(`**/api/v1/spaces/${SOLUTION_GUID}/vault`, (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      json: {
        locked: true,
        reason,
        items: [
          {
            id: '01HZZZZZZZZZZZZZZZZZZZZZZZ',
            space: SOLUTION_GUID,
            agent_solution_id: SOLUTION_GUID,
            title: 'E2E заблокированный',
            data: { type: 'token', username: 'deploy-bot' },
            rev: 1,
            author: { kind: 'agent' },
            created_at: '2026-10-26T10:00:00Z',
            updated_at: '2026-10-26T10:00:00Z',
          },
        ],
      },
    }),
  );
  await openJournal(page, 'Vault');
  const banner = page.locator('.locked');
  await expect(banner).toContainText('Vault заблокирован: мастер-ключ не подходит');
  await expect(banner).toContainText('ZAM_MASTER_KEY');
  const item = page.locator('li.item', { hasText: 'E2E заблокированный' });
  await expect(item).toContainText('deploy-bot');
  await expect(item.getByRole('button', { name: 'Показать' })).toBeDisabled();
});

test('Vault: 503 при раскрытии — баннер vault-locked', async ({ page }) => {
  await page.route('**/api/v1/vault/*', (route) =>
    route.fulfill({
      status: 503,
      contentType: 'application/problem+json',
      json: {
        type: '/problems/vault-locked',
        title: 'Service Unavailable',
        status: 503,
        detail: 'Vault заблокирован: файл мастер-ключа не найден',
      },
    }),
  );
  await openJournal(page, 'Vault');
  const item = page.locator('li.item', { hasText: 'E2E токен деплоя' });
  await item.getByRole('button', { name: 'Показать' }).click();
  await expect(page.locator('.locked')).toContainText('Vault заблокирован: файл мастер-ключа');
  await expect(item.getByRole('button', { name: 'Показать' })).toBeDisabled();
});

test('Связи: обратная ссылка и неразрешённая исходящая', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  const t = tree(page);
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await t.getByRole('button', { name: 'E2E', exact: true }).click();
  await t.getByRole('link', { name: 'Вторая заметка' }).click();
  await page.getByRole('tab', { name: 'Связи' }).click();
  const back = page.getByRole('region', { name: 'Обратные ссылки' });
  await expect(back).toContainText('E2E/Медиа-заметка');
  await page.screenshot({ path: `${screens}links.png`, fullPage: true });

  // переход по обратной ссылке; у Медиа-заметки — исходящие, в том числе неразрешённая
  await back.getByRole('button', { name: 'E2E/Медиа-заметка' }).click();
  await expect(page).toHaveURL(new RegExp(`/n/${env('E2E_NOTE_MEDIA')}$`));
  await page.getByRole('tab', { name: 'Связи' }).click();
  const out = page.getByRole('region', { name: 'Исходящие связи' });
  await expect(out.getByRole('button', { name: 'E2E/Вторая заметка' })).toBeVisible();
  await expect(out.locator('.unresolved')).toContainText('Нет такой заметки');
  await expect(out).toContainText('не найдено');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Поиск Ctrl+K: карточка записи со статусом и надёжностью', async ({ page }) => {
  const problems = watch(page);
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  await page.keyboard.press('Control+k');
  const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
  await search.fill('CRLF');
  const card = page.getByRole('option').filter({ hasText: 'CRLF ломает хэш содержимого' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('insight/gotcha');
  await expect(card).toContainText('действует');
  await expect(card.locator('[data-certainty="theorem"]')).toBeVisible();
  await page.screenshot({ path: `${screens}search-entry.png` });
  await card.click();
  await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/insights/[0-9A-Za-z]+$`));
  await expect(
    page.getByRole('article', { name: 'Карточка insight' }).getByRole('heading', { name: 'CRLF ломает хэш содержимого' }),
  ).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

// ---- числа разделов (M7.2, спецификация §5.3; задача 4) ----

interface SpaceCounts {
  journals: Record<string, number>;
  open: { questions: number; tasks: number };
  notes: number;
}

/** Журналы решения в порядке дерева и полосы: ключ ответа `counts` и прежнее имя ссылки. */
const JOURNALS: [string, string][] = [
  ['changelog', 'Changelog'],
  ['releases', 'Releases'],
  ['reports', 'Отчёты'],
  ['code', 'Код'],
  ['insights', 'Insights'],
  ['glossary', 'Словарь'],
  ['tasks', 'Задачи'],
  ['ideas', 'Идеи'],
  ['questions', 'Вопросы'],
  ['vault', 'Vault'],
];

/** Ожидаемые подписи — своим кодом, независимо от `counts.ts` (его формы — `counts.test.ts`). */
const fmt = (n: number) => new Intl.NumberFormat('ru-RU').format(n);
function forms(n: number, one: string, few: string, many: string) {
  const d = n % 10;
  const h = n % 100;
  return d === 1 && h !== 11 ? one : d >= 2 && d <= 4 && (h < 12 || h > 14) ? few : many;
}
const description = (key: string, n: number, open: number) =>
  `${fmt(n)} ${key === 'code' ? forms(n, 'файл', 'файла', 'файлов') : forms(n, 'запись', 'записи', 'записей')}` +
  (open > 0 ? `, открыто ${fmt(open)}` : '');

/** Числа решения E2E у сессии администратора (с отчётами — как у дерева). */
async function solutionCounts(page: Page): Promise<SpaceCounts> {
  const r = await page.request.get(`${base()}api/v1/spaces/${SOLUTION_GUID}/counts`);
  expect(r.status(), await r.text()).toBe(200);
  return (await r.json()) as SpaceCounts;
}

/** REST по токену администратора (агент). */
async function agentApi<T>(method: string, path: string, body?: unknown, extra: Record<string, string> = {}) {
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
  return (r.status === 204 ? undefined : await r.json()) as T;
}

test('tree_and_tabs_show_counts', async ({ page }) => {
  const problems = watch(page);
  // открытый вопрос решения — для «открыто N» у «Вопросов» (убирается в конце)
  const q = await agentApi<{ id: string; rev: number }>('POST', `spaces/${SOLUTION_GUID}/entries`, {
    kind: 'question',
    title: 'Вопрос E2E: числа разделов',
    body: 'Ждёт ответа человека.',
  });
  let ideaId = '';
  try {
    const c = await solutionCounts(page);
    expect(c.open.questions).toBeGreaterThanOrEqual(1);
    await page.goto(base());
    await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
    const t = tree(page);
    await t.getByRole('button', { name: /^Solutions/ }).first().click();
    const sol = t.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) }).first();
    await sol.click();
    const journals = t.getByRole('list', { name: `Журналы: ${SOLUTION_NAME}` });
    // имена ссылок прежние: число — отдельный элемент вне ссылки (пре-флайт N3)
    await expect(journals.getByRole('link')).toHaveText(JOURNALS.map(([, title]) => title));
    const treeNum = (key: string) => journals.locator(`[data-count="${key}"]`);
    for (const [key, title] of JOURNALS) {
      const n = c.journals[key];
      const open = key === 'questions' ? c.open.questions : 0;
      await expect(treeNum(key), key).toHaveText(fmt(n));
      await expect(journals.getByRole('link', { name: title, exact: true })).toHaveAccessibleDescription(
        description(key, n, open),
      );
    }
    // «Вопросы» с открытым — акцентная метка; у пространства — число заметок (описание кнопки)
    const qRow = journals.getByRole('listitem').filter({ has: page.getByRole('link', { name: 'Вопросы', exact: true }) });
    await expect(qRow.getByText(`открыто ${fmt(c.open.questions)}`, { exact: true })).toBeVisible();
    const solLabel = page.getByRole('button', { name: new RegExp(`^${SOLUTION_NAME}`) });
    const notes = t.locator('.tree-row.space').filter({ has: solLabel }).locator('[data-count="notes"]');
    await expect(notes).toHaveText(fmt(c.notes));
    await expect(sol).toHaveAccessibleDescription(new RegExp(`^${fmt(c.notes)} замет(ка|ки|ок)$`));

    // полоса журналов — те же числа, имена ссылок прежние
    await journals.getByRole('link', { name: 'Идеи', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/s/${SOLUTION_GUID}/j/ideas$`));
    const strip = page.getByRole('navigation', { name: 'Журналы пространства' });
    await expect(strip.getByRole('link')).toHaveText(JOURNALS.map(([, title]) => title));
    const stripNum = (key: string) => strip.locator(`[data-count="${key}"]`);
    for (const [key, title] of JOURNALS) {
      const open = key === 'questions' ? c.open.questions : 0;
      await expect(stripNum(key), key).toHaveText(fmt(c.journals[key]));
      await expect(strip.getByRole('link', { name: title, exact: true })).toHaveAccessibleDescription(
        description(key, c.journals[key], open),
      );
    }
    await expect(strip.getByText(`открыто ${fmt(c.open.questions)}`, { exact: true })).toBeVisible();
    // число и «открыто N» — поверх правого поля ссылки: внутри её рамки, перенос строки их не
    // разделяет (ревью r1, M3)
    const inside = async (title: string, part: Locator) => {
      const [lb, nb] = await Promise.all([strip.getByRole('link', { name: title, exact: true }).boundingBox(), part.boundingBox()]);
      return !!lb && !!nb && nb.x >= lb.x - 0.5 && nb.x + nb.width <= lb.x + lb.width + 0.5 && nb.y >= lb.y - 0.5;
    };
    expect(await inside('Идеи', stripNum('ideas')), 'число внутри ссылки').toBe(true);
    expect(await inside('Вопросы', strip.getByText(`открыто ${fmt(c.open.questions)}`, { exact: true })), 'метка внутри ссылки').toBe(true);
    // ноль — цветом `--muted` без прозрачности (контраст WCAG AA, ревью r1 M2); в дереве — кроме
    // выбранной строки «Идеи» под указателем: там все числа цветом текста
    for (const [key] of JOURNALS.filter(([k]) => c.journals[k] === 0)) {
      for (const el of key === 'ideas' ? [stripNum(key)] : [treeNum(key), stripNum(key)]) {
        const [color, muted, opacity] = await el.evaluate((e) => {
          const probe = document.createElement('span');
          probe.style.color = 'var(--muted)';
          e.parentElement!.append(probe);
          const m = getComputedStyle(probe).color;
          probe.remove();
          return [getComputedStyle(e).color, m, getComputedStyle(e).opacity];
        });
        expect([color, opacity], key).toEqual([muted, '1']);
      }
    }
    await page.screenshot({ path: `${screens}journal-counts.png` });

    // новая идея из WebUI — «Идеи» +1 в дереве и в полосе без перезагрузки
    await page.getByRole('button', { name: 'Новая идея' }).click();
    // «Новая идея» — окно (M7.3, §6.14 правило Б), после «Записать» — карточка созданной
    const form = page.getByRole('dialog', { name: 'Новая идея' });
    await form.getByLabel('Заголовок').fill('Идея E2E: число раздела');
    await form.getByLabel('Замысел').fill('Проверка числа записей раздела.');
    const created = page.waitForResponse((r) => r.request().method() === 'POST' && r.url().endsWith(`/spaces/${SOLUTION_GUID}/entries`));
    await form.getByRole('button', { name: 'Записать' }).click();
    ideaId = ((await (await created).json()) as { id: string }).id;
    await expect(page.getByRole('status')).toHaveText('Идея записана.');
    const ideas = c.journals.ideas + 1;
    await expect(stripNum('ideas')).toHaveText(fmt(ideas));
    await expect(treeNum('ideas')).toHaveText(fmt(ideas));
    await expect(journals.getByRole('link', { name: 'Идеи', exact: true })).toHaveAccessibleDescription(
      description('ideas', ideas, 0),
    );
    expect((await solutionCounts(page)).journals.ideas).toBe(ideas);
    expect(problems, problems.join('\n')).toEqual([]);
  } finally {
    // идею человека в корзину убирает человек (сессия), вопрос агента — его токен
    if (ideaId) {
      const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
      const cur = (await (await page.request.get(`${base()}api/v1/entries/${ideaId}`)).json()) as { rev: number };
      const del = await page.request.delete(`${base()}api/v1/entries/${ideaId}`, {
        headers: { 'X-CSRF-Token': me.csrf, 'If-Match': `"${cur.rev}"` },
      });
      expect(del.status()).toBe(204);
    }
    await agentApi('DELETE', `entries/${q.id}`, undefined, { 'If-Match': `"${q.rev}"` });
  }
});

// ---- правка журналов и корзина (M7.2, спецификация §5.9; задача 10) ----

/** Новое решение администратора для сценария с точными числами (в решении E2E — записи сида и
 * соседних спецификаций); возвращает его GUID. */
async function newSolution(name: string): Promise<string> {
  const guid = randomUUID();
  await agentApi('POST', 'solutions', { agent_solution_id: guid, name: `Журналы E2E: ${name}` });
  return guid;
}

interface Rec {
  id: string;
  rev: number;
  title: string;
  body: string;
  status: string | null;
  data: Record<string, unknown>;
}

test('changelog_edit_and_trash_restore', async ({ page }) => {
  const problems = watch(page);
  const guid = await newSolution('правка changelog');
  await agentApi('POST', `spaces/${guid}/entries`, {
    kind: 'changelog',
    title: 'Соседняя запись',
    body: 'Остаётся в ленте.',
    data: { kind: 'docs' },
  });
  const e = await agentApi<Rec>('POST', `spaces/${guid}/entries`, {
    kind: 'changelog',
    title: 'Запись для правки',
    body: 'Текст до правки.',
    data: { kind: 'fix', refs: { commits: ['abc1234'] } },
  });
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${guid}/j/changelog`);
  const journal = page.locator('section.journal');
  const feed = page.getByRole('feed', { name: 'Лента changelog' });
  const card = feed.locator(`[data-entry="${e.id}"]`);
  await expect(card.getByRole('heading')).toHaveText('Запись для правки');
  // число раздела в полосе журналов — 2 (задача 4): правка его не меняет, корзина — да
  const num = page.getByRole('navigation', { name: 'Журналы пространства' }).locator('[data-count="changelog"]');
  await expect(num).toHaveText('2');

  // «Изменить»: заголовок и вид — одним PATCH с ревизией карточки, только изменённое
  await card.getByRole('button', { name: 'Изменить «Запись для правки»' }).click();
  const form = card.getByRole('form', { name: 'Правка записи' });
  await expect(form.getByLabel('Заголовок')).toBeFocused();
  await expect(form.getByLabel('Вид')).toHaveValue('fix');
  await form.getByLabel('Заголовок').fill('Запись после правки');
  await form.getByLabel('Вид').selectOption('feat');
  const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().endsWith(`/entries/${e.id}`));
  await form.getByRole('button', { name: 'Сохранить' }).click();
  const resp = await saved;
  expect(resp.status()).toBe(200);
  expect(resp.request().headers()['if-match']).toBe(`"${e.rev}"`);
  // `data` changelog сервер заменяет целиком: `refs` уходят вместе с новым видом
  expect(resp.request().postDataJSON()).toEqual({
    title: 'Запись после правки',
    data: { kind: 'feat', refs: { commits: ['abc1234'] } },
  });
  await expect(journal.getByRole('status')).toHaveText('Запись «Запись после правки» сохранена.');
  await expect(form).toHaveCount(0);
  await expect(card.getByRole('heading')).toHaveText('Запись после правки');
  await expect(card.locator('.kind')).toHaveText('feat');
  await expect(card).toContainText('Коммиты:');
  const after = await agentApi<Rec>('GET', `entries/${e.id}`);
  expect([after.title, after.body, after.data]).toEqual([
    'Запись после правки',
    'Текст до правки.',
    { kind: 'feat', refs: { commits: ['abc1234'] } },
  ]);
  await expect(num).toHaveText('2');

  // «Удалить» — с подтверждением, в корзину: записи нет в ленте, число раздела — 1
  await card.getByRole('button', { name: 'Удалить «Запись после правки»' }).click();
  const ask = page.getByRole('dialog', { name: 'Удалить запись?' });
  await expect(ask).toContainText('«Запись после правки» — в корзину журнала');
  await ask.getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(ask).toHaveCount(0);
  await expect(journal.getByRole('status')).toHaveText('Запись «Запись после правки» перемещена в корзину.');
  await expect(card).toHaveCount(0);
  await expect(feed.locator('article')).toHaveCount(1);
  await expect(num).toHaveText('1');

  // «Корзина» — удалённая запись (`deleted=only` вида), «Восстановить» — снова в ленте
  const trashReq = page.waitForRequest((r) => new URL(r.url()).searchParams.get('deleted') === 'only');
  const toggle = journal.getByRole('button', { name: 'Корзина', exact: true });
  await toggle.click();
  const q = new URL((await trashReq).url());
  expect([q.pathname.endsWith(`/spaces/${guid}/entries`), q.searchParams.get('kind')]).toEqual([true, 'changelog']);
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(feed).toHaveCount(0);
  const trash = page.getByRole('region', { name: 'Корзина' });
  const item = trash.locator(`[data-trash="${e.id}"]`);
  await expect(trash.locator('[data-trash]')).toHaveCount(1);
  await expect(item.getByRole('heading')).toHaveText('Запись после правки');
  await expect(item).toContainText('удалено');
  await page.screenshot({ path: `${screens}changelog-trash.png`, fullPage: true });
  await item.getByRole('button', { name: 'Восстановить «Запись после правки»' }).click();
  await expect(trash.getByRole('status')).toHaveText('Восстановлено: «Запись после правки» — снова в журнале.');
  await expect(trash.locator('[data-trash]')).toHaveCount(0);
  await expect(trash).toContainText('Корзина пуста.');
  await expect(num).toHaveText('2');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  await expect(feed.locator(`[data-entry="${e.id}"]`).getByRole('heading')).toHaveText('Запись после правки');
  await expect(feed.locator('article')).toHaveCount(2);
  expect((await agentApi<Rec & { deleted_at: string | null }>('GET', `entries/${e.id}`)).deleted_at).toBeNull();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('entry_edit_stale_rev_rereads', async ({ page }) => {
  const problems = watch(page);
  const guid = await newSolution('устаревшая ревизия');
  const e = await agentApi<Rec>('POST', `spaces/${guid}/entries`, {
    kind: 'changelog',
    title: 'Запись под правкой',
    body: 'Исходный текст.',
    data: { kind: 'fix' },
  });
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${guid}/j/changelog`);
  const card = page.getByRole('feed', { name: 'Лента changelog' }).locator(`[data-entry="${e.id}"]`);
  await card.getByRole('button', { name: 'Изменить «Запись под правкой»' }).click();
  const form = card.getByRole('form', { name: 'Правка записи' });
  await expect(form.getByLabel('Текст')).toHaveValue('Исходный текст.');
  // пока форма открыта, запись правит агент (другая сессия): текст и вид
  const other = await agentApi<Rec>('PATCH', `entries/${e.id}`, { body: 'Текст агента.', data: { kind: 'docs' } }, {
    'If-Match': `"${e.rev}"`,
  });
  expect(other.rev).toBe(e.rev + 1);
  await form.getByLabel('Заголовок').fill('Заголовок человека');
  const stale = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().endsWith(`/entries/${e.id}`));
  await form.getByRole('button', { name: 'Сохранить' }).click();
  expect((await stale).status()).toBe(409);
  // запись перечитана: ошибка видна, карточка — новой ревизии, нетронутые поля формы — агента,
  // набранный заголовок — человека
  // текст 409 — один у всех форм с ревизией (M7.3, §6.14 п. 5)
  await expect(form.getByRole('alert')).toContainText(`Запись изменилась, пока вы правили (сейчас ревизия ${other.rev})`);
  await expect(card).toContainText(`ревизия ${other.rev}`);
  await expect(card).toContainText('Текст агента.');
  await expect(form.getByLabel('Текст')).toHaveValue('Текст агента.');
  await expect(form.getByLabel('Вид')).toHaveValue('docs');
  await expect(form.getByLabel('Заголовок')).toHaveValue('Заголовок человека');
  // повтор — с перечитанной ревизией, только заголовок: правка агента не откатывается
  const retry = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().endsWith(`/entries/${e.id}`));
  await form.getByRole('button', { name: 'Сохранить' }).click();
  const ok = await retry;
  expect(ok.status()).toBe(200);
  expect(ok.request().headers()['if-match']).toBe(`"${other.rev}"`);
  expect(ok.request().postDataJSON()).toEqual({ title: 'Заголовок человека' });
  await expect(form).toHaveCount(0);
  const final = await agentApi<Rec>('GET', `entries/${e.id}`);
  expect([final.title, final.body, final.data.kind]).toEqual(['Заголовок человека', 'Текст агента.', 'docs']);
  // 409 — ожидаемый ответ сценария
  const expected = `HTTP 409 PATCH /api/v1/entries/${e.id}`;
  expect(problems.filter((p) => p !== expected), problems.join('\n')).toEqual([]);
});

// ---- ревью задачи 10, круг 1 (M3, M5): релизы, корзины прочих журналов, участник `ro` ----

/** Вопрос перед уходом из формы правки с несохранённым (`LEAVE_EDIT`, `web/src/lib/leaveEdit.ts`). */
const LEAVE_EDIT = 'В форме есть несохранённый текст. Закрыть её без сохранения?';

/** Корзина журнала: переключатель на странице и область корзины. */
const trashToggle = (page: Page) => page.locator('section.journal').getByRole('button', { name: 'Корзина', exact: true });
const trashRegion = (page: Page) => page.getByRole('region', { name: 'Корзина' });

/** Корзина журнала: удалённая запись `id` есть, «Восстановить» возвращает её; корзина пуста. */
async function restoreFromTrash(page: Page, id: string, title: string) {
  await trashToggle(page).click();
  const trash = trashRegion(page);
  await expect(trash.locator(`[data-trash="${id}"]`).getByRole('heading')).toHaveText(title);
  await trash.getByRole('button', { name: `Восстановить «${title}»` }).click();
  await expect(trash.getByRole('status')).toHaveText(`Восстановлено: «${title}» — снова в журнале.`);
  await expect(trash).toContainText('Корзина пуста.');
  // строки нет — фокус на итоге корзины
  await expect(trash.getByRole('status')).toBeFocused();
  await trashToggle(page).click();
  await expect(trashToggle(page)).toHaveAttribute('aria-pressed', 'false');
  expect((await agentApi<{ deleted_at: string | null }>('GET', `entries/${id}`)).deleted_at).toBeNull();
}

test('releases_edit_and_trash_restore', async ({ page }) => {
  const problems = watch(page);
  const guid = await newSolution('правка релиза');
  const rel = (version: string, notes: string) =>
    agentApi<Rec>('POST', `spaces/${guid}/entries`, {
      kind: 'release',
      title: `Релиз ${version}`,
      body: notes,
      data: { version, commits: [{ sha: 'abc1234', subject: 'feat: x' }] },
    });
  await rel('1.0.0', 'Первый.');
  const r2 = await rel('1.1.0', 'Второй.');
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${guid}/j/releases/${r2.id}`);
  await expect(page.getByRole('heading', { name: 'Релиз 1.1.0' })).toBeVisible();
  const compare = page.locator('.compare');
  await compare.getByRole('button', { name: 'Изменить', exact: true }).click();
  const form = compare.getByRole('form', { name: 'Правка релиза' });
  await expect(form.getByLabel('Заголовок')).toBeFocused();
  await form.getByLabel('Заголовок').fill('Релиз 1.1.0 «Осень»');
  await form.getByLabel('Текст').fill('Второй, с правкой.');
  // выбор другого релиза и «Корзина» при несохранённом — сначала вопрос; «Отмена» вопроса —
  // релиз и форма с набранным на месте
  const list = page.getByRole('list', { name: 'Релизы' });
  for (const leave of [list.getByRole('button', { name: /1\.0\.0/ }), trashToggle(page)]) {
    page.once('dialog', (d) => {
      expect([d.type(), d.message()]).toEqual(['confirm', LEAVE_EDIT]);
      void d.dismiss();
    });
    await leave.click();
    await expect(page.getByRole('heading', { name: 'Релиз 1.1.0' })).toBeVisible();
    await expect(form.getByLabel('Заголовок')).toHaveValue('Релиз 1.1.0 «Осень»');
  }
  await expect(trashToggle(page)).toHaveAttribute('aria-pressed', 'false');
  const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && r.url().endsWith(`/entries/${r2.id}`));
  await form.getByRole('button', { name: 'Сохранить' }).click();
  const resp = await saved;
  expect(resp.status()).toBe(200);
  // версия и коммиты — как есть: `data` в правке нет
  expect(resp.request().postDataJSON()).toEqual({ title: 'Релиз 1.1.0 «Осень»', body: 'Второй, с правкой.' });
  await expect(page.locator('section.journal > [role="status"]')).toHaveText('Релиз «Релиз 1.1.0 «Осень»» сохранён.');
  await expect(compare.locator('.rel-title')).toHaveText('Релиз 1.1.0 «Осень»');
  await expect(compare.getByRole('button', { name: 'Изменить', exact: true })).toBeFocused();
  const after = await agentApi<Rec>('GET', `entries/${r2.id}`);
  expect(after.data).toMatchObject({ version: '1.1.0', commits: [{ sha: 'abc1234', subject: 'feat: x' }] });

  // «Удалить» — в корзину: в списке один релиз, фокус — на итоге; «Корзина» — «Восстановить»
  await compare.getByRole('button', { name: 'Удалить', exact: true }).click();
  await page.getByRole('dialog', { name: 'Удалить релиз?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  const notice = page.locator('section.journal > [role="status"]');
  await expect(notice).toHaveText('Релиз «Релиз 1.1.0 «Осень»» перемещён в корзину.');
  await expect(notice).toBeFocused();
  await expect(list.getByRole('button')).toHaveCount(1);
  await restoreFromTrash(page, r2.id, 'Релиз 1.1.0 «Осень»');
  await expect(list.getByRole('button')).toHaveCount(2);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('journals_trash_insights_glossary_ideas_questions', async ({ page }) => {
  // «Удалить» у Insights и Словаря (итог над списком, фокус на нём), корзины Идей и Вопросов
  const problems = watch(page);
  const guid = await newSolution('корзины журналов');
  const post = (body: Record<string, unknown>) => agentApi<Rec>('POST', `spaces/${guid}/entries`, body);
  const ins = await post({ kind: 'insight', title: 'Insight в корзину', body: 'Суть.', data: { kind: 'gotcha' } });
  const term = await post({ kind: 'term', title: 'Терминус', body: '', data: { definition: 'Термин для корзины', aliases: [] } });
  const idea = await post({ kind: 'idea', title: 'Идея в корзину', body: 'Замысел.', data: { origin: 'agent' } });
  const q = await post({ kind: 'question', title: 'Вопрос в корзину', body: 'Нужен ли?' });
  const c = await pageCtx(page);
  const url = (j: string, id = '') => `${base()}#/u/${c.user}/s/${guid}/j/${j}${id ? `/${id}` : ''}`;

  // Insights: карточка → «Удалить» → строки нет, итог над таблицей в фокусе → корзина
  await page.goto(url('insights', ins.id));
  const icard = page.getByRole('article', { name: 'Карточка insight' });
  await icard.getByRole('button', { name: 'Удалить', exact: true }).click();
  await page.getByRole('dialog', { name: 'Удалить запись?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  const inotice = page.locator('section.journal').getByRole('status').filter({ hasText: 'перемещена в корзину' });
  await expect(inotice).toHaveText('Запись «Insight в корзину» перемещена в корзину.');
  await expect(inotice).toBeFocused();
  await expect(icard).toHaveCount(0);
  await expect(page.getByRole('table', { name: 'Insights' }).locator(`[data-entry="${ins.id}"]`)).toHaveCount(0);
  await restoreFromTrash(page, ins.id, 'Insight в корзину');
  await expect(page.getByRole('table', { name: 'Insights' }).locator(`[data-entry="${ins.id}"]`)).toHaveCount(1);

  // Словарь: то же
  await page.goto(url('glossary', term.id));
  const tcard = page.getByRole('article', { name: 'Карточка термина' });
  await tcard.getByRole('button', { name: 'Удалить', exact: true }).click();
  await page.getByRole('dialog', { name: 'Удалить термин?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  const tnotice = page.locator('section.journal').getByRole('status').filter({ hasText: 'в корзину' });
  await expect(tnotice).toHaveText('Термин «Терминус» перемещён в корзину.');
  await expect(tnotice).toBeFocused();
  await expect(page.getByRole('list', { name: 'Термины' })).not.toContainText('Терминус');
  await restoreFromTrash(page, term.id, 'Терминус');
  await expect(page.getByRole('list', { name: 'Термины' })).toContainText('Терминус');

  // Идеи: удалена агентом — в корзине журнала, «Восстановить»
  await agentApi('DELETE', `entries/${idea.id}`, undefined, { 'If-Match': `"${idea.rev}"` });
  await page.goto(url('ideas'));
  await expect(page.getByRole('button', { name: 'Новая идея' })).toBeVisible();
  await restoreFromTrash(page, idea.id, 'Идея в корзину');

  // Вопросы: «Удалить» в карточке → итог в фокусе → корзина
  await page.goto(url('questions'));
  const qcard = page.locator(`[data-question="${q.id}"]`);
  await qcard.getByRole('button', { name: 'Удалить «Вопрос в корзину»' }).click();
  await page.getByRole('dialog', { name: 'Удалить вопрос?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  const qnotice = page.locator('section.journal > [role="status"]');
  await expect(qnotice).toHaveText('Вопрос «Вопрос в корзину» перемещён в корзину.');
  await expect(qnotice).toBeFocused();
  await expect(qcard).toHaveCount(0);
  await restoreFromTrash(page, q.id, 'Вопрос в корзину');
  await expect(qcard).toContainText('Вопрос в корзину');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('changelog_edit_follows_entry', async ({ page }) => {
  // ревью задачи 10, круг 2 (N1, N2): форма правки привязана к записи — переход к другой записи
  // (адрес, Ctrl+K) её закрывает, кнопки журнала снова доступны; с несохранённым — сначала вопрос,
  // «Отмена» вопроса оставляет запись и набранное; переход к записи выключает «Корзину»
  const problems = watch(page);
  const guid = await newSolution('форма и переход');
  const post = (title: string) =>
    agentApi<Rec>('POST', `spaces/${guid}/entries`, { kind: 'changelog', title, body: '', data: { kind: 'fix' } });
  const x = await post('Запись X');
  const y = await post('Запись Y');
  const c = await pageCtx(page);
  const at = (id: string) => `#/u/${c.user}/s/${guid}/j/changelog/${id}`;
  const go = (h: string) => page.evaluate((hash) => (location.hash = hash), h);
  const opened = page.locator('.focused');
  await page.goto(`${base()}${at(x.id)}`);
  await expect(opened.getByRole('heading')).toHaveText('Запись X');
  await opened.getByRole('button', { name: 'Изменить «Запись X»' }).click();
  await expect(opened.getByRole('form', { name: 'Правка записи' })).toBeVisible();

  // без набранного: переход закрывает форму сразу, без вопроса; кнопки журнала доступны
  page.on('dialog', (d) => {
    problems.push(`неожиданный вопрос: ${d.message()}`);
    void d.dismiss();
  });
  await go(at(y.id));
  await expect(opened.getByRole('heading')).toHaveText('Запись Y');
  await expect(page.getByRole('form', { name: 'Правка записи' })).toHaveCount(0);
  for (const b of [
    opened.getByRole('button', { name: 'Изменить «Запись Y»' }),
    page.getByRole('feed', { name: 'Лента changelog' }).getByRole('button', { name: 'Изменить «Запись X»' }),
    trashToggle(page),
  ]) {
    await expect(b).toBeEnabled();
  }
  page.removeAllListeners('dialog');

  // с набранным: переход спрашивает; «Отмена» — адрес и форма с текстом на месте; согласие — форма
  // закрыта, запись X открыта
  await opened.getByRole('button', { name: 'Изменить «Запись Y»' }).click();
  const form = opened.getByRole('form', { name: 'Правка записи' });
  await form.getByLabel('Заголовок').fill('Y с правкой');
  const place = () => page.evaluate(() => (history.state as { zamIdx?: number } | null)?.zamIdx);
  const yIdx = await place();
  page.once('dialog', (d) => {
    expect([d.type(), d.message()]).toEqual(['confirm', LEAVE_EDIT]);
    void d.dismiss();
  });
  await go(at(x.id));
  await expect(page).toHaveURL(new RegExp(`/j/changelog/${y.id}$`));
  await expect(form.getByLabel('Заголовок')).toHaveValue('Y с правкой');
  // отменённый переход историю не меняет (круг 3, N6): прежняя запись (тот же номер), а не её копия;
  // отменённое «назад» — тоже
  await expect.poll(place).toBe(yIdx);
  page.once('dialog', (d) => void d.dismiss());
  await page.goBack();
  await expect(page).toHaveURL(new RegExp(`/j/changelog/${y.id}$`));
  await expect.poll(place).toBe(yIdx);
  await expect(form.getByLabel('Заголовок')).toHaveValue('Y с правкой');
  page.once('dialog', (d) => void d.accept());
  await go(at(x.id));
  await expect(opened.getByRole('heading')).toHaveText('Запись X');
  await expect(page.getByRole('form', { name: 'Правка записи' })).toHaveCount(0);
  await expect(opened.getByRole('button', { name: 'Изменить «Запись X»' })).toBeEnabled();
  expect((await agentApi<Rec>('GET', `entries/${y.id}`)).title).toBe('Запись Y');

  // при нажатой «Корзине» переход к записи показывает её: корзина выключается
  await trashToggle(page).click();
  await expect(trashRegion(page)).toBeVisible();
  await go(at(y.id));
  await expect(trashToggle(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(trashRegion(page)).toHaveCount(0);
  await expect(opened.getByRole('heading')).toHaveText('Запись Y');
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Участник решения `guid` с доступом `access`: вход в WebUI (пароль со сменой) — свой контекст. */
async function member(browser: Browser, page: Page, guid: string, access: 'ro' | 'rw') {
  const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { csrf: string };
  const admin = async <T>(method: string, path: string, data?: unknown) => {
    const r = await page.request.fetch(`${base()}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': me.csrf }, data });
    if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
    return (r.status() === 204 ? undefined : await r.json()) as T;
  };
  const name = `e2e-trash-${access}-${Date.now()}`;
  const pass = ['e2e', 'trash', 'pass', '1'].join('-');
  const pass2 = ['e2e', 'trash', 'pass', '2'].join('-');
  const u = await admin<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const req = await ctx.request.post(`${base()}api/v1/solutions`, {
    headers: { Authorization: `Bearer ${u.token.plaintext}` },
    data: { agent_solution_id: guid, name: 'Журналы E2E: доступ' },
  });
  expect(req.status()).toBe(202);
  await admin('POST', `solutions/${guid}/access/${u.user.id}/approve`, { access });
  await admin('POST', `admin/users/${u.user.id}/password`, { password: pass });
  // лимит входа общий у спек прогона — вход и смена пароля с повтором при 429 (`login.ts`)
  await loginChangingPassword(ctx.request, base(), name, pass, pass2);
  return { ctx, id: u.user.id };
}

test('journal_trash_ro_member_403', async ({ page, browser }) => {
  // участник `ro` общего решения: «Корзина» видна (кнопки не скрываются — как прочие действия
  // журналов), сервер отвечает 403 — текст ошибки в корзине, записей нет
  const guid = await newSolution('корзина участника ro');
  const e = await agentApi<Rec>('POST', `spaces/${guid}/entries`, { kind: 'changelog', title: 'Удалённая владельцем', data: { kind: 'fix' } });
  await agentApi('DELETE', `entries/${e.id}`, undefined, { 'If-Match': `"${e.rev}"` });
  const m = await member(browser, page, guid, 'ro');
  try {
    const mp = await m.ctx.newPage();
    const problems: string[] = [];
    mp.on('pageerror', (err) => problems.push(err.message));
    const trashed = mp.waitForResponse((r) => new URL(r.url()).searchParams.get('deleted') === 'only');
    await mp.goto(`${base()}#/u/${m.id}/s/${guid}/j/changelog`);
    // живых записей нет (единственная — в корзине): лента пуста
    await expect(mp.locator('section.journal')).toContainText('Записей нет.');
    await mp.locator('section.journal').getByRole('button', { name: 'Корзина', exact: true }).click();
    expect((await trashed).status()).toBe(403);
    const trash = mp.getByRole('region', { name: 'Корзина' });
    await expect(trash.getByRole('alert')).toHaveText('Недостаточно прав: запись запрещена: доступ участника к решению — ro');
    await expect(trash.locator('[data-trash]')).toHaveCount(0);
    await expect(trash).not.toContainText('Удалённая владельцем');
    expect(problems, problems.join('\n')).toEqual([]);
  } finally {
    await m.ctx.close();
  }
});

// ---- «Шумные» и счётчик «открыто» в Insights (M7.3, спецификация §6.6 п. 3; задача 8) ----
//
// Показ — подсказка к ошибке (`/recall post_tool`, триггер `error_regex`) в сессии агента, открытие —
// чтение записи по токену той же сессией после показа (`GET /entries/{id}` с `X-Zam-Session`).
// Фильтр — на сервере (лента `noisy=1`, решение 68): показано ≥ 10 и ни одного открытия за окно
// `max(now − 30 сут, opened_since)`; учёт открытий в e2e начался с запуска сервера — строка
// «данные об открытии копятся с <дата>».

/** Десять показов записи подсказкой: каждый — в своей сессии агента. */
async function shownTimes(guid: string, pattern: string, n: number, tag: string): Promise<string[]> {
  const sessions: string[] = [];
  for (let i = 1; i <= n; i++) {
    const s = `e2e-noisy-${tag}-${i}-${Date.now().toString(36)}`;
    await agentApi('POST', 'recall', { event: 'post_tool', error: `ошибка сборки: ${pattern}`, solution: guid, timeout_ms: 2000 }, { 'X-Zam-Session': s });
    sessions.push(s);
  }
  return sessions;
}

test('insights_noisy_filter', async ({ page }) => {
  test.setTimeout(120_000);
  const problems = watch(page);
  const guid = await newSolution('шумные');
  const stamp = Date.now().toString(36);
  const insight = (title: string, pattern: string) =>
    agentApi<Rec>('POST', `spaces/${guid}/entries`, {
      kind: 'insight',
      title,
      body: `${title}: подсказка к ошибке сборки.`,
      data: { kind: 'gotcha' },
      triggers: [{ type: 'error_regex', pattern }],
      force: true,
    });
  const noisy = await insight('Шумная подсказка E2E', `noisyalpha${stamp}`);
  const opened = await insight('Открытая подсказка E2E', `noisybeta${stamp}`);
  await shownTimes(guid, `noisyalpha${stamp}`, 10, 'a');
  const sessions = await shownTimes(guid, `noisybeta${stamp}`, 10, 'b');
  // агент открыл вторую после её подсказки — в той же сессии
  await agentApi('GET', `entries/${opened.id}`, undefined, { 'X-Zam-Session': sessions[0] });
  type Usage = { usage: { shown: number; opened: number; useful: number; useless: number } };
  expect((await agentApi<Usage>('GET', `entries/${noisy.id}`)).usage).toEqual({ shown: 10, opened: 0, useful: 0, useless: 0 });
  expect((await agentApi<Usage>('GET', `entries/${opened.id}`)).usage).toEqual({ shown: 10, opened: 1, useful: 0, useless: 0 });

  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${guid}/j/insights`);
  const table = page.getByRole('table', { name: 'Insights' });
  await expect(table.getByRole('columnheader', { name: 'Показано / открыто / полезно / бесполезно' })).toBeVisible();
  await expect(table.locator(`tr[data-entry="${noisy.id}"] td.usage`)).toHaveText('10 / 0 / 0 / 0');
  await expect(table.locator(`tr[data-entry="${opened.id}"] td.usage`)).toHaveText('10 / 1 / 0 / 0');
  // «Шумные» — с 10 показами без открытия есть, с одним открытием — нет
  const lists: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname.endsWith(`/spaces/${guid}/entries`)) lists.push(u.searchParams.get('noisy') ?? '');
  });
  await page.getByLabel('Шумные', { exact: true }).check();
  await expect(table.locator(`tr[data-entry="${noisy.id}"]`)).toBeVisible();
  await expect(table.locator(`tr[data-entry="${opened.id}"]`)).toHaveCount(0);
  expect(lists).toContain('1');
  // первые 30 суток учёта открытий — предупреждение с датой начала
  await expect(page.getByTestId('noisy-note')).toContainText('Данные об открытии копятся с');
  // карточка — «показано · открыто · полезно · бесполезно»
  await table.locator(`tr[data-entry="${noisy.id}"] button`).first().click();
  await expect(page.getByRole('article', { name: 'Карточка insight' }).getByTestId('usage')).toHaveText(
    'показано 10 · открыто 0 · полезно 0 · бесполезно 0',
  );
  // фильтр снят — обе снова в ленте
  await page.getByLabel('Шумные', { exact: true }).uncheck();
  await expect(table.locator(`tr[data-entry="${opened.id}"]`)).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});
