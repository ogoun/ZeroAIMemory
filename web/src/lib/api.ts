// Клиент REST API. Пути относительные: разрешаются от <base href>, который подставляет сервер.

export interface User {
  id: string;
  name: string;
  is_admin: boolean;
  created_at: string;
  disabled_at: string | null;
  tokens: number;
}

export type Access = 'rw' | 'ro';

export interface TokenInfo {
  id: string;
  user_id: string;
  name: string;
  prefix: string;
  access: Access;
  created_at: string;
  last_used_at: string | null;
  last_ip: string | null;
  revoked_at: string | null;
}

export interface IssuedToken {
  info: TokenInfo;
  plaintext: string;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

// CSRF-токен живёт только в памяти (приходит в ответе на вход).
let csrf = '';

export const hasSession = (): boolean => csrf !== '';

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET' && csrf) headers['X-CSRF-Token'] = csrf;
  const resp = await fetch(`api/v1/${path}`, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!resp.ok) {
    let detail = resp.statusText;
    try {
      const p = await resp.json();
      if (typeof p.detail === 'string') detail = p.detail;
    } catch {
      /* тело не JSON */
    }
    throw new ApiError(resp.status, detail);
  }
  if (resp.status === 204) return undefined as T;
  return (await resp.json()) as T;
}

export async function login(loginName: string, password: string): Promise<void> {
  const r = await call<{ csrf: string }>('POST', 'auth/login', { login: loginName, password });
  csrf = r.csrf;
}

export async function logout(): Promise<void> {
  try {
    await call<void>('POST', 'auth/logout');
  } finally {
    csrf = '';
  }
}

export const me = () => call<{ login: string; user_id: string }>('GET', 'auth/me');
export const listUsers = () => call<User[]>('GET', 'admin/users');
export const listTokens = (userId: string) =>
  call<TokenInfo[]>('GET', `admin/users/${userId}/tokens`);
export const createUser = (name: string) =>
  call<{ user: User; token: IssuedToken }>('POST', 'admin/users', { name });
export const issueToken = (userId: string, name: string, access: Access) =>
  call<IssuedToken>('POST', `admin/users/${userId}/tokens`, { name, access });
export const revokeToken = (id: string) => call<void>('DELETE', `admin/tokens/${id}`);
