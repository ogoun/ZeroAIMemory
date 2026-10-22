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
  /** Человеческий токен (§7.5): запись по нему — от человека, ставит и меняет axiom. */
  human: boolean;
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
export const issueToken = (userId: string, name: string, access: Access, human = false) =>
  call<IssuedToken>('POST', `admin/users/${userId}/tokens`, { name, access, human });
export const revokeToken = (id: string) => call<void>('DELETE', `admin/tokens/${id}`);

// ---- пространства и узлы (администратор — от имени пользователя `user`) ----

export type SpaceKind = 'rules' | 'human' | 'knowledge' | 'solution';

export interface Space {
  id: string;
  kind: SpaceKind;
  agent_solution_id: string | null;
  name: string;
  /** Роль пользователя (`GET /spaces`): `personal` — фиксированное, `owner` — своё решение,
   * `member` — чужое решение из базы владельца (§11.6). */
  role?: 'personal' | 'owner' | 'member';
  /** Своё решение с одобренными участниками. */
  shared?: boolean;
  /** Режим участника чужого решения. */
  access?: Access;
  /** Владелец чужого решения. */
  owner?: UserRef | null;
}

export interface UserRef {
  id: string;
  name: string | null;
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
  return s.agent_solution_id ?? s.id;
}

/** Пространство соответствует ссылке `ref` (форма ссылки — как у `spaceRef`, либо id). */
export function spaceMatches(s: Space, ref: string): boolean {
  const r = ref.toLowerCase();
  return spaceRef(s).toLowerCase() === r || s.id.toLowerCase() === r;
}

const q = encodeURIComponent;

export const listSpaces = (user: string) =>
  call<{
    user: { id: string; name: string };
    access: string;
    /** Запрос пишет как человек (сессия WebUI — всегда). */
    human: boolean;
    spaces: Space[];
    /** Ожидающие запросы доступа к решениям пользователя. */
    access_requests_pending: number;
  }>(
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

/** Ответ 422 «нужна причина смены certainty» (§7.5): сохранить с сообщением ревизии. */
export const needsCertaintyReason = (e: unknown): boolean =>
  e instanceof ApiError &&
  e.status === 422 &&
  e.problem?.type === '/problems/certainty' &&
  e.problem?.code === 'reason-required';

/** `PUT /nodes/{id}` с `If-Match: <rev>`: замена текста целиком; `message` — сообщение ревизии
 * (обязательно при смене `certainty:` во frontmatter). */
export const replaceNode = async (
  user: string,
  id: string,
  rev: number,
  content: string,
  message?: string,
) =>
  (await (
    await request('PUT', `nodes/${q(id)}`, {
      body: message ? { content, message } : { content },
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
  /** У заметки; у записи журнала — `entry_id`. */
  node_id?: string;
  entry_id?: string;
  /** `note` или вид записи (`insight`, `changelog`, `release`). */
  kind?: string;
  /** Статус insight. */
  status?: EntryStatus | null;
  /** `data.kind` insight. */
  insight_kind?: string | null;
  certainty?: Certainty | null;
  /** Заголовок записи. */
  title?: string;
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

// ---- журналы решения: Changelog, Releases, Insights (§7.2–7.5) ----

/** Эпистемический статус (§7.5): `axiom` ставит и меняет только человек. */
export type Certainty = 'axiom' | 'theorem' | 'hypothesis';
export type EntryKind = 'changelog' | 'release' | 'insight';
export type EntryStatus = 'active' | 'revisit' | 'superseded' | 'retracted';
export type TriggerType = 'file_glob' | 'command_regex' | 'error_regex' | 'keyword';

export interface AuthorView {
  /** `human` | `agent` | `system`. */
  kind: string;
  user_id: string | null;
  token_id: string | null;
  session_id: string | null;
  machine: string | null;
}

export interface Trigger {
  id: string;
  type: TriggerType;
  pattern: string;
  tools: string[] | null;
  created_at: string;
}

/** Новый триггер (набор в `PATCH` заменяется целиком). */
export interface NewTrigger {
  type: TriggerType;
  pattern: string;
  tools?: string[] | null;
}

/** Доказательство `theorem` (`data.evidence`). */
export interface Evidence {
  checked: string;
  method: string;
  result: string;
}

export interface Entry {
  id: string;
  space_id: string;
  kind: EntryKind;
  title: string;
  body: string;
  data: Record<string, unknown>;
  tags: string[];
  area: string | null;
  status: EntryStatus | null;
  certainty: Certainty | null;
  supersedes_id: string | null;
  superseded_by_id: string | null;
  retract_reason: string | null;
  rev: number;
  author: AuthorView;
  index_state: IndexState;
  created_at: string;
  updated_at: string;
  deleted_at: string | null;
  /** У insight в `GET`/`PATCH`. */
  triggers?: Trigger[];
}

export interface EntryQuery {
  kind?: EntryKind;
  status?: EntryStatus[];
  certainty?: Certainty[];
  area?: string;
  tag?: string;
  cursor?: string | null;
  limit?: number;
}

/** Лента записей пространства (новые — первыми), курсор — `next_cursor`. */
export function listEntries(user: string, space: string, f: EntryQuery) {
  const p = new URLSearchParams();
  if (f.kind) p.set('kind', f.kind);
  if (f.status?.length) p.set('status', f.status.join(','));
  if (f.certainty?.length) p.set('certainty', f.certainty.join(','));
  if (f.area) p.set('area', f.area);
  if (f.tag) p.set('tag', f.tag);
  if (f.cursor) p.set('cursor', f.cursor);
  if (f.limit) p.set('limit', String(f.limit));
  return call<{ items: Entry[]; next_cursor: string | null }>(
    'GET',
    `spaces/${q(space)}/entries?${p}`,
    undefined,
    user,
  );
}

export const getEntry = (user: string, id: string) =>
  call<Entry>('GET', `entries/${q(id)}`, undefined, user);

export interface EntryPatch {
  title?: string;
  body?: string;
  data?: Record<string, unknown>;
  tags?: string[];
  status?: 'active' | 'revisit';
  triggers?: NewTrigger[];
  /** `null` — снять статус. */
  certainty?: Certainty | null;
  reason?: string;
  message?: string;
}

export const patchEntry = (user: string, id: string, patch: EntryPatch) =>
  call<Entry>('PATCH', `entries/${q(id)}`, patch, user);

export interface InsightDraft {
  title: string;
  body: string;
  data: Record<string, unknown>;
  tags?: string[];
  area?: string | null;
  certainty?: Certainty;
  message?: string;
}

/** Замещение insight: старая запись — `superseded`, ничего не удаляется. */
export const supersedeEntry = (user: string, id: string, draft: InsightDraft) =>
  call<{ entry: Entry; superseded: Entry }>('POST', `entries/${q(id)}/supersede`, draft, user);

export const retractEntry = (user: string, id: string, reason: string) =>
  call<Entry>('POST', `entries/${q(id)}/retract`, { reason }, user);

export interface ReleaseCompare {
  from: Entry | null;
  to: Entry;
  /** Записи changelog интервала по `data.kind`. */
  changelog: Record<string, Entry[]>;
  truncated: boolean;
  notes_diff: string;
  commits: { sha: string; subject?: string | null }[];
}

/** Сравнение релиза с предыдущим (`withId` не задан) или с релизом `withId`. */
export const compareRelease = (user: string, id: string, withId?: string) =>
  call<ReleaseCompare>(
    'GET',
    `releases/${q(id)}/compare${withId ? `?with=${q(withId)}` : ''}`,
    undefined,
    user,
  );

// ---- граф: исходящие и обратные ссылки (§8.5) ----

export interface EdgeView {
  id: string;
  /** `n:<id>` | `e:<id>`. */
  src: string;
  src_space: string;
  /** `null` — неразрешённая ссылка (ключ в `dst_raw`). */
  dst: string | null;
  dst_space: string | null;
  dst_raw: string | null;
  dst_anchor: string | null;
  type: string;
  /** `wikilink` | `frontmatter` | `explicit` | `system`. */
  origin: string;
  created_at: string;
}

export interface GraphNode {
  ref: string;
  space_id: string;
  title: string;
  path: string | null;
  status: EntryStatus | null;
  certainty: Certainty | null;
  deleted: boolean;
}

export interface EdgeList {
  items: EdgeView[];
  nodes: GraphNode[];
}

export const nodeLinks = (user: string, id: string) =>
  call<EdgeList>('GET', `nodes/${q(id)}/links`, undefined, user);
export const nodeBacklinks = (user: string, id: string) =>
  call<EdgeList>('GET', `nodes/${q(id)}/backlinks`, undefined, user);
export const entryBacklinks = (user: string, id: string) =>
  call<EdgeList>('GET', `entries/${q(id)}/backlinks`, undefined, user);
/** Подграф на шаг вокруг записи: исходящие связи записи — рёбра с `src = e:<id>`. */
export const entryGraph = (user: string, id: string) =>
  call<{ nodes: GraphNode[]; edges: EdgeView[] }>(
    'GET',
    `graph?entry=${q(id)}&depth=1`,
    undefined,
    user,
  );

// ---- Vault (§7.6): значения не кэшируются и не логируются ----

export interface VaultItem {
  id: string;
  /** `knowledge` или GUID решения. */
  space: string;
  agent_solution_id: string | null;
  title: string;
  data: { type: string; username?: string; url?: string; description?: string };
  rev: number;
  author: AuthorView;
  created_at: string;
  updated_at: string;
}

export const listVault = (user: string, space: string) =>
  call<{ items: VaultItem[] }>('GET', `spaces/${q(space)}/vault`, undefined, user);

/** Значение секрета: ответ `no-store`, раскрытие записывается в аудит. */
export async function revealVault(user: string, id: string): Promise<string> {
  const r = await call<{ secret?: unknown }>('GET', `vault/${q(id)}?reveal=1`, undefined, user);
  if (typeof r.secret !== 'string') throw new ApiError(500, 'сервер не вернул значение');
  return r.secret;
}

// ---- решения и совместный доступ (§11.6) ----

export interface SolutionItem {
  agent_solution_id: string;
  name: string;
  space_id: string;
  owner: UserRef;
  /** Отношение пользователя запроса: `owner` | `member` | `admin`. */
  role: string;
  access: string | null;
  created_at: string;
}

export type AccessState = 'pending' | 'approved' | 'rejected' | 'revoked';

export interface AccessEntry {
  user_id: string;
  user_name: string;
  access: Access;
  state: AccessState;
  requested_at: string;
  decided_at: string | null;
  decided_by: string | null;
}

export const listSolutions = () => call<SolutionItem[]>('GET', 'solutions');
export const solutionAccess = (guid: string) =>
  call<{ agent_solution_id: string; owner: UserRef; members: AccessEntry[]; pending: number }>(
    'GET',
    `solutions/${q(guid)}/access`,
  );
export const approveAccess = (guid: string, user: string, access: Access) =>
  call<AccessEntry>('POST', `solutions/${q(guid)}/access/${q(user)}/approve`, { access });
export const rejectAccess = (guid: string, user: string, reason: string) =>
  call<AccessEntry>(
    'POST',
    `solutions/${q(guid)}/access/${q(user)}/reject`,
    reason ? { reason } : {},
  );
export const revokeAccess = (guid: string, user: string) =>
  call<AccessEntry>('POST', `solutions/${q(guid)}/access/${q(user)}/revoke`);

// ---- настройки сервера (администратор) ----

export interface Settings {
  rules: { agent_write: boolean };
}

export const getSettings = () => call<Settings>('GET', 'admin/settings');
export const setAgentWrite = (on: boolean) =>
  call<Settings>('PATCH', 'admin/settings', { rules: { agent_write: on } });

// ---- понятные сообщения об ошибках (RFC 9457) ----

const CERTAINTY_TEXT: Record<string, string> = {
  'axiom-requires-human': 'Статус axiom ставит, меняет и снимает только человек.',
  'evidence-required':
    'Для theorem нужно доказательство: что проверено, каким методом и с каким результатом.',
  'reason-required': 'Укажите причину смены статуса надёжности.',
  'invalid-transition': 'Такой переход статуса надёжности недопустим.',
};

const ACCESS_TEXT: Record<string, string> = {
  pending: 'запрос доступа к решению ждёт подтверждения владельца',
  rejected: 'запрос доступа к решению отклонён владельцем',
  revoked: 'доступ к решению отозван владельцем',
};

/** Ошибка → текст для человека: известные `type`/`code` problem+json — по-русски. */
export function errorText(e: unknown): string {
  if (!(e instanceof ApiError)) return e instanceof Error ? e.message : 'Ошибка';
  const p = e.problem;
  const type = typeof p?.type === 'string' ? p.type : '';
  const code = typeof p?.code === 'string' ? p.code : '';
  if (type === '/problems/certainty' && CERTAINTY_TEXT[code]) return CERTAINTY_TEXT[code];
  if (type === '/problems/solution-access') {
    const st = typeof p?.state === 'string' ? p.state : '';
    return `Нет доступа: ${ACCESS_TEXT[st] ?? 'доступа к решению нет'}.`;
  }
  if (type === '/problems/vault-locked') return e.message;
  if (type === '/problems/possible-duplicate') return `Похоже на уже записанный insight: ${e.message}`;
  if (e.status === 404) return 'Не найдено: запись удалена или недоступна.';
  if (e.status === 403) return `Недостаточно прав: ${e.message}`;
  return e.message;
}

/** 503 `/problems/vault-locked`. */
export const isVaultLocked = (e: unknown): boolean =>
  e instanceof ApiError && e.status === 503 && e.problem?.type === '/problems/vault-locked';
