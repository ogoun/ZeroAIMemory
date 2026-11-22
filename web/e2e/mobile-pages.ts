// Общий список адресов обхода узких экранов (M7.2, спецификация §5.1 «Проверка», §5.2 п. 7):
// задача, добавившая страницу или журнал, дописывает её сюда — перед последним элементом или на
// своё место (не в хвост: параллельные задачи иначе правят одну строку).
import { expect, type Page } from '@playwright/test';
import { SOLUTION_GUID } from './seed';

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
}

export const PAGES: MobilePage[] = [
  { name: 'стартовая', path: (c) => `#/u/${c.user}` },
  { name: 'документ', path: (c) => `#/u/${c.user}/s/knowledge/n/${c.env('E2E_NOTE_MEDIA')}` },
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
