// Передача только что выпущенного токена на страницу «Подключение агента» (§15.5): страница
// открывается сразу после создания пользователя или выпуска токена. Открытый текст токена
// живёт только в памяти вкладки и забирается страницей один раз — в адрес, историю и
// хранилище браузера он не попадает.
import type { IssuedToken } from './api';
import { go } from './router';

let pending: { user: string; token: IssuedToken } | null = null;

/** Открыть «Подключение агента» для пользователя `user` с новым токеном. */
export function openAgentSetup(user: string, token: IssuedToken): void {
  pending = { user, token };
  go({ name: 'agentsetup' });
}

/** Забрать переданный токен (один раз). */
export function takeIssued(): { user: string; token: IssuedToken } | null {
  const p = pending;
  pending = null;
  return p;
}
