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

/** Заголовок выбора пользователя: администратор действует от имени `user`. */
const USER_HEADER = 'X-Zam-User';

async function call<T>(method: string, path: string, body?: unknown, user?: string): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET' && csrf) headers['X-CSRF-Token'] = csrf;
  if (user) headers[USER_HEADER] = user;
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

export interface Me {
  login: string;
  user_id: string;
  csrf: string;
}

export const me = () => call<Me>('GET', 'auth/me');

/** Восстанавливает вход по живой cookie-сессии (после F5); `null` — нужен экран входа. */
export async function restore(): Promise<Me | null> {
  try {
    const r = await me();
    csrf = r.csrf;
    return r;
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) return null;
    throw e;
  }
}
export const listUsers = () => call<User[]>('GET', 'admin/users');
export const listTokens = (userId: string) =>
  call<TokenInfo[]>('GET', `admin/users/${userId}/tokens`);
export const createUser = (name: string) =>
  call<{ user: User; token: IssuedToken }>('POST', 'admin/users', { name });
export const issueToken = (userId: string, name: string, access: Access) =>
  call<IssuedToken>('POST', `admin/users/${userId}/tokens`, { name, access });
export const revokeToken = (id: string) => call<void>('DELETE', `admin/tokens/${id}`);

// ---- пространства и узлы (администратор — от имени пользователя `user`) ----

export type SpaceKind = 'rules' | 'human' | 'knowledge' | 'solution';

export interface Space {
  id: string;
  kind: SpaceKind;
  solution_guid: string | null;
  name: string;
}

export type NodeKind = 'note' | 'folder';

/** `pending` | `indexed` | `failed` | `excluded`. */
export type IndexState = string;

export interface NodeInfo {
  id: string;
  space_id: string;
  parent_id: string | null;
  kind: NodeKind;
  name: string;
  path: string;
  current_rev: number;
  content_hash: string | null;
  frontmatter: Record<string, unknown> | null;
  l0: string | null;
  l0_source: string | null;
  index_state: IndexState;
  created_at: string;
  updated_at: string;
  deleted_at: string | null;
  has_children: boolean;
}

export interface OutlineItem {
  level: number;
  title: string;
  anchor: string;
  path: string[];
}

export interface RenderedNode {
  html: string;
  toc: OutlineItem[];
  warnings: string[];
  rev: number;
}

/** Ссылка на пространство в API и в адресе страницы: вид фиксированного, GUID решения или id. */
export function spaceRef(s: Space): string {
  if (s.kind !== 'solution') return s.kind;
  return s.solution_guid ?? s.id;
}

/** Пространство соответствует ссылке `ref` (форма ссылки — как у `spaceRef`, либо id). */
export function spaceMatches(s: Space, ref: string): boolean {
  const r = ref.toLowerCase();
  return spaceRef(s).toLowerCase() === r || s.id.toLowerCase() === r;
}

const q = encodeURIComponent;

export const listSpaces = (user: string) =>
  call<{ user: { id: string; name: string }; access: string; spaces: Space[] }>(
    'GET',
    'spaces',
    undefined,
    user,
  );
export const listChildren = (user: string, space: string, path: string) =>
  call<{ space: Space; items: NodeInfo[] }>(
    'GET',
    `spaces/${q(space)}/tree?path=${q(path)}&depth=1`,
    undefined,
    user,
  );
export const getNode = (user: string, id: string) =>
  call<{ node: NodeInfo; content: string | null; rev: number }>(
    'GET',
    `nodes/${q(id)}?format=json`,
    undefined,
    user,
  );
export const getNodeHtml = (user: string, id: string) =>
  call<RenderedNode>('GET', `nodes/${q(id)}?format=html`, undefined, user);
export const createNode = (
  user: string,
  req: { space: string; path: string; kind: NodeKind; content?: string },
) => call<NodeInfo>('POST', 'nodes', req, user);
export const moveNode = (user: string, id: string, path: string) =>
  call<NodeInfo>('POST', `nodes/${q(id)}/move`, { path }, user);
export const deleteNode = (user: string, id: string) =>
  call<void>('DELETE', `nodes/${q(id)}`, undefined, user);
export const restoreNode = (user: string, id: string) =>
  call<NodeInfo>('POST', `nodes/${q(id)}/restore`, undefined, user);
export const listTrash = (user: string, space?: string) =>
  call<{ items: NodeInfo[] }>(
    'GET',
    space ? `trash?space=${q(space)}` : 'trash',
    undefined,
    user,
  );
