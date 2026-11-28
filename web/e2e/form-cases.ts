// Перечень мест, где пользователь пишет (M7.3, спецификация §6.14 п. 9; задача владельца
// k:01M85AS1C0EZVV95FQZ2ZEQCET): одна строка на форму. `forms.spec.ts` проходит каждую строку в трёх
// ширинах (`phone` с касанием, `tablet`, `desktop`), открытие с фокусом — ещё и в `tabletLand`:
// окно видно и в фокусе, Tab не выходит из окна, Esc без набранного закрывает и возвращает фокус,
// несохранённое спрашивает внутри окна, ошибка сервера (ответ 422 на `route`) — в окне, поля целы,
// после «Добавить» (`after: 'card'`) — карточка созданного на виду и в фокусе.
//
// Строки задачи 10a — Задачи, Insights, Идеи (ядро); задачи 10b — Словарь, Вопросы, ленты Changelog
// и Releases, окна дерева, вложений, истории, отчётов и кэша, администрирование, импорт, копии и
// индекс; задачи 8 — страницы M7.3 (подключение модели, «Подтвердить адрес», «Отключить модель»,
// «Запустить сейчас», «Принять как норму», «Принять» и «Отклонить» кандидата); M7.4 задачи 4 —
// «Добавить пробу» и «Снять» набора проб «Проверки памяти». Задачи дописывают
// свои строки — перед последним элементом массива (не в хвост: параллельные задачи иначе правят
// одну строку). Данные строк — `E2E_FORMS_*` (наполнение — `seedForms` в `forms.spec.ts`) и `E2E_*`
// global-setup.
import { expect, type Locator, type Page } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { IDEAS_GUID } from './seed';
import { checkFixture, runsReply } from './seed-internal';
import type { PageCtx } from './mobile-pages';

export interface FormCase {
  /** Латиница без пробелов: имя теста и файла снимка (`<id>-<ширина>.png`). */
  id: string;
  /** Форма перечня §6.14 (по-русски). */
  name: string;
  /** Имя окна для чтения с экрана (`getByRole('dialog', {name})`); по умолчанию — `name`. */
  dialog?: string | RegExp;
  /** Адрес, где открывается форма. */
  page: (c: PageCtx) => string;
  /** Открыть форму (кнопкой страницы или карточки). */
  open: (p: Page) => Promise<void>;
  /** Подпись поля, на котором фокус при открытии (у подтверждений — нет). */
  first?: string;
  /** Набрать текст (у подтверждений — нет): форма становится «грязной». */
  fill?: (p: Page) => Promise<void>;
  /** Запрос отправки: `page.route` отвечает на него 422 (`forms_server_error_keeps_input`). */
  route?: { method: string; url: string | RegExp };
  /** Что после отправки: карточка созданного, итог строкой или ничего. */
  after: 'card' | 'notice' | 'none';
  /** Окно-подтверждение без полей. */
  confirm?: boolean;
  /** Подтверждение необратимого или рискованного (`danger` и `cautious` каркаса): фокус — на
   * «Отмена», Enter ничего не делает. */
  danger?: boolean;
  as?: 'admin' | 'user';
  /** Снимки «до» (0.0.15): `aria-label` прежней формы в карточке — на виду ли она. */
  legacy?: string;
  /** Формы нет в 0.0.15 (снимок «до» — только экран, откуда она откроется). */
  added?: boolean;
  /** Форма не в окне, а в карточке на месте (правка «Изменить», поле шага — правила А и В):
   * где искать поля и ошибку (`scope`), чем отправить (`submit`); `focus` — фокус на `first` при
   * открытии (поле на виду открытия нет — фокус не переводится). */
  inline?: { scope: (p: Page) => Locator; submit: string; focus: boolean };
  /** До открытия страницы (подмена ответа `page.route`, где состояние сервера трогать нельзя). */
  prepare?: (p: Page) => Promise<void>;
  /** Подтверждение необратимого: после Enter на «Отмена» — ничего не изменилось (кроме общей
   * проверки «запроса `route` не было»; без `kept` у адреса журнала — запись адреса цела). */
  kept?: (p: Page) => Promise<void>;
}

/** GET REST сессией администратора (cookie состояния браузера). */
export async function sessionGet<T>(p: Page, path: string): Promise<T> {
  const r = await p.request.get(`${process.env.E2E_BASE_URL}api/v1/${path}`);
  expect(r.ok(), `GET ${path}: HTTP ${r.status()}`).toBe(true);
  return (await r.json()) as T;
}

/** Окно формы по имени. */
export const dialogOf = (p: Page, name: string | RegExp): Locator =>
  typeof name === 'string' ? p.getByRole('dialog', { name, exact: true }) : p.getByRole('dialog', { name });

/** Карточка записи журнала (`article` с `aria-label`). */
const card = (p: Page, label: string) => p.getByRole('article', { name: label });

/** Кнопка карточки (видимая) — нажать. */
async function pressIn(p: Page, label: string, button: string): Promise<void> {
  const c = card(p, label);
  await expect(c).toBeVisible();
  await c.getByRole('button', { name: button, exact: true }).click();
}

/** Кнопка страницы — нажать. */
async function press(p: Page, button: string): Promise<void> {
  await p.getByRole('button', { name: button, exact: true }).first().click();
}

/** Поле окна `dialog` (или страницы, если окна нет — снимки «до») по подписи — набрать `text`. */
async function type(p: Page, dialog: string, label: string, text: string): Promise<void> {
  const d = dialogOf(p, dialog);
  const scope = (await d.count()) ? d : p.locator('body');
  await scope.getByLabel(label).first().fill(text);
}

const stamp = () => `${Date.now().toString(36)}`;
/** Дата UTC через `days` дней (`YYYY-MM-DD`). */
const utcDay = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString().slice(0, 10);

const sol = (c: PageCtx) => c.env('E2E_FORMS_SOLUTION');
const journal = (c: PageCtx, j: string, entry = '', space = sol(c)) => `#/u/${c.user}/s/${space}/j/${j}${entry ? `/${entry}` : ''}`;

const TASK = 'Карточка задачи';
const INSIGHT = 'Карточка insight';
const IDEA = 'Карточка идеи';
/** Отправка правки записи: `PATCH /entries/{id}`. */
const PATCH_ENTRY = { method: 'PATCH', url: /\/api\/v1\/entries\/[^/?]+$/ };
const DECIDE = { method: 'POST', url: /\/api\/v1\/entries\/[^/?]+\/decide$/ };
const DELETE_ENTRY = { method: 'DELETE', url: /\/api\/v1\/entries\/[^/?]+$/ };
const CREATE_ENTRY = { method: 'POST', url: /\/api\/v1\/spaces\/[^/?]+\/entries$/ };
const ANSWER = { method: 'POST', url: /\/api\/v1\/entries\/[^/?]+\/answer$/ };
const NODE_CREATE = { method: 'POST', url: /\/api\/v1\/nodes$/ };
const SETTINGS_PATCH = { method: 'PATCH', url: /\/api\/v1\/admin\/settings$/ };
const REINDEX = { method: 'POST', url: /\/api\/v1\/admin\/reindex$/ };

const TERM = 'Карточка термина';
/** Вопрос ленты по id из `E2E_*`. */
const question = (p: Page, k: string) => p.locator(`[data-question="${process.env[k]}"]`);
/** Запись ленты Changelog из адреса (открытая сверху — первая). */
const changelogEntry = (p: Page) => p.locator(`article[data-entry="${process.env.E2E_FORMS_CHANGELOG}"]`).first();
/** Участники решения форм раскрыты («Доступ» — переключатель: при повторном открытии окна строки
 * уже на виду), строка участника k — на виду. */
async function accessOf(p: Page, k: string): Promise<void> {
  const m = p.locator(`tr[data-member="${process.env[k]}"]`);
  if (!(await m.isVisible())) await p.locator(`tr[data-solution="${process.env.E2E_FORMS_ACCESS_SOL}"]`).getByRole('button', { name: 'Доступ' }).click();
  await expect(m).toBeVisible();
}

/** Строка пользователя форм в таблице «Пользователи». */
const userRow = (p: Page) => p.locator(`tr[data-user="${process.env.E2E_FORMS_USER}"]`);

// ---- страницы M7.3 (задача 8): «Внутренние работы», «Проверка памяти», «Кандидаты в Rules» ----

/** Настройки пользователя (`GET/PATCH /auth/settings`): исполнитель внутренних работ — `works`. */
const SETTINGS = /\/api\/v1\/auth\/settings$/;

/** Перейти на страницу приложения из открытой: страница монтируется заново и читает подменённые
 * `page.route` ответы; запомненная кнопка, открывшая окно, — из того же документа. */
async function goHash(p: Page, hash: string, heading: string): Promise<void> {
  await p.evaluate((h) => (location.hash = h), hash);
  await expect(p.getByRole('heading', { name: heading, exact: true })).toBeVisible();
}

/** Подменить `works` в `GET /auth/settings` (прочее — настоящий ответ сервера). Нужное состояние
 * (пауза `vault_changed`, режим «агент») на сервере e2e не подготовить без настоящей модели или
 * правки чужих сценариев; отправку окна строки форм не делают — подмены хватает. */
async function mockWorks(p: Page, patch: (w: Record<string, unknown>) => Record<string, unknown>): Promise<void> {
  await p.route(SETTINGS, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const resp = await route.fetch();
    const body = (await resp.json()) as { works?: Record<string, unknown> };
    return route.fulfill({ response: resp, json: { ...body, works: patch(body.works ?? {}) } });
  });
}

/** Подключение для подмены паузы: запись Vault «модель в LAN» (`seed-internal.ts`). */
const mockModel = () => ({
  protocol: 'openai',
  vault: `v:${process.env.E2E_VAULT_LAN}`,
  model: 'e2e-mock',
  auth: 'none',
  max_tokens: 4000,
  token_field: 'auto',
  body_extra: {},
  pinned: { scheme: 'http', host: '127.0.0.1', port: 9, path: '/v1' },
});

export const FORM_CASES: FormCase[] = [
  // ---- Задачи (задача 10a) ----
  {
    id: 'task-create',
    name: 'Новая задача',
    page: (c) => journal(c, 'tasks'),
    open: (p) => press(p, 'Новая задача'),
    first: 'Заголовок',
    fill: (p) => type(p, 'Новая задача', 'Заголовок', `Задача из окна E2E ${stamp()}`),
    route: CREATE_ENTRY,
    after: 'card',
    legacy: 'Новая задача',
  },
  {
    id: 'task-blocked',
    name: 'Смена статуса: заблокирован',
    dialog: 'Задача заблокирована',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: (p) => pressIn(p, TASK, '→ заблокирован'),
    first: 'Что мешает',
    fill: (p) => type(p, 'Задача заблокирована', 'Что мешает', 'ждём ответа владельца'),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Смена статуса задачи',
  },
  {
    id: 'task-reopen',
    name: 'Смена статуса: переоткрыть',
    dialog: 'Переоткрыть задачу',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_DONE')),
    open: (p) => pressIn(p, TASK, 'Переоткрыть'),
    first: 'Причина переоткрытия',
    fill: (p) => type(p, 'Переоткрыть задачу', 'Причина переоткрытия', 'тесты снова красные'),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Смена статуса задачи',
  },
  {
    id: 'task-done',
    name: 'Смена статуса: выполнен',
    dialog: 'Отметить задачу выполненной?',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: (p) => pressIn(p, TASK, '→ выполнен'),
    route: PATCH_ENTRY,
    after: 'notice',
    confirm: true,
    legacy: 'Смена статуса задачи',
  },
  {
    id: 'task-dropped',
    name: 'Смена статуса: снят',
    dialog: 'Снять задачу?',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: (p) => pressIn(p, TASK, '→ снят'),
    route: PATCH_ENTRY,
    after: 'notice',
    confirm: true,
    danger: true,
    legacy: 'Смена статуса задачи',
  },
  {
    id: 'task-delete',
    name: 'Удалить задачу',
    dialog: 'Удалить задачу?',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: (p) => pressIn(p, TASK, 'Удалить'),
    route: DELETE_ENTRY,
    after: 'none',
    confirm: true,
    danger: true,
  },
  // ---- Insights (задача 10a) ----
  {
    id: 'insight-new',
    name: 'Новый insight',
    page: (c) => journal(c, 'insights'),
    open: (p) => press(p, 'Новый insight'),
    first: 'Заголовок',
    fill: (p) => type(p, 'Новый insight', 'Заголовок', `Insight из окна E2E ${stamp()}`),
    route: CREATE_ENTRY,
    after: 'card',
    added: true,
  },
  {
    id: 'insight-supersede',
    name: 'Заменить insight',
    dialog: 'Заменить запись',
    // своя запись: удавшаяся замена (`forms_create_opens_created_card`) переводит строку на новую
    // запись, не задевая прочие строки Insights
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT_SUP')),
    open: (p) => pressIn(p, INSIGHT, 'Заменить'),
    first: 'Заголовок',
    fill: (p) => type(p, 'Заменить запись', 'Заголовок', `Решение форм E2E, уточнено ${stamp()}`),
    route: { method: 'POST', url: /\/api\/v1\/entries\/[^/?]+\/supersede$/ },
    after: 'card',
    legacy: 'Замена insight',
  },
  {
    id: 'insight-adr',
    name: 'Поля ADR',
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT')),
    open: (p) => pressIn(p, INSIGHT, 'Править ADR'),
    first: 'Контекст',
    fill: (p) => type(p, 'Поля ADR', 'Контекст', 'Контекст решения дописан в окне'),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Поля ADR',
  },
  {
    id: 'insight-retract',
    name: 'Отозвать insight',
    dialog: 'Отозвать запись',
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT')),
    open: (p) => pressIn(p, INSIGHT, 'Отозвать'),
    first: 'Причина',
    fill: (p) => type(p, 'Отозвать запись', 'Причина', 'устарело после перехода на окна'),
    route: { method: 'POST', url: /\/api\/v1\/entries\/[^/?]+\/retract$/ },
    after: 'notice',
    legacy: 'Отзыв insight',
  },
  {
    id: 'insight-certainty',
    name: 'Статус надёжности',
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT')),
    open: (p) => pressIn(p, INSIGHT, 'Сменить статус надёжности'),
    // фокус — на главном выборе окна: выбранный статус (запись агента — `hypothesis` по умолчанию)
    first: 'hypothesis',
    fill: (p) => type(p, 'Статус надёжности', 'Причина', 'проверено на стенде'),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Статус надёжности',
  },
  {
    id: 'insight-triggers',
    name: 'Триггеры',
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT')),
    open: (p) => pressIn(p, INSIGHT, 'Триггеры'),
    first: 'Шаблон триггера',
    fill: (p) => type(p, 'Триггеры', 'Шаблон триггера', 'FormDialog'),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Триггеры',
  },
  {
    id: 'insight-delete',
    name: 'Удалить insight',
    dialog: 'Удалить запись?',
    page: (c) => journal(c, 'insights', c.env('E2E_FORMS_INSIGHT')),
    open: (p) => pressIn(p, INSIGHT, 'Удалить'),
    route: DELETE_ENTRY,
    after: 'none',
    confirm: true,
    danger: true,
  },
  // ---- Идеи (задача 10a) ----
  {
    id: 'idea-create',
    name: 'Новая идея',
    page: (c) => journal(c, 'ideas'),
    open: (p) => press(p, 'Новая идея'),
    first: 'Заголовок',
    fill: (p) => type(p, 'Новая идея', 'Заголовок', `Идея из окна E2E ${stamp()}`),
    route: CREATE_ENTRY,
    after: 'card',
    legacy: 'Новая идея',
  },
  {
    id: 'idea-start',
    name: 'Начать идею Knowledge',
    dialog: 'Начать: где поставить задачу',
    page: (c) => journal(c, 'ideas', c.env('E2E_IDEA_KNOWLEDGE'), 'knowledge'),
    open: (p) => pressIn(p, IDEA, 'Начать'),
    first: 'Пространство задачи',
    fill: async (p) => {
      const d = dialogOf(p, 'Начать: где поставить задачу');
      const scope = (await d.count()) ? d : p.locator('body');
      await scope.getByLabel('Пространство задачи').selectOption({ index: 1 });
    },
    route: DECIDE,
    after: 'notice',
    legacy: 'Начать идею',
  },
  {
    id: 'idea-defer',
    name: 'Отложить идею',
    dialog: 'Отложить',
    page: (c) => journal(c, 'ideas', c.env('E2E_FORMS_IDEA_NEW')),
    open: (p) => pressIn(p, IDEA, 'Отложить'),
    first: 'Отложить до',
    fill: (p) => type(p, 'Отложить', 'Отложить до', utcDay(60)),
    route: DECIDE,
    after: 'notice',
    legacy: 'Отложить идею',
  },
  {
    id: 'idea-decline',
    name: 'Отказаться от идеи',
    dialog: 'Отказаться',
    page: (c) => journal(c, 'ideas', c.env('E2E_FORMS_IDEA_NEW')),
    open: (p) => pressIn(p, IDEA, 'Отказаться'),
    first: 'Причина',
    fill: (p) => type(p, 'Отказаться', 'Причина', 'не в этом году'),
    route: DECIDE,
    after: 'notice',
    legacy: 'Отказаться от идеи',
  },
  {
    id: 'idea-reopen',
    name: 'Вернуть идею на разбор',
    dialog: 'Вернуть на разбор',
    page: (c) => journal(c, 'ideas', c.env('E2E_FORMS_IDEA_DECLINED')),
    open: (p) => pressIn(p, IDEA, 'Вернуть на разбор'),
    first: 'Причина',
    fill: (p) => type(p, 'Вернуть на разбор', 'Причина', 'появились новые данные'),
    route: DECIDE,
    after: 'notice',
    legacy: 'Вернуть идею',
  },
  // ---- правка в карточке на месте и поле на виду (правила В и А, задача 10a) ----
  {
    id: 'task-edit',
    name: 'Изменить задачу (в карточке)',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: (p) => pressIn(p, TASK, 'Изменить'),
    first: 'Заголовок',
    fill: (p) => p.getByRole('form', { name: 'Правка задачи' }).getByLabel('Заголовок').fill(`Задача форм: правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    inline: { scope: (p) => p.getByRole('form', { name: 'Правка задачи' }), submit: 'Сохранить', focus: true },
  },
  {
    id: 'task-step',
    name: 'Следующий шаг (поле на виду)',
    page: (c) => journal(c, 'tasks', c.env('E2E_FORMS_TASK_OPEN')),
    open: async (p) => {
      await expect(card(p, TASK).getByLabel('Следующий шаг')).toBeVisible();
    },
    first: 'Следующий шаг',
    fill: (p) => card(p, TASK).getByLabel('Следующий шаг').fill(`шаг из e2e ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    inline: { scope: (p) => card(p, TASK), submit: 'Сохранить шаг', focus: false },
  },
  {
    id: 'idea-edit',
    name: 'Изменить идею (в карточке)',
    page: (c) => journal(c, 'ideas', c.env('E2E_FORMS_IDEA_NEW')),
    open: (p) => pressIn(p, IDEA, 'Изменить'),
    first: 'Заголовок',
    fill: (p) => p.getByRole('form', { name: 'Правка идеи' }).getByLabel('Заголовок').fill(`Идея форм: правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    inline: { scope: (p) => p.getByRole('form', { name: 'Правка идеи' }), submit: 'Сохранить', focus: true },
  },
  // ---- страницы M7.3 (задача 8): подключение модели, «Подтвердить адрес», «Отключить модель»,
  // «Запустить сейчас», «Принять как норму», «Принять» и «Отклонить» кандидата ----
  {
    id: 'works-model',
    name: 'Подключение модели',
    page: () => '#/works',
    open: (p) => p.getByRole('button', { name: /^(Подключить модель|Изменить подключение)$/ }).click(),
    first: 'Запись Vault',
    fill: (p) => type(p, 'Подключение модели', 'Имя модели', `e2e-forms-${stamp()}`),
    route: { method: 'PATCH', url: SETTINGS },
    after: 'notice',
    added: true,
  },
  {
    id: 'works-confirm-address',
    name: 'Подтвердить адрес',
    dialog: 'Подтвердить новый адрес подключения?',
    page: (c) => `#/u/${c.user}`,
    open: async (p) => {
      await mockWorks(p, (w) => ({
        ...w,
        model: w.model ?? mockModel(),
        pause: { class: 'vault_changed', since: new Date().toISOString(), from: 'http://127.0.0.1:9/v1', to: 'http://127.0.0.1:9/v2' },
      }));
      await goHash(p, '#/works', 'Внутренние работы');
      await press(p, 'Подтвердить адрес');
    },
    route: { method: 'PATCH', url: SETTINGS },
    after: 'notice',
    confirm: true,
    // фокус — на «Отмена» (`cautious`; финальное ревью M7.3, Minor 10): Enter не подтверждает
    // новый адрес подключения
    danger: true,
    added: true,
    kept: async (p) => {
      // пауза на месте: «Подтвердить адрес» по-прежнему на странице
      await expect(p.getByTestId('works-pause')).toBeVisible();
      await expect(p.getByRole('button', { name: 'Подтвердить адрес', exact: true })).toBeVisible();
    },
  },
  {
    // ревью r1 задачи 8, Minor 7: подключение убирается из интерфейса — опасное подтверждение
    id: 'works-disconnect',
    name: 'Отключить модель',
    dialog: 'Отключить модель?',
    page: (c) => `#/u/${c.user}`,
    open: async (p) => {
      await mockWorks(p, (w) => ({ ...w, model: w.model ?? mockModel() }));
      await goHash(p, '#/works', 'Внутренние работы');
      await press(p, 'Отключить модель');
    },
    route: { method: 'PATCH', url: SETTINGS },
    after: 'notice',
    confirm: true,
    danger: true,
    added: true,
    kept: async (p) => {
      // подключение на месте: сводка и кнопка «Отключить модель» видны
      await expect(p.getByRole('button', { name: 'Отключить модель', exact: true })).toBeVisible();
      await expect(p.getByRole('button', { name: 'Изменить подключение', exact: true })).toBeVisible();
    },
  },
  {
    id: 'works-run',
    name: 'Запустить сейчас',
    dialog: 'Запустить «Пробы ночной проверки» сейчас?',
    page: (c) => `#/u/${c.user}`,
    open: async (p) => {
      await mockWorks(p, (w) => ({ ...w, effective: { ...(w.effective as object), probes: { mode: 'agent', reason: null } } }));
      await goHash(p, '#/works', 'Внутренние работы');
      await press(p, 'Запустить сейчас: Пробы ночной проверки');
    },
    route: { method: 'POST', url: /\/api\/v1\/works\/run$/ },
    after: 'notice',
    confirm: true,
    added: true,
  },
  {
    id: 'check-accept',
    name: 'Принять как норму',
    dialog: 'Принять как норму?',
    page: (c) => `#/u/${c.user}`,
    open: async (p) => {
      // эпизод регрессии — подстановкой (решение 66): на release-сборке его не подготовить
      const f = checkFixture();
      await p.route(/\/api\/v1\/check\/status$/, (route) => route.fulfill({ json: f.status }));
      await p.route(/\/api\/v1\/check\/runs(\?.*)?$/, (route) => route.fulfill({ json: runsReply(route.request().url(), f.runs) }));
      await goHash(p, '#/check', 'Проверка памяти');
      await press(p, 'Принять как норму');
    },
    route: { method: 'POST', url: /\/api\/v1\/check\/accept$/ },
    after: 'notice',
    confirm: true,
    added: true,
  },
  // ---- набор проб «Проверки памяти» (M7.4, §7.7 п. 1): «Добавить пробу» и «Снять» ----
  {
    id: 'probe-add',
    name: 'Добавить пробу',
    page: () => '#/check',
    open: (p) => press(p, 'Добавить пробу'),
    first: 'Вопрос',
    fill: async (p) => {
      await type(p, 'Добавить пробу', 'Вопрос', `Где окно формы пробы ${stamp()}`);
      // цель — живая запись решения форм: форма ссылки верна, отказ 422 подставляет `route`
      await type(p, 'Добавить пробу', 'Цели', `k:${process.env.E2E_FORMS_TASK_OPEN}`);
    },
    route: { method: 'POST', url: /\/api\/v1\/check\/probes$/ },
    after: 'notice',
    added: true,
  },
  {
    id: 'probe-retire',
    name: 'Снять пробу',
    dialog: 'Снять пробу?',
    page: () => '#/check',
    // проба строки — ручная проба агента (`E2E_FORMS_PROBE`, `seedForms`); снимает только человек
    open: (p) => p.locator(`[data-probe-set="${process.env.E2E_FORMS_PROBE}"]`).getByRole('button', { name: /^Снять пробу «/ }).click(),
    first: 'Причина снятия',
    fill: (p) => type(p, 'Снять пробу?', 'Причина снятия', 'вопрос не о записи-цели'),
    route: { method: 'DELETE', url: /\/api\/v1\/check\/probes\/[^/?]+$/ },
    after: 'notice',
    danger: true,
    added: true,
  },
  {
    id: 'proposal-accept',
    name: 'Принять кандидата',
    page: (c) => `#/proposals/${c.env('E2E_PROPOSAL_FORMS')}`,
    open: (p) => pressIn(p, 'Карточка кандидата', 'Принять'),
    first: 'Путь в Rules',
    fill: (p) => type(p, 'Принять кандидата', 'Текст правила', `Перед правкой окна формы проверяй его на телефоне и планшете ${stamp()}.`),
    route: { method: 'POST', url: /\/api\/v1\/proposals\/[^/?]+\/accept$/ },
    after: 'notice',
    added: true,
  },
  {
    id: 'proposal-decline',
    name: 'Отклонить кандидата',
    page: (c) => `#/proposals/${c.env('E2E_PROPOSAL_FORMS')}`,
    open: (p) => pressIn(p, 'Карточка кандидата', 'Отклонить'),
    first: 'Причина',
    fill: (p) => type(p, 'Отклонить кандидата', 'Причина', 'правило уже есть в другом виде'),
    route: { method: 'POST', url: /\/api\/v1\/proposals\/[^/?]+\/decline$/ },
    after: 'notice',
    added: true,
  },
  // ---- Словарь (задача 10b): «Новый термин» и «Отозвать» — окна, правка — в карточке ----
  {
    id: 'term-new',
    name: 'Новый термин',
    page: (c) => journal(c, 'glossary'),
    open: (p) => press(p, 'Новый термин'),
    first: 'Имя',
    fill: async (p) => {
      await type(p, 'Новый термин', 'Имя', `Термин E2E ${stamp()}`);
      await type(p, 'Новый термин', 'Определение', 'Определение из окна формы.');
    },
    route: CREATE_ENTRY,
    after: 'card',
    legacy: 'Новый термин',
  },
  {
    id: 'term-retract',
    name: 'Отозвать термин',
    page: (c) => journal(c, 'glossary', c.env('E2E_FORMS_TERM')),
    open: (p) => pressIn(p, TERM, 'Отозвать'),
    first: 'Причина',
    fill: (p) => type(p, 'Отозвать термин', 'Причина', 'термин больше не нужен'),
    route: { method: 'POST', url: /\/api\/v1\/entries\/[^/?]+\/retract$/ },
    after: 'notice',
    legacy: 'Отзыв термина',
  },
  {
    id: 'term-edit',
    name: 'Править термин (в карточке)',
    page: (c) => journal(c, 'glossary', c.env('E2E_FORMS_TERM')),
    open: (p) => pressIn(p, TERM, 'Править'),
    first: 'Имя',
    fill: (p) => p.getByRole('form', { name: 'Правка термина' }).getByLabel('Определение').fill(`Определение, правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Правка термина',
    inline: { scope: (p) => p.getByRole('form', { name: 'Правка термина' }), submit: 'Сохранить', focus: true },
  },
  // ---- Вопросы (задача 10b): лента без карточки — правка в окне, «отложить» и «начать» — окна ----
  {
    id: 'question-edit',
    name: 'Правка вопроса',
    page: (c) => journal(c, 'questions', c.env('E2E_FORMS_QUESTION')),
    open: (p) => question(p, 'E2E_FORMS_QUESTION').getByRole('button', { name: /^Изменить «/ }).click(),
    first: 'Заголовок',
    fill: (p) => type(p, 'Правка вопроса', 'Заголовок', `Вопрос форм E2E: правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Правка вопроса',
  },
  {
    id: 'question-obsolete',
    name: 'Вопрос неактуален',
    dialog: 'Вопрос неактуален?',
    page: (c) => journal(c, 'questions', c.env('E2E_FORMS_QUESTION')),
    open: (p) => question(p, 'E2E_FORMS_QUESTION').getByRole('button', { name: /^Неактуален «/ }).click(),
    first: 'Причина',
    fill: (p) => type(p, 'Вопрос неактуален?', 'Причина', 'решили без вопроса'),
    route: PATCH_ENTRY,
    after: 'notice',
  },
  {
    id: 'question-defer',
    name: 'Отложить до (вопрос-предложение)',
    dialog: 'Отложить',
    page: (c) => `#/u/${c.user}/s/knowledge/j/questions/${c.env('E2E_FORMS_QUESTION_SHARED')}`,
    open: (p) => question(p, 'E2E_FORMS_QUESTION_SHARED').getByRole('button', { name: 'отложить', exact: true }).click(),
    first: 'Отложить до',
    fill: (p) => type(p, 'Отложить', 'Отложить до', utcDay(45)),
    route: ANSWER,
    after: 'notice',
    legacy: 'Отложить идею',
  },
  {
    id: 'question-start',
    name: 'Начать идею в общем решении',
    dialog: 'Начать идею в общем решении?',
    page: (c) => `#/u/${c.user}/s/knowledge/j/questions/${c.env('E2E_FORMS_QUESTION_SHARED')}`,
    open: (p) => question(p, 'E2E_FORMS_QUESTION_SHARED').getByRole('button', { name: 'начать', exact: true }).click(),
    route: ANSWER,
    after: 'notice',
    confirm: true,
    legacy: 'Подтверждение «начать»',
  },
  {
    id: 'question-answer',
    name: 'Ответ текстом (поле на виду)',
    page: (c) => journal(c, 'questions', c.env('E2E_FORMS_QUESTION')),
    open: async (p) => {
      await expect(question(p, 'E2E_FORMS_QUESTION').getByLabel('Ответ на вопрос')).toBeVisible();
    },
    first: 'Ответ на вопрос',
    fill: (p) => question(p, 'E2E_FORMS_QUESTION').getByLabel('Ответ на вопрос').fill(`свой ответ ${stamp()}`),
    route: ANSWER,
    after: 'notice',
    legacy: 'Ответ текстом',
    inline: { scope: (p) => question(p, 'E2E_FORMS_QUESTION'), submit: 'Ответить', focus: false },
  },
  // ---- ленты Changelog и Releases (задача 10b): правка — в широком окне ----
  {
    id: 'changelog-edit',
    name: 'Правка записи Changelog',
    dialog: 'Правка записи',
    page: (c) => journal(c, 'changelog', c.env('E2E_FORMS_CHANGELOG')),
    open: (p) => changelogEntry(p).getByRole('button', { name: /^Изменить «/ }).click(),
    first: 'Заголовок',
    fill: (p) => type(p, 'Правка записи', 'Заголовок', `Запись форм E2E: правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Правка записи',
  },
  {
    id: 'changelog-delete',
    name: 'Удалить запись журнала',
    dialog: 'Удалить запись?',
    page: (c) => journal(c, 'changelog', c.env('E2E_FORMS_CHANGELOG')),
    open: (p) => changelogEntry(p).getByRole('button', { name: /^Удалить «/ }).click(),
    route: DELETE_ENTRY,
    after: 'none',
    confirm: true,
    danger: true,
  },
  {
    id: 'release-edit',
    name: 'Правка релиза',
    page: (c) => journal(c, 'releases', c.env('E2E_FORMS_RELEASE')),
    open: (p) => p.locator('.compare').getByRole('button', { name: 'Изменить', exact: true }).click(),
    first: 'Заголовок',
    fill: (p) => type(p, 'Правка релиза', 'Заголовок', `Релиз форм E2E: правка ${stamp()}`),
    route: PATCH_ENTRY,
    after: 'notice',
    legacy: 'Правка релиза',
  },
  // ---- окна `Prompt` дерева, вложений, истории, отчёта и кэша (каркас — задача 10a; строки — 10b) ----
  {
    id: 'note-new',
    name: 'Новая заметка',
    page: (c) => `#/u/${c.user}`,
    open: (p) => treeMenu(p, 'Knowledge', 'Новая заметка'),
    first: 'Имя',
    fill: (p) => type(p, 'Новая заметка', 'Имя', `Заметка форм ${stamp()}`),
    route: NODE_CREATE,
    after: 'none',
  },
  {
    id: 'folder-new',
    name: 'Новая папка',
    page: (c) => `#/u/${c.user}`,
    open: (p) => treeMenu(p, 'Knowledge', 'Новая папка'),
    first: 'Имя',
    fill: (p) => type(p, 'Новая папка', 'Имя', `Папка форм ${stamp()}`),
    route: NODE_CREATE,
    after: 'none',
  },
  {
    id: 'note-move',
    name: 'Переименовать или переместить',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: (p) => treeMenu(p, 'Медиа-заметка', 'Переименовать/переместить'),
    first: 'Путь',
    fill: (p) => type(p, 'Переименовать или переместить', 'Путь', `Формы/Перенос ${stamp()}`),
    route: { method: 'POST', url: /\/api\/v1\/nodes\/[^/?]+\/move$/ },
    after: 'none',
  },
  {
    id: 'note-delete',
    name: 'Удалить заметку',
    dialog: 'Удалить?',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: (p) => treeMenu(p, 'Медиа-заметка', 'Удалить'),
    route: { method: 'DELETE', url: /\/api\/v1\/nodes\/[^/?]+$/ },
    after: 'none',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await sessionGet(p, `nodes/${process.env.E2E_NOTE_MEDIA}`);
    },
  },
  {
    id: 'attachment-detach',
    name: 'Открепить вложение',
    dialog: 'Открепить вложение?',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_DOCS')}`,
    open: async (p) => {
      await p.getByRole('tab', { name: 'Вложения' }).click();
      await p.getByRole('button', { name: /^Открепить / }).first().click();
    },
    route: { method: 'DELETE', url: /\/api\/v1\/nodes\/[^/?]+\/attachments\// },
    after: 'none',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.getByRole('button', { name: /^Открепить / }).first()).toBeVisible();
    },
  },
  {
    id: 'history-revert',
    name: 'Восстановить ревизию',
    dialog: /^Откатить к ревизии \d+\?$/,
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: async (p) => {
      await p.getByRole('tab', { name: 'История' }).click();
      await p.getByRole('button', { name: 'Откатить к этой' }).first().click();
    },
    route: { method: 'POST', url: /\/api\/v1\/nodes\/[^/?]+\/revert$/ },
    after: 'none',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.getByRole('button', { name: 'Откатить к этой' }).first()).toBeVisible();
    },
  },
  {
    id: 'report-delete',
    name: 'Удалить отчёт',
    dialog: 'Удалить отчёт?',
    page: (c) => journal(c, 'reports', c.env('E2E_REPORT_PRINT'), c.solution),
    open: (p) => press(p, 'Удалить'),
    route: DELETE_ENTRY,
    after: 'none',
    confirm: true,
    danger: true,
    // отчёт на месте: адрес прежний, страница отчёта с «Удалить» (запроса `DELETE` не было — общая
    // проверка сценария)
    kept: async (p) => {
      await expect(p).toHaveURL(/\/j\/reports\/[^/?#]+$/);
      await expect(p.getByRole('button', { name: 'Удалить', exact: true }).first()).toBeVisible();
    },
  },
  {
    id: 'cache-note',
    name: 'Превратить выжимку в заметку',
    dialog: 'Превратить в заметку',
    page: () => '#/cache',
    open: (p) => p.locator(`tr[data-cache="${process.env.E2E_FORMS_CACHE_QUERY}"]`).getByRole('button', { name: 'Превратить в заметку' }).click(),
    first: 'Путь заметки',
    fill: (p) => type(p, 'Превратить в заметку', 'Путь заметки', `Формы/Выжимка ${stamp()}`),
    route: { method: 'POST', url: /\/api\/v1\/cache\/[^/?]+\/promote$/ },
    after: 'none',
  },
  {
    id: 'cache-delete',
    name: 'Удалить выжимку',
    page: () => '#/cache',
    open: (p) => p.locator(`tr[data-cache="${process.env.E2E_FORMS_CACHE_QUERY}"]`).getByRole('button', { name: 'Удалить', exact: true }).click(),
    route: { method: 'DELETE', url: /\/api\/v1\/cache\/[^/?]+$/ },
    after: 'none',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.locator(`tr[data-cache="${process.env.E2E_FORMS_CACHE_QUERY}"]`)).toHaveCount(1);
    },
  },
  // ---- администрирование и аккаунт (задача 10b): необратимое — подтверждение `danger` ----
  {
    id: 'user-password',
    name: 'Пароль пользователя',
    get dialog() {
      return `Пароль пользователя «${process.env.E2E_FORMS_USER}»`;
    },
    page: () => '#/admin',
    open: (p) => userRow(p).getByRole('button', { name: 'Сменить пароль' }).click(),
    first: 'Пароль',
    fill: (p) => type(p, `Пароль пользователя «${process.env.E2E_FORMS_USER}»`, 'Пароль', ['forms', 'e2e', 'pass', '2'].join('-')),
    route: { method: 'POST', url: /\/api\/v1\/admin\/users\/[^/?]+\/password$/ },
    after: 'notice',
  },
  {
    id: 'user-close',
    name: 'Закрыть вход пользователя',
    get dialog() {
      return `Закрыть вход «${process.env.E2E_FORMS_USER}» в WebUI`;
    },
    page: () => '#/admin',
    open: (p) => userRow(p).getByRole('button', { name: 'Закрыть вход' }).click(),
    route: { method: 'DELETE', url: /\/api\/v1\/admin\/users\/[^/?]+\/password$/ },
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      const users = await sessionGet<{ name: string; has_password: boolean }[]>(p, 'admin/users');
      expect(users.find((u) => u.name === process.env.E2E_FORMS_USER)?.has_password).toBe(true);
    },
  },
  {
    id: 'user-revoke',
    name: 'Отозвать токен пользователя',
    dialog: 'Отозвать токен?',
    page: () => '#/admin',
    open: async (p) => {
      const row = p.getByRole('row', { name: new RegExp(`^${process.env.E2E_FORMS_USER_TOKEN} `) });
      // раскрытие — переключатель: открытое повторно (сценарий «Назад») уже раскрыто
      if (!(await row.isVisible())) await userRow(p).getByRole('button', { name: 'Токены' }).click();
      await row.getByRole('button', { name: 'Отозвать' }).click();
    },
    route: { method: 'DELETE', url: /\/api\/v1\/admin\/tokens\/[^/?]+$/ },
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      const tokens = await sessionGet<{ name: string; revoked_at: string | null }[]>(p, `admin/users/${process.env.E2E_FORMS_USER_ID}/tokens`);
      expect(tokens.find((t) => t.name === process.env.E2E_FORMS_USER_TOKEN)?.revoked_at).toBeNull();
    },
  },
  {
    id: 'account-revoke',
    name: 'Отозвать свой токен',
    dialog: 'Отозвать токен?',
    page: () => '#/account',
    open: (p) => p.locator(`tr[data-token="${process.env.E2E_FORMS_OWN_TOKEN}"]`).getByRole('button', { name: 'Отозвать' }).click(),
    route: { method: 'DELETE', url: /\/api\/v1\/auth\/tokens\/[^/?]+$/ },
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      const tokens = await sessionGet<{ name: string; revoked_at: string | null }[]>(p, 'auth/tokens');
      expect(tokens.find((t) => t.name === process.env.E2E_FORMS_OWN_TOKEN)?.revoked_at).toBeNull();
    },
  },
  {
    id: 'access-revoke',
    name: 'Отозвать доступ участника',
    dialog: 'Отозвать доступ?',
    page: () => '#/admin/solutions',
    open: async (p) => {
      await accessOf(p, 'E2E_FORMS_USER');
      await p.locator(`tr[data-member="${process.env.E2E_FORMS_USER}"]`).getByRole('button', { name: 'Отозвать' }).click();
    },
    route: { method: 'POST', url: /\/api\/v1\/solutions\/[^/?]+\/access\/[^/?]+\/revoke$/ },
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      const r = await sessionGet<{ members: { user_name: string; state: string }[] }>(p, `solutions/${process.env.E2E_FORMS_ACCESS_SOL}/access`);
      expect(r.members.find((m) => m.user_name === process.env.E2E_FORMS_USER)?.state).toBe('approved');
    },
  },
  {
    id: 'access-reject',
    name: 'Отклонить запрос доступа',
    dialog: 'Отклонить запрос?',
    page: () => '#/admin/solutions',
    open: async (p) => {
      await accessOf(p, 'E2E_FORMS_PENDING_USER');
      await p.locator(`tr[data-member="${process.env.E2E_FORMS_PENDING_USER}"]`).getByRole('button', { name: 'Отклонить' }).click();
    },
    first: 'Причина',
    fill: (p) => type(p, 'Отклонить запрос?', 'Причина', 'решение закрытое'),
    route: { method: 'POST', url: /\/api\/v1\/solutions\/[^/?]+\/access\/[^/?]+\/reject$/ },
    after: 'notice',
  },
  {
    id: 'template-reset',
    name: 'Сбросить шаблон к умолчанию',
    dialog: 'Сбросить шаблон к умолчанию?',
    page: () => '#/admin/settings',
    // свой шаблон — подменой ответа настроек: общий шаблон сервера сценарии форм не меняют
    prepare: (p) => customTemplate(p),
    open: (p) => p.getByRole('button', { name: 'Сбросить к умолчанию' }).click(),
    route: SETTINGS_PATCH,
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.getByRole('region', { name: 'Шаблон инструкции' })).toContainText('свой шаблон');
    },
  },
  {
    id: 'template-edit',
    name: 'Шаблон инструкции (поле на виду)',
    page: () => '#/admin/settings',
    open: async (p) => {
      const area = p.getByLabel('Текст шаблона');
      await expect(area).toBeVisible();
      // поле на странице ниже первого экрана — к нему прокручивает человек: здесь — в середину окна
      await area.evaluate((e) => e.scrollIntoView({ block: 'center' }));
    },
    first: 'Текст шаблона',
    fill: (p) => p.getByLabel('Текст шаблона').fill(`## Шаблон форм E2E ${stamp()}\nАдрес: {url}\nРешение: {solution}. {rules}`),
    route: SETTINGS_PATCH,
    after: 'notice',
    legacy: 'Шаблон инструкции',
    inline: { scope: (p) => p.getByRole('region', { name: 'Шаблон инструкции' }), submit: 'Сохранить', focus: false },
  },
  // ---- импорт, копии, индекс (задача 10b): свои окна — на каркасе `FormDialog` ----
  {
    id: 'import',
    name: 'Импорт архива',
    page: (c) => `#/u/${c.user}`,
    open: (p) => treeMenu(p, 'Knowledge', 'Импорт (.zip)…'),
    first: 'Архив (.zip)',
    // выбранный архив — несохранённое окна (ревью r1 задачи 10b, ⚠️ 2); отказ приёма — в окне
    fill: (p) => dialogOf(p, 'Импорт архива').getByLabel('Архив (.zip)').setInputFiles(emptyZip()),
    route: { method: 'POST', url: /\/api\/v1\/spaces\/[^/?]+\/import(\?|$)/ },
    after: 'none',
  },
  {
    id: 'backup-run',
    name: 'Снять копию сейчас',
    dialog: 'Снять копию сейчас?',
    page: () => '#/admin/backups',
    open: (p) => press(p, 'Снять копию сейчас'),
    route: { method: 'POST', url: /\/api\/v1\/admin\/backups$/ },
    after: 'notice',
    confirm: true,
  },
  {
    id: 'backup-delete',
    name: 'Удалить копию',
    dialog: 'Удалить копию?',
    page: () => '#/admin/backups',
    // копия в списке — подменой ответа (снимать настоящую копию для окна долго; ackups.spec.ts
    // удаляет настоящую)
    prepare: (p) => fakeBackup(p),
    open: (p) => p.getByRole('button', { name: `Удалить копию ${FAKE_BACKUP}` }).click(),
    route: { method: 'DELETE', url: /\/api\/v1\/admin\/backups\/[^/?]+$/ },
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.getByRole('button', { name: `Удалить копию ${FAKE_BACKUP}` })).toBeVisible();
    },
  },
  {
    id: 'reindex',
    name: 'Переиндексация из embeddings',
    dialog: 'Переиндексировать из embeddings?',
    page: () => '#/admin/index',
    open: (p) => press(p, 'Переиндексировать из embeddings'),
    route: REINDEX,
    after: 'notice',
    confirm: true,
  },
  {
    id: 'reindex-full',
    name: 'Полная переиндексация',
    dialog: 'Запустить полную переиндексацию?',
    page: () => '#/admin/index',
    open: (p) => press(p, 'Полная переиндексация'),
    route: REINDEX,
    after: 'notice',
    confirm: true,
    danger: true,
    kept: async (p) => {
      await expect(p.getByTestId('reindex-status')).toHaveCount(0);
    },
  },
  {
    id: 'idea-delete',
    name: 'Удалить идею',
    dialog: 'Удалить идею?',
    page: (c) => journal(c, 'ideas', c.env('E2E_FORMS_IDEA_DECLINED')),
    open: (p) => pressIn(p, IDEA, 'Удалить'),
    route: DELETE_ENTRY,
    after: 'none',
    confirm: true,
    danger: true,
  },
];

/** Дерево заметок: на узком экране — в выдвижной панели. */
async function treeMenu(p: Page, target: string, item: string): Promise<void> {
  const tree = p.locator('nav[aria-label="Дерево заметок"]');
  const toggle = p.getByRole('button', { name: 'Показать дерево' });
  if (await toggle.isVisible()) await toggle.click();
  await tree.getByRole('button', { name: `Действия: ${target}` }).first().click();
  await p.getByRole('menuitem', { name: item }).click();
}

/** Ответ `GET /admin/settings` — с «своим» шаблоном Claude Code · ru (кнопка «Сбросить к умолчанию»
 * доступна), запросы прочих методов — дальше по цепочке подмен. */
export async function customTemplate(p: Page): Promise<void> {
  await p.route(/\/api\/v1\/admin\/settings$/, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const r = await route.fetch();
    const j = (await r.json()) as { agent_setup: { templates: Record<string, { text: string; custom: boolean }> } };
    j.agent_setup.templates['claude.ru'].custom = true;
    await route.fulfill({ response: r, json: j });
  });
}
/** Копия каталога копий в ответе `GET /admin/backups` (подмена): окно «Удалить копию?» без
 * настоящей копии. */
export const FAKE_BACKUP = '2026-01-02_030405-e2e-forms';
async function fakeBackup(p: Page): Promise<void> {
  await p.route(/\/api\/v1\/admin\/backups$/, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const r = await route.fetch();
    const j = (await r.json()) as { items: unknown[] };
    j.items = [
      { name: FAKE_BACKUP, created_at: '2026-01-02T03:04:05Z', server_version: '0.0.16', bytes: 4096, files: 3, dbs: 2, blobs: 0, drift: [] },
      ...j.items,
    ];
    await route.fulfill({ response: r, json: j });
  });
}

/** Пустой архив `.zip` (только конец каталога) для поля «Архив (.zip)» окна импорта. */
function emptyZip(): string {
  const dir = fileURLToPath(new URL('../../.tmp/e2e/', import.meta.url));
  mkdirSync(dir, { recursive: true });
  const f = `${dir}forms-empty.zip`;
  writeFileSync(f, Buffer.from('PK\u0005\u0006'.padEnd(22, '\u0000'), 'latin1'));
  return f;
}