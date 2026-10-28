// Сквозные сценарии журналов решения в WebUI (M3): Changelog с фильтрами, сравнение релизов,
// замещение insight, повышение до theorem с доказательством, раскрытие секрета Vault,
// вкладка «Связи» с обратной ссылкой, карточки записей в поиске Ctrl+K.
// Данные — seedJournals (global-setup). Снимки — в .tmp/e2e/screens/ (под .gitignore).
import { expect, test, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
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
  await expect(journals.getByRole('link')).toHaveText(['Changelog', 'Releases', 'Insights', 'Vault']);
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

  // Knowledge: только Insights и Vault
  const t = tree(page);
  await t.getByRole('button', { name: 'Knowledge', exact: true }).click();
  await expect(t.getByRole('list', { name: 'Журналы: Knowledge' }).getByRole('link')).toHaveText([
    'Insights',
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
  const form = card.getByRole('form', { name: 'Замена insight' });
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
  const cf = card.getByRole('form', { name: 'Статус надёжности' });
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
  const tr = card.getByLabel('Триггеры', { exact: true });
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
  const rf = card.getByRole('form', { name: 'Отзыв insight' });
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
