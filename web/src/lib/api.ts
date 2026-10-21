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
    /** Тело problem+json (RFC 9457) с дополнительными полями, если пришло. */
    public problem: Record<string, unknown> | null = null,
  ) {
    super(message);
  }
}

// CSRF-токен живёт только в памяти (приходит в ответе на вход).
let csrf = '';

export const hasSession = (): boolean => csrf !== '';

/** Заголовок выбора пользователя: администратор действует от имени `user`. */
const USER_HEADER = 'X-Zam-User';

interface Req {
  body?: unknown;
  user?: string;
  headers?: Record<string, string>;
}

/** Запрос к API: CSRF для изменяющих методов, выбор пользователя, разбор problem+json. */
async function request(method: string, path: string, r: Req = {}): Promise<Response> {
  const headers: Record<string, string> = { ...r.headers };
  let body: BodyInit | undefined;
  if (r.body instanceof FormData) {
    // Content-Type с boundary ставит браузер
    body = r.body;
  } else if (r.body !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(r.body);
  }
  if (method !== 'GET' && csrf) headers['X-CSRF-Token'] = csrf;
  if (r.user) headers[USER_HEADER] = r.user;
  const resp = await fetch(`api/v1/${path}`, {
    method,
    headers,
    credentials: 'same-origin',
    body,
  });
  if (!resp.ok) {
    let detail = resp.statusText;
    let problem: Record<string, unknown> | null = null;
    try {
      const p = await resp.json();
      if (p && typeof p === 'object') problem = p as Record<string, unknown>;
      if (typeof p.detail === 'string') detail = p.detail;
    } catch {
      /* тело не JSON */
    }
    throw new ApiError(resp.status, detail, problem);
  }
  return resp;
}

async function call<T>(method: string, path: string, body?: unknown, user?: string): Promise<T> {
  const resp = await request(method, path, { body, user });
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

// ---- правка, история, вложения, поиск ----

/** Ответ 409 на замену: текущая ревизия и дифф от моей базовой к ней. */
export interface Conflict {
  current_rev: number;
  diff: string;
}

/** Конфликт ревизий из ошибки `PUT` (409 с `current_rev`); иначе `null`. */
export function conflictOf(e: unknown): Conflict | null {
  if (!(e instanceof ApiError) || e.status !== 409 || !e.problem) return null;
  const rev = e.problem.current_rev;
  if (typeof rev !== 'number') return null;
  const diff = e.problem.diff;
  return { current_rev: rev, diff: typeof diff === 'string' ? diff : '' };
}

/** `PUT /nodes/{id}` с `If-Match: <rev>`: замена текста целиком. */
export const replaceNode = async (user: string, id: string, rev: number, content: string) =>
  (await (
    await request('PUT', `nodes/${q(id)}`, {
      body: { content },
      user,
      headers: { 'If-Match': `"${rev}"` },
    })
  ).json()) as NodeInfo;

export interface Preview {
  html: string;
  toc: OutlineItem[];
  warnings: string[];
}

/** Предпросмотр редактора: рендер в контексте заметки (вики-ссылки, вложения). */
export const renderPreview = (user: string, md: string, baseNode: string) =>
  call<Preview>('POST', 'render', { md, base_node: baseNode }, user);

export interface UploadedBlob {
  sha256: string;
  link: string;
  mime: string;
  size: number;
}

/** `POST /blobs`: поле `node` строго перед `file` (так читает сервер). */
export function uploadBlob(user: string, node: string, file: File): Promise<UploadedBlob> {
  const form = new FormData();
  form.append('node', node);
  form.append('file', file, file.name);
  return request('POST', 'blobs', { body: form, user }).then((r) => r.json() as Promise<UploadedBlob>);
}

export interface RevisionInfo {
  rev: number;
  content_hash: string;
  size: number;
  /** `human` | `agent` | `system`. */
  author_kind: string;
  token_id: string | null;
  session_id: string | null;
  machine: string | null;
  message: string | null;
  ts: string;
}

export const listRevisions = (user: string, id: string) =>
  call<RevisionInfo[]>('GET', `nodes/${q(id)}/revisions`, undefined, user);

/** Унифицированный дифф `from → to` (текст). */
export const getDiff = async (user: string, id: string, from: number, to: number) =>
  (await request('GET', `nodes/${q(id)}/diff?from=${from}&to=${to}`, { user })).text();

export const revertNode = (user: string, id: string, rev: number) =>
  call<NodeInfo>('POST', `nodes/${q(id)}/revert`, { rev }, user);

export const getOutline = (user: string, id: string) =>
  call<OutlineItem[]>('GET', `nodes/${q(id)}/outline`, undefined, user);

export interface SearchItem {
  node_id: string;
  space: Space | null;
  path: string;
  heading_path: string;
  rev: number;
  /** Совпадения обрамлены U+E000 … U+E001. */
  snippet: string;
  score: number;
  /** Каналы, нашедшие элемент: `vector`, `fts`, `trigram`. */
  sources: string[];
}

export interface SearchResult {
  items: SearchItem[];
  /** Векторный канал выключен: выдача только полнотекстовая. */
  degraded: boolean;
  /** Причина деградации (`no_model`, `qdrant_down`, …) или `null`. */
  degraded_reason: string | null;
  /** Та же причина по-русски (словарь сервера). */
  degraded_text?: string | null;
  /** Предупреждения (пока только о реранке). */
  warnings: string[];
}

/** Гибридный поиск по пространствам пользователя (Human — только администратору). */
export const search = (user: string, query: string, spaces: string[]) =>
  call<SearchResult>(
    'POST',
    'search',
    {
      query,
      scope: { spaces, include_human: true },
      mode: 'brief',
      top_k: 20,
      // brief: путь, заголовок и сниппет (≤ 160 символов) — до ~600 символов на
      // элемент; 4000 токенов × 4 символа с запасом вмещают все top_k
      budget_tokens: 4000,
      format: 'json',
    },
    user,
  );

// ---- векторный индекс (администратор) ----

export interface ReindexOutcome {
  user: string | null;
  space: string | null;
  full: boolean;
  finished_at: string;
  result?: {
    users: number;
    nodes: number;
    points: number;
    folders: number;
    misses: number;
    folder_misses: number;
    jobs: number;
    gc_deleted: number;
    recreated: boolean;
    failed_users: { user_id: string; error: string }[];
    elapsed_ms: number;
  };
  error?: string;
}

/** Ответ `GET /index/status` (crates/zam-server/src/api/index.rs). */
export interface IndexStatus {
  /** `text` — причина по-русски, `hint` — что сделать (команды в обратных кавычках). */
  vector: { available: boolean; reason: string | null; text?: string | null; hint: string | null };
  /** `kind`: `cpu` | `cuda` | `null` (модель не загружена). */
  device: { kind: string | null; reason: string };
  model: { id: string | null; dim: number; collection_model: string | null; error: string | null };
  qdrant: {
    /** `off` | `starting` | `ready` | `restarting` | `broken` | `external`. */
    state: string;
    version: string | null;
    rss_bytes: number | null;
    points: number | null;
    restarts: number;
    error: string | null;
  };
  queue: { queued: number; running: number; failed: number; oldest: string | null };
  failed_items: {
    /** Id задания: у одного узла неудавшихся заданий может быть несколько. */
    id: number;
    user_id: string;
    node_id: string;
    /** Пространство и путь узла; `null` — узел удалён насовсем. */
    space_id: string | null;
    path: string | null;
    /** Узел в корзине (время удаления); `null` — жив или удалён насовсем. */
    deleted_at: string | null;
    op: string;
    error: string;
    attempts: number;
    updated_at: string;
  }[];
  reindex: { running: boolean; queued: boolean; last: ReindexOutcome | null };
  /** Базы, пропущенные пересборкой коллекции (повтор — reindex). */
  rebuild_skipped: { user_id: string; error: string; attempts: number }[];
  /** Базы пользователей, которые не открылись (повреждены или недоступны). */
  unavailable_dbs: { user_id: string; error: string }[];
}

export const indexStatus = () => call<IndexStatus>('GET', 'index/status');

/** `POST /admin/reindex`: без `full` — из кэша embeddings, с `full` — перечанковка и инференс. */
export const reindex = (full: boolean) =>
  call<{ job_count: number; unavailable_dbs: { user_id: string; error: string }[] }>(
    'POST',
    'admin/reindex',
    { full },
  );
