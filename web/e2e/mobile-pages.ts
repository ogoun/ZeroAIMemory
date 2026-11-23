// Общий список адресов обхода узких экранов (M7.2, спецификация §5.1 «Проверка», §5.2 п. 7):
// задача, добавившая страницу или журнал, дописывает её сюда — перед последним элементом или на
// своё место (не в хвост: параллельные задачи иначе правят одну строку).
import { expect, type Page } from '@playwright/test';
import { IDEAS_GUID, SOLUTION_GUID } from './seed';

/** Контекст адреса: свой пользователь сессии, решение E2E и переменные `E2E_*` global-setup. */
export interface PageCtx {
  /** id пользователя сессии (администратор или пользователь — по `as`). */
  user: string;
  /** GUID решения E2E (`seed.ts`). */
  solution: string;
  /** Значение `E2E_*` из global-setup (нет — ошибка). */
  env: (k: string) => string;
}

export interface MobilePage {
  name: string;
  /** Адрес от корня приложения: `#/…`. */
  path: (c: PageCtx) => string;
  /** Чья сессия: по умолчанию — администратор (`ADMIN_STATE`). */
  as?: 'admin' | 'user';
  /** Вкладки страницы (роль `tab`), которые обход открывает по очереди и проверяет так же. */
  tabs?: string[];
  /** Кнопки (первая с таким именем), которые обход нажимает по очереди — раскрытые строки таблиц. */
  buttons?: string[];
}

/**
 * Журналы решения E2E и Knowledge (`journalsOf`, lib/journals.ts) — для обхода §5.2 п. 7; у решения —
 * кроме названных в списке отдельно («задачи», «insights», «журнал» — Словарь).
 */
const SOLUTION_JOURNALS = ['changelog', 'releases', 'reports', 'code', 'ideas', 'questions', 'vault'];
const KNOWLEDGE_JOURNALS = ['insights', 'glossary', 'tasks', 'ideas', 'questions', 'vault'];

/** Вложение заметки `E2E_NOTE_DOCS` (seed-docs.ts): `sha` — переменная `E2E_*`, имя — файла. */
const attachment = (c: PageCtx, sha: string, file: string) =>
  `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_DOCS')}/a/${c.env(sha)}/${encodeURIComponent(file)}`;

export const PAGES: MobilePage[] = [
  { name: 'стартовая', path: (c) => `#/u/${c.user}` },
  // «Сессии» (задача 9): список с субагентами под родителем и блок «Без сессии»
  { name: 'сессии', path: () => '#/sessions' },
  { name: 'документ', path: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}` },
  // журналы «список — карточка» и доска задач (задача 2, §5.2 пп. 1–3)
  { name: 'задачи', path: (c) => `#/u/${c.user}/s/${c.solution}/j/tasks` },
  { name: 'идеи', path: (c) => `#/u/${c.user}/s/${IDEAS_GUID}/j/ideas` },
  { name: 'insights', path: (c) => `#/u/${c.user}/s/${c.solution}/j/insights` },
  // прочие страницы (задача 3, §5.2 пп. 4, 7): все журналы решения и Knowledge, документ с широкой
  // таблицей и кодом, вложения и просмотрщики, страница отчёта, администрирование, «Аккаунт» и
  // «Мои решения» пользователя, «Подключение агента», «Кэш»
  ...SOLUTION_JOURNALS.map((j) => ({ name: `решение-${j}`, path: (c: PageCtx) => `#/u/${c.user}/s/${c.solution}/j/${j}` })),
  ...KNOWLEDGE_JOURNALS.map((j) => ({ name: `knowledge-${j}`, path: (c: PageCtx) => `#/u/${c.user}/s/knowledge/j/${j}` })),
  {
    name: 'документ-таблица-код',
    path: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_WIDE')}`,
    tabs: ['История', 'Связи'],
  },
  { name: 'вложения', path: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_DOCS')}`, tabs: ['Вложения'] },
  { name: 'вложение-xlsx', path: (c) => attachment(c, 'E2E_XLSX_SHA', 'таблица.xlsx') },
  { name: 'вложение-docx', path: (c) => attachment(c, 'E2E_DOCX_SHA', 'договор.docx') },
  { name: 'вложение-pdf', path: (c) => attachment(c, 'E2E_PDF_SHA', 'отчёт.pdf') },
  { name: 'вложение-pptx', path: (c) => attachment(c, 'E2E_PPTX_SHA', 'доклад.pptx') },
  { name: 'отчёт', path: (c) => `#/u/${c.user}/s/${c.solution}/j/reports/${c.env('E2E_REPORT_PRINT')}` },
  // раскрытые строки: токены пользователя, участники решения (вложенные таблицы и форма в ячейке)
  { name: 'пользователи', path: () => '#/admin', buttons: ['Токены'] },
  { name: 'решения', path: () => '#/admin/solutions', buttons: ['Доступ'] },
  { name: 'настройки', path: () => '#/admin/settings' },
  // «Копии» и «Аудит» администратора (задача 7, §5.6 пп. 2, 4): плашка `no_recent_backup`, таблицы
  // копий и событий — карточками на телефоне
  { name: 'копии', path: () => '#/admin/backups' },
  { name: 'аудит', path: () => '#/admin/audit' },
  { name: 'индекс', path: () => '#/admin/index' },
  { name: 'подключение-агента', path: () => '#/agent-setup' },
  { name: 'кэш', path: () => '#/cache' },
  { name: 'аккаунт', path: () => '#/account', as: 'user' },
  { name: 'мои-решения', path: () => '#/my/solutions', as: 'user' },
  { name: 'журнал', path: (c) => `#/u/${c.user}/s/${c.solution}/j/glossary` },
];

export const env = (k: string): string => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};

/** Контекст адресов для сессии страницы `page` (её cookie): пользователь — из `GET /auth/me`. */
export async function pageCtx(page: Page): Promise<PageCtx> {
  const r = await page.request.get(`${env('E2E_BASE_URL')}api/v1/auth/me`);
  expect(r.ok(), `auth/me: HTTP ${r.status()}`).toBe(true);
  const me = (await r.json()) as { user_id: string };
  return { user: me.user_id, solution: SOLUTION_GUID, env };
}
