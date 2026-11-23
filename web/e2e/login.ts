// Вход пользователя WebUI из сценариев e2e по API — с повтором при 429 (M7.2, ревью r1 задачи 7).
// Лимит входа сервера — 5 попыток в минуту на IP, скользящее окно (`crates/zam-server/src/auth/
// rate.rs`), общий у входа и смены пароля и у всех спек прогона (`workers: 1`): соседние спеки в
// ту же минуту его выбирают. При 429 помощник ждёт `Retry-After` и повторяет запрос один раз,
// продлив таймаут сценария на время ожидания; повтор с 429 — ошибка. Лимит сервера не меняется.
// Вход через форму с повтором — `submitRetry429` в `user-login.spec.ts`.
import { expect, test, type APIRequestContext, type APIResponse } from '@playwright/test';

/** POST под лимитом входа (`auth/login`, `auth/password`): при 429 — ждать `Retry-After` и
 * повторить один раз. */
export async function postLimited(
  rc: APIRequestContext,
  url: string,
  opts: { data?: unknown; headers?: Record<string, string> } = {},
): Promise<APIResponse> {
  const r = await rc.post(url, opts);
  if (r.status() !== 429) return r;
  const wait = Number(r.headers()['retry-after'] ?? '60');
  expect(wait, 'Retry-After').toBeGreaterThan(0);
  // ожидание не съедает таймаут сценария (по умолчанию 60 с — столько же, сколько окно лимита)
  const info = test.info();
  info.setTimeout(info.timeout + wait * 1000 + 10_000);
  await new Promise((res) => setTimeout(res, wait * 1000 + 500));
  const again = await rc.post(url, opts);
  expect(again.status(), `${url}: 429 и после Retry-After`).not.toBe(429);
  return again;
}

/**
 * Вход пользователя `login` с паролем `first`, заданным администратором, и обязательная смена на
 * `next` (оба запроса — под лимитом входа, {@link postLimited}). Сессия остаётся в cookie
 * контекста `rc`; возвращает её CSRF-токен.
 */
export async function loginChangingPassword(
  rc: APIRequestContext,
  base: string,
  login: string,
  first: string,
  next: string,
): Promise<string> {
  const r = await postLimited(rc, `${base}api/v1/auth/login`, { data: { login, password: first } });
  expect(r.status(), `вход ${login}`).toBe(200);
  const { csrf } = (await r.json()) as { csrf: string };
  const pw = await postLimited(rc, `${base}api/v1/auth/password`, {
    headers: { 'X-CSRF-Token': csrf },
    data: { old: first, new: next },
  });
  expect(pw.status(), `смена пароля ${login}`).toBe(204);
  return csrf;
}
