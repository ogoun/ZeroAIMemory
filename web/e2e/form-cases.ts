// Перечень мест, где пользователь пишет (M7.3, спецификация §6.14 п. 9; задача владельца
// k:01M85AS1C0EZVV95FQZ2ZEQCET): одна строка на форму. `forms.spec.ts` проходит каждую строку в трёх
// ширинах (`phone` с касанием, `tablet`, `desktop`), открытие с фокусом — ещё и в `tabletLand`:
// окно видно и в фокусе, Tab не выходит из окна, Esc без набранного закрывает и возвращает фокус,
// несохранённое спрашивает внутри окна, ошибка сервера (ответ 422 на `route`) — в окне, поля целы,
// после «Добавить» (`after: 'card'`) — карточка созданного на виду и в фокусе.
//
// Строки задачи 10a — Задачи, Insights, Идеи (ядро). Задачи 10b и 8 дописывают свои строки — перед
// последним элементом массива (не в хвост: параллельные задачи иначе правят одну строку). Данные
// строк — `E2E_FORMS_*` (наполнение — `seedForms` в `forms.spec.ts`) и `E2E_*` global-setup.
//
// `SNAP_ONLY` — места перечня вне ядра (задача 10b) для снимков «до» (`E2E_FORMS_SNAP=before`):
// только как открыть форму на 0.0.15, без проверок; 10b заменяет их полными строками.
import { expect, type Locator, type Page } from '@playwright/test';
import { IDEAS_GUID } from './seed';
import type { PageCtx } from './mobile-pages';

export interface FormCase {
  /** Латиница без пробелов: имя теста и файла снимка (`<id>-<ширина>.png`). */
  id: string;
  /** Форма перечня §6.14 (по-русски). */
  name: string;
  /** Имя окна для чтения с экрана (`getByRole('dialog', {name})`); по умолчанию — `name`. */
  dialog?: string;
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
  /** Подтверждение необратимого: фокус — на «Отмена». */
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
}

/** Окно формы по имени. */
export const dialogOf = (p: Page, name: string): Locator => p.getByRole('dialog', { name, exact: true });

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

/** Место перечня вне ядра для снимков «до» (задача 10b заменит полными строками). */
export interface SnapCase {
  id: string;
  name: string;
  page: (c: PageCtx) => string;
  open: (p: Page) => Promise<void>;
  /** `aria-label` формы в карточке или ленте (не окно). */
  legacy?: string;
  as?: 'admin' | 'user';
}

/** Дерево заметок: на узком экране — в выдвижной панели. */
async function treeMenu(p: Page, target: string, item: string): Promise<void> {
  const tree = p.locator('nav[aria-label="Дерево заметок"]');
  const toggle = p.getByRole('button', { name: 'Показать дерево' });
  if (await toggle.isVisible()) await toggle.click();
  await tree.getByRole('button', { name: `Действия: ${target}` }).first().click();
  await p.getByRole('menuitem', { name: item }).click();
}

export const SNAP_ONLY: SnapCase[] = [
  {
    id: 'term-new',
    name: 'Новый термин',
    page: (c) => journal(c, 'glossary'),
    open: (p) => press(p, 'Новый термин'),
    legacy: 'Новый термин',
  },
  {
    id: 'term-edit',
    name: 'Правка термина',
    page: (c) => journal(c, 'glossary', c.env('E2E_FORMS_TERM')),
    open: (p) => pressIn(p, 'Карточка термина', 'Править'),
    legacy: 'Правка термина',
  },
  {
    id: 'term-retract',
    name: 'Отозвать термин',
    page: (c) => journal(c, 'glossary', c.env('E2E_FORMS_TERM')),
    open: (p) => pressIn(p, 'Карточка термина', 'Отозвать'),
    legacy: 'Отзыв термина',
  },
  {
    id: 'question-edit',
    name: 'Правка вопроса',
    page: (c) => journal(c, 'questions', c.env('E2E_FORMS_QUESTION')),
    open: (p) => p.locator(`[data-question="${process.env.E2E_FORMS_QUESTION}"]`).getByRole('button', { name: /^Изменить «/ }).click(),
    legacy: 'Правка вопроса',
  },
  {
    id: 'question-obsolete',
    name: 'Вопрос неактуален',
    page: (c) => journal(c, 'questions', c.env('E2E_FORMS_QUESTION')),
    open: (p) => p.locator(`[data-question="${process.env.E2E_FORMS_QUESTION}"]`).getByRole('button', { name: /^Неактуален «/ }).click(),
  },
  {
    id: 'question-defer',
    name: 'Отложить до (вопрос-предложение)',
    page: (c) => journal(c, 'questions', c.env('E2E_IDEA_QUESTION'), IDEAS_GUID),
    open: (p) => p.getByRole('button', { name: /^отложить/i }).first().click(),
    legacy: 'Отложить идею',
  },
  {
    id: 'changelog-edit',
    name: 'Правка записи Changelog',
    page: (c) => journal(c, 'changelog', c.env('E2E_FORMS_CHANGELOG')),
    open: (p) => p.locator(`article[data-entry="${process.env.E2E_FORMS_CHANGELOG}"]`).first().getByRole('button', { name: /^Изменить «/ }).click(),
    legacy: 'Правка записи',
  },
  {
    id: 'changelog-delete',
    name: 'Удалить запись журнала',
    page: (c) => journal(c, 'changelog', c.env('E2E_FORMS_CHANGELOG')),
    open: (p) => p.locator(`article[data-entry="${process.env.E2E_FORMS_CHANGELOG}"]`).first().getByRole('button', { name: /^Удалить «/ }).click(),
  },
  {
    id: 'release-edit',
    name: 'Правка релиза',
    page: (c) => journal(c, 'releases', c.env('E2E_FORMS_RELEASE')),
    open: (p) => press(p, 'Изменить'),
    legacy: 'Правка релиза',
  },
  { id: 'note-new', name: 'Новая заметка', page: (c) => `#/u/${c.user}`, open: (p) => treeMenu(p, 'Knowledge', 'Новая заметка') },
  { id: 'folder-new', name: 'Новая папка', page: (c) => `#/u/${c.user}`, open: (p) => treeMenu(p, 'Knowledge', 'Новая папка') },
  { id: 'import', name: 'Импорт архива', page: (c) => `#/u/${c.user}`, open: (p) => treeMenu(p, 'Knowledge', 'Импорт (.zip)…') },
  {
    id: 'note-delete',
    name: 'Удалить заметку',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: (p) => treeMenu(p, 'Медиа-заметка', 'Удалить'),
  },
  {
    id: 'note-move',
    name: 'Переименовать или переместить',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: (p) => treeMenu(p, 'Медиа-заметка', 'Переименовать/переместить'),
  },
  {
    id: 'attachment-detach',
    name: 'Открепить вложение',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_DOCS')}`,
    open: async (p) => {
      await p.getByRole('tab', { name: 'Вложения' }).click();
      await p.getByRole('button', { name: /^Открепить / }).first().click();
    },
  },
  {
    id: 'history-revert',
    name: 'Восстановить ревизию',
    page: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}`,
    open: async (p) => {
      await p.getByRole('tab', { name: 'История' }).click();
      await p.getByRole('button', { name: 'Откатить к этой' }).first().click();
    },
  },
  {
    id: 'report-delete',
    name: 'Удалить отчёт',
    page: (c) => journal(c, 'reports', c.env('E2E_REPORT_PRINT'), c.solution),
    open: (p) => press(p, 'Удалить'),
  },
  { id: 'user-password', name: 'Пароль пользователя', page: () => '#/admin', open: (p) => p.getByRole('button', { name: /^(Сменить|Задать) пароль$/ }).first().click() },
  { id: 'backup-run', name: 'Снять копию сейчас', page: () => '#/admin/backups', open: (p) => press(p, 'Снять копию сейчас') },
  { id: 'reindex', name: 'Переиндексация', page: () => '#/admin/index', open: (p) => press(p, 'Переиндексировать из embeddings') },
];
