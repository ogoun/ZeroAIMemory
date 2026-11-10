// Клиент REST API. Пути относительные: разрешаются от <base href>, который подставляет сервер.

export interface User {
  id: string;
  name: string;
  is_admin: boolean;
  created_at: string;
  disabled_at: string | null;
  tokens: number;
  /** Пароль входа в WebUI задан (§11.4). */
  has_password: boolean;
  /** При следующем входе пользователь обязан сменить пароль. */
  must_change_password: boolean;
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

/**
 * Стили подсветки кода сервера (кэш — сутки, `render.rs`): версия в адресе сбрасывает кэш
 * браузера при смене CSS. Поднимать вместе с `zam_markdown::highlight_css` (v2 — тёмная тема
 * только для screen, M5.5).
 */
export const HIGHLIGHT_CSS = 'api/v1/render/highlight.css?v=2';

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

/** JSON-запрос к API (`request` + разбор тела; 204 — `undefined`). Экспорт — для модулей
 * разделов в своих файлах (`code.ts`, M5.6): один источник CSRF и разбора problem+json. */
export async function call<T>(method: string, path: string, body?: unknown, user?: string): Promise<T> {
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
  /** `admin` — администратор из конфигурации, `user` — пользователь (§11.4). */
  kind: 'admin' | 'user';
  /** Чем входили: логин администратора или имя пользователя. */
  login: string;
  /** Имя пользователя базы. */
  name: string;
  user_id: string;
  /** Пароль задал администратор: до смены доступна только смена пароля. */
  must_change_password: boolean;
  csrf: string;
}

/** Сессия администратора (разделы «Пользователи», «Настройки», полный «Индекс»). */
export const isAdminSession = (m: Me): boolean => m.kind !== 'user';

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
/** Администратор задаёт пароль входа в WebUI (≥ 10 символов, смена при первом входе). */
export const setUserPassword = (userId: string, password: string) =>
  call<User>('POST', `admin/users/${userId}/password`, { password });
/** Администратор закрывает вход пользователя в WebUI. */
export const clearUserPassword = (userId: string) =>
  call<void>('DELETE', `admin/users/${userId}/password`);

// ---- свой аккаунт (сессия пользователя или администратора, §11.4) ----

/** Смена своего пароля: `old` — текущий, `new` — не короче 10 символов. */
export const changePassword = (old: string, next: string) =>
  call<void>('POST', 'auth/password', { old, new: next });
export const listOwnTokens = () => call<TokenInfo[]>('GET', 'auth/tokens');
export const issueOwnToken = (name: string, access: Access) =>
  call<IssuedToken>('POST', 'auth/tokens', { name, access });
export const revokeOwnToken = (id: string) => call<void>('DELETE', `auth/tokens/${q(id)}`);

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

// ---- вложения заметки и превью документов (M5) ----

/** Состояние извлечения текста вложения: `unsupported` — вид без извлечения (картинка, архив). */
export type ExtractState = 'pending' | 'done' | 'failed' | 'unsupported';

/** Вложение заметки (`GET /nodes/{id}/attachments`). */
export interface Attachment {
  id: string;
  sha256: string;
  filename: string;
  mime: string;
  size: number;
  extract_state: ExtractState;
  /** Код причины неудачи (`parse_timeout`, `parse_error: xml (word/document.xml)`…), не путь. */
  extract_error: string | null;
  /** Текста нет (скан): нужно распознавание. */
  ocr_needed: boolean;
  /** Ссылка для Markdown (`_attachments/<sha>/<имя>`). */
  link: string;
  created_at: string;
}

export const listAttachments = (user: string, node: string) =>
  call<Attachment[]>('GET', `nodes/${q(node)}/attachments`, undefined, user);

/** Открепление вложения от заметки (право записи). */
export const detachAttachment = (user: string, node: string, sha: string, filename: string) =>
  call<void>('DELETE', `nodes/${q(node)}/attachments/${q(sha)}/${q(filename)}`, undefined, user);

/**
 * Адрес файла вложения (для `<img>` и ссылки «Скачать»): у администратора пользователь —
 * параметром `user` (заголовок выбора пользователя у `<img>` не передать).
 */
export const blobUrl = (user: string, sha: string, filename: string) =>
  `api/v1/blobs/${q(sha)}/${q(filename)}?user=${q(user)}`;

/**
 * Адрес архива экспорта пространства (M6 §12б.5): zip с заметками, вложениями, журналами,
 * связями и историей; браузер скачивает его по ссылке (cookie сессии), у администратора
 * пользователь — параметром `user`, как у вложений.
 */
export const exportUrl = (user: string, space: string) =>
  `api/v1/spaces/${q(space)}/export?user=${q(user)}`;

/**
 * Проверка экспорта перед скачиванием (`probe=true` → 204): права и свободный слот. Ошибку
 * скачивания по ссылке браузер не показывает — её показывает дерево по ответу проверки.
 */
export const exportProbe = (user: string, space: string) =>
  call<void>('GET', `spaces/${q(space)}/export?probe=true`, undefined, user);

/** Байты файла вложения (просмотрщики PDF и DOCX разбирают его в браузере). */
export const blobBytes = async (user: string, sha: string, filename: string): Promise<ArrayBuffer> =>
  (await request('GET', `blobs/${q(sha)}/${q(filename)}`, { user })).arrayBuffer();

/** Лист книги в превью: номер в порядке книги, имя через сканер секретов, номер `sheet:#N`. */
export interface PreviewSheetRef {
  no: number;
  name: string;
  secret: boolean;
  /** Номер листа в `locator` вида `sheet:#N` (только если в книге есть лист с секретом). */
  ref_no: number | null;
}

/** Страница листа (`GET /previews/{sha}?kind=sheet`). */
export interface SheetPreview {
  kind: 'sheet';
  sheet: number;
  name: string;
  sheets: PreviewSheetRef[];
  /** Шапка — первая непустая строка листа. */
  header: string[];
  /** Строки окна; пустая строка — пустой массив. */
  rows: string[][];
  /** Номер строки листа (с 1) первой строки `rows`. */
  first_row: number;
  /** Строк данных под шапкой. */
  total_rows: number;
  cols: number;
  warnings: string[];
}

export const previewSheet = (user: string, sha: string, sheet: number, from: number, rows: number) =>
  call<SheetPreview>(
    'GET',
    `previews/${q(sha)}?kind=sheet&sheet=${sheet}&from=${from}&rows=${rows}`,
    undefined,
    user,
  );

export interface PreviewSlide {
  slide: number;
  title: string;
  text: string;
  notes: string;
}

/** Текст документа (`kind=text`): слайды PPTX или HTML подсветки (только `<span class>`). */
export type TextPreview =
  | { kind: 'text'; format: 'slides'; slides: PreviewSlide[]; warnings: string[] }
  | {
      kind: 'text';
      format: 'html';
      syntax: string | null;
      html: string;
      highlighted: boolean;
      highlight_skipped: string | null;
      truncated: boolean;
    };

export const previewText = (user: string, sha: string) =>
  call<TextPreview>('GET', `previews/${q(sha)}?kind=text`, undefined, user);

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
  /** `note`, `attachment` или вид записи (`insight`, `changelog`, `release`, `report`…). */
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
  /** Пути файлов, чьи ссылки записи устарели (M4.5 §6): «возможно устарело». */
  stale_refs?: string[];
  /** Совпадения обрамлены U+E000 … U+E001. */
  snippet: string;
  score: number;
  /** Каналы, нашедшие элемент: `vector`, `fts`, `trigram`. */
  sources: string[];
  /** Фрагмент вложения (`kind: attachment`): `a:<chunk>`, документ, файл и место в нём. */
  id?: string;
  attachment_id?: string;
  file?: string;
  blob_sha256?: string;
  /** `page:12`, `sheet:Итоги!A1:F40` (`sheet:#N!…` для листа с секретом), `slide:3`, `para:40`. */
  locator?: string | null;
  /** Подпись места: `стр. 12`, `лист «Итоги» A1:F40`, `слайд 3`, `абз. 40`. */
  locator_text?: string | null;
  /** У отчёта (`kind: report`): дата создания. */
  created_at?: string;
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

/** Гибридный поиск по пространствам пользователя (Human — только сессии WebUI, своей базы). */
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
    /** Записей точек, не подтверждённых в WAL Qdrant (`null` — external, записей не было). */
    wal_pending_ops: number | null;
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
  /** Видимые базы: ход фоновой перенарезки после смены настроек нарезки (`null` — не идёт) и
   * Rules базы (M6 §8; старый сервер — без поля). */
  dbs: { user_id: string; rechunk: Rechunk | null; rules?: RulesStatus | null }[];
  /** Rules пользователя запроса (токен, пользователь, администратор с `X-Zam-User`); у
   * администратора по всем базам — `null` (Rules каждой — в `dbs`). */
  rules?: RulesStatus | null;
}

/** Перенарезка базы (§8.2): `reason` — `params` | `algorithm` | `tokenizer` | `unknown`;
 * `changed` — ключи `[index]`, `algorithm` или `tokenizer`; `done` из `total` записей. */
export interface Rechunk {
  reason: string;
  changed: string[];
  total: number;
  done: number;
}

/** Администратору — все базы; пользователю — только своя (`dbs` из одной базы). */
export const indexStatus = () => call<IndexStatus>('GET', 'index/status');
/** Лёгкий статус для баннера: только ход перенарезки баз. */
export const rechunkStatus = () => call<{ dbs: IndexStatus['dbs'] }>('GET', 'index/status?only=rechunk');

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
/** Вид записи (спецификация M4.5 §1.3): с M4.5 — термин словаря, задача, вопрос; с M5.5 —
 * отчёт (`report`, только WebUI); с M5.7 — идея (`idea`, ссылка `w:`). */
export type EntryKind = 'changelog' | 'release' | 'report' | 'insight' | 'term' | 'task' | 'question' | 'idea';
/** Статус по виду: insight/term — `active…retracted` (у term без `revisit`), task —
 * `open…dropped`, question — `open`, `answered`, `obsolete`, idea — `new`, `proposed`,
 * `accepted`, `deferred`, `declined`, `done` (M5.7 §5). */
export type EntryStatus =
  | 'active'
  | 'revisit'
  | 'superseded'
  | 'retracted'
  | 'open'
  | 'in_progress'
  | 'blocked'
  | 'done'
  | 'dropped'
  | 'answered'
  | 'obsolete'
  | 'new'
  | 'proposed'
  | 'accepted'
  | 'deferred'
  | 'declined';
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
  /** У insight в `GET /entries/{id}` и в списке: показы подсказками и оценки (§10.5). */
  usage?: UsageCounts;
  /** У задачи `done`: минуты от `started_at` до `done_at` (`null` — старта не было). */
  duration_min?: number | null;
  /** У идеи в `GET /entries/{id}`: задача из «начать» (удалённая — `deleted`, M5.7 §7.1). */
  task?: { id: string; status?: EntryStatus; title?: string; deleted?: boolean };
  /** У идеи в `GET /entries/{id}`: последний вопрос-предложение. */
  question?: { id: string; status?: EntryStatus; deleted?: boolean };
}

export interface EntryQuery {
  kind?: EntryKind;
  status?: EntryStatus[];
  certainty?: Certainty[];
  area?: string;
  tag?: string;
  /** `data.kind`: вид changelog или insight (фильтр на сервере, курсор его учитывает). */
  dataKind?: string;
  /** Решения по `data.decision.decided_by` (M4.5 §5). */
  decidedBy?: 'human' | 'agent';
  /** Только `kind: idea`: происхождение `data.origin` (M5.7 §7.1). */
  origin?: 'human' | 'agent';
  /** Только `kind: idea`: `data.priority`. */
  priority?: 'low' | 'normal' | 'high';
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
  if (f.dataKind) p.set('data_kind', f.dataKind);
  if (f.decidedBy) p.set('decided_by', f.decidedBy);
  if (f.origin) p.set('origin', f.origin);
  if (f.priority) p.set('priority', f.priority);
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
  status?: EntryStatus;
  /** Задача: что делать дальше; пустая строка снимает. */
  next_step?: string;
  /** Задача: что мешает; пустая строка снимает. */
  blocked_by?: string;
  triggers?: NewTrigger[];
  /** `null` — снять статус. */
  certainty?: Certainty | null;
  reason?: string;
  message?: string;
}

/** `If-Match: "<rev>"` ревизии записи (§11.3). */
const ifMatch = (rev?: number): Record<string, string> =>
  rev === undefined ? {} : { 'If-Match': `"${rev}"` };

/** `PATCH /entries/{id}` с `If-Match: <rev>` (обязателен, §11.3): `rev` — ревизия из
 * `GET /entries/{id}`; запись изменилась — 409 с `rev` текущей. */
export const patchEntry = async (user: string, id: string, rev: number, patch: EntryPatch) =>
  (await (
    await request('PATCH', `entries/${q(id)}`, { body: patch, user, headers: ifMatch(rev) })
  ).json()) as Entry;

/** Новая запись журнала (`kind`: term, task, question, …); сервер добавляет `warnings`/`dedup`. */
export const createEntry = async (user: string, space: string, body: Record<string, unknown>) =>
  (await (
    await request('POST', `spaces/${q(space)}/entries`, { body, user })
  ).json()) as Entry & { warnings?: string[] };

// ---- отчёты (M5.5): только чтение, удаление и восстановление — владельцу решения ----

/** Файл отчёта (`entry_attachments`); `url` — адрес для `<img>` и «Скачать» (собирает сервер). */
export interface EntryAttachment {
  entry_id: string;
  sha256: string;
  filename: string;
  mime: string;
  size: number;
  created_at: string;
  url: string;
}

/** Элемент ленты отчётов: без `body` (тело — страница отчёта), миниатюра — первая картинка. */
export interface ReportItem extends Omit<Entry, 'body'> {
  first_image: EntryAttachment | null;
}

/** Страница отчёта `GET /entries/{id}?format=html`. */
export interface ReportHtml {
  entry: Entry;
  html: string;
  toc: OutlineItem[];
  warnings: string[];
  files: EntryAttachment[];
}

export interface ReportQuery {
  tag?: string;
  area?: string;
  cursor?: string | null;
  /** Корзина (`deleted=only`): только владельцу решения. */
  deleted?: boolean;
  limit?: number;
}

/** Лента отчётов решения (новые — первыми; в корзине — по времени удаления), курсор — `next_cursor`. */
export function listReports(user: string, space: string, f: ReportQuery) {
  const p = new URLSearchParams({ kind: 'report' });
  if (f.tag) p.set('tag', f.tag);
  if (f.area) p.set('area', f.area);
  if (f.cursor) p.set('cursor', f.cursor);
  if (f.deleted) p.set('deleted', 'only');
  if (f.limit) p.set('limit', String(f.limit));
  return call<{ items: ReportItem[]; next_cursor: string | null }>(
    'GET',
    `spaces/${q(space)}/entries?${p}`,
    undefined,
    user,
  );
}

/** Поиск отчётов решения по заголовку и тексту: `POST /search` с `kinds: [report]` (FTS). */
export const searchReports = (user: string, space: string, query: string, area?: string) =>
  call<SearchResult>(
    'POST',
    'search',
    {
      query,
      scope: { spaces: [space], ...(area ? { area } : {}) },
      filters: { kinds: ['report'] },
      mode: 'brief',
      top_k: 50,
      budget_tokens: 8000,
      format: 'json',
    },
    user,
  );

export const getReportHtml = (user: string, id: string) =>
  call<ReportHtml>('GET', `entries/${q(id)}?format=html`, undefined, user);

/** `DELETE /entries/{id}` с `If-Match` ревизии (в корзину; отчёт — только владелец решения). */
export const deleteEntry = async (user: string, id: string, rev: number): Promise<void> => {
  await request('DELETE', `entries/${q(id)}`, { user, headers: ifMatch(rev) });
};

/** Восстановление записи из корзины. */
export const restoreEntry = (user: string, id: string) =>
  call<Entry>('POST', `entries/${q(id)}/restore`, undefined, user);

// ---- словарь, задачи, вопросы (M4.5 §2–4) ----

export interface GlossaryItem {
  id: string;
  title: string;
  aliases: string[];
  definition: string;
}

/** Действующие термины пространства (по имени). */
export const getGlossary = (user: string, space: string) =>
  call<{ version: number; items: GlossaryItem[] }>('GET', `spaces/${q(space)}/glossary`, undefined, user);

/** Занятые имена из 409 `/problems/term-conflict`. */
export function termConflicts(e: unknown): { id: string; title: string; matched: string }[] {
  if (!(e instanceof ApiError) || e.status !== 409 || e.problem?.type !== '/problems/term-conflict') return [];
  const c = e.problem.conflicts;
  return Array.isArray(c) ? (c as { id: string; title: string; matched: string }[]) : [];
}

/** Снять захват задачи (человек в WebUI снимает и чужой, M4.5 §3). */
export const releaseTask = (user: string, id: string) =>
  call<Entry>('POST', `entries/${q(id)}/release`, undefined, user);

/** Ответ человека на вопрос; из WebUI — всегда `channel: webui`. Создаёт решение (`decision_id`).
 * У вопроса-предложения идеи (`data.idea`) `option` обязателен, `horizon` — только у `defer`; в
 * ответе — ещё идея и задача «начать» (M5.7 §5.3). */
export const answerQuestion = (
  user: string,
  id: string,
  a: { text?: string; option?: string; horizon?: string },
) =>
  call<{ entry: Entry; decision: Entry; decision_id: string; idea?: Entry; task?: Entry }>(
    'POST',
    `entries/${q(id)}/answer`,
    { ...a, channel: 'webui' },
    user,
  );

/** 409 «вопрос уже отвечен» с `decision_id` первого ответа. */
export const answeredDecision = (e: unknown): string | null =>
  e instanceof ApiError && e.status === 409 && e.problem?.type === '/problems/already-answered'
    ? typeof e.problem.decision_id === 'string'
      ? e.problem.decision_id
      : ''
    : null;

// ---- идеи (M5.7 §5): решение человека и «принять как свою»; propose — действие агента ----

export type IdeaDecision = 'accept' | 'defer' | 'decline' | 'reopen';

/** `POST /entries/{id}/decide` из WebUI (`by: human`, `channel: webui` ставит сервер). `horizon` —
 * только у `defer` (дата UTC позже сегодняшней), `reason` — у `decline`, обязателен у `reopen`,
 * `space` — пространство задачи `accept` (идея Knowledge). Открытый вопрос-предложение сервер
 * отвечает в той же транзакции. */
export const decideIdea = (
  user: string,
  id: string,
  decision: IdeaDecision,
  opts: { horizon?: string; reason?: string; space?: string } = {},
) =>
  call<{ entry: Entry; task?: Entry; question?: Entry }>(
    'POST',
    `entries/${q(id)}/decide`,
    { decision, ...opts },
    user,
  );

/** «Принять как свою»: идея «от агента» → `origin: human` (только человек в WebUI). */
export const adoptIdea = (user: string, id: string) =>
  call<Entry>('POST', `entries/${q(id)}/adopt`, undefined, user);

/** Поиск идей пространства по заголовку и тексту: `POST /search` `kinds: [idea]`. */
export const searchIdeas = (user: string, space: string, query: string, status: EntryStatus[]) =>
  call<SearchResult>(
    'POST',
    'search',
    {
      query,
      scope: { spaces: [space] },
      filters: { kinds: ['idea'], ...(status.length ? { status } : {}) },
      mode: 'brief',
      top_k: 50,
      budget_tokens: 8000,
      format: 'json',
    },
    user,
  );

// ---- ссылки знаний на файлы (M4.5 §6) ----

export interface FileRefPath {
  path: string;
  items: { id: string; kind: string; title: string; state: string }[];
}

/** Индекс ссылок на файлы решения `solution` (GUID): до 5 записей на путь. */
export const fileRefIndex = (user: string, solution: string) =>
  call<{ refs: FileRefPath[] }>('GET', `file-refs?solution=${q(solution)}`, undefined, user);

export interface InsightDraft {
  title: string;
  body: string;
  data: Record<string, unknown>;
  tags?: string[];
  area?: string | null;
  certainty?: Certainty;
  message?: string;
}

/** Замещение insight: старая запись — `superseded`, ничего не удаляется; `rev` старой
 * (`If-Match`) необязателен, при наличии сервер его проверяет. */
export const supersedeEntry = async (user: string, id: string, draft: InsightDraft, rev?: number) =>
  (await (
    await request('POST', `entries/${q(id)}/supersede`, { body: draft, user, headers: ifMatch(rev) })
  ).json()) as { entry: Entry; superseded: Entry };

export const retractEntry = async (user: string, id: string, reason: string, rev?: number) =>
  (await (
    await request('POST', `entries/${q(id)}/retract`, { body: { reason }, user, headers: ifMatch(rev) })
  ).json()) as Entry;

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
export const entryLinks = (user: string, id: string) =>
  call<EdgeList>('GET', `entries/${q(id)}/links`, undefined, user);
export const entryBacklinks = (user: string, id: string) =>
  call<EdgeList>('GET', `entries/${q(id)}/backlinks`, undefined, user);

// ---- Vault (§7.6): значения не кэшируются и не логируются ----

export interface VaultItem {
  id: string;
  /** `knowledge` или GUID решения. */
  space: string;
  agent_solution_id: string | null;
  title: string;
  data: {
    type: string;
    username?: string;
    url?: string;
    description?: string;
    /** Окружение (`host`, `environment`, M4.5 §8): открытые поля. */
    purpose?: string;
    address?: string;
    limits?: string;
    notes?: string;
  };
  rev: number;
  author: AuthorView;
  created_at: string;
  updated_at: string;
}

/** Список метаданных; при заблокированном Vault — `locked: true` и `reason` (значений нет). */
export interface VaultList {
  items: VaultItem[];
  locked?: boolean;
  reason?: string;
}

export const listVault = (user: string, space: string) =>
  call<VaultList>('GET', `spaces/${q(space)}/vault`, undefined, user);

/** Значение секрета: ответ `no-store`, раскрытие записывается в аудит. */
export async function revealVault(user: string, id: string): Promise<string | null> {
  const r = await call<{ secret?: unknown }>('GET', `vault/${q(id)}?reveal=1`, undefined, user);
  // у записей окружения значение необязательно: `secret: null` — значения нет
  if (r.secret === null) return null;
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

/** Агент блока инструкции (§15.5): Claude Code, AGENTS.md (Codex, Cursor и др.), Codex с хуками. */
export type AgentKind = 'claude' | 'agents' | 'codex';
export type Lang = 'ru' | 'en';
export const AGENT_KINDS: AgentKind[] = ['claude', 'agents', 'codex'];
export const LANGS: Lang[] = ['ru', 'en'];

export interface Settings {
  rules: { agent_write: boolean };
  /** Шаблоны блока инструкции: ключ `<agent>.<lang>`, `custom` — свой (не по умолчанию). */
  agent_setup: { version: number; templates: Record<string, { text: string; custom: boolean }> };
}

export const getSettings = () => call<Settings>('GET', 'admin/settings');
export const setAgentWrite = (on: boolean) =>
  call<Settings>('PATCH', 'admin/settings', { rules: { agent_write: on } });
/** Сохранить шаблон (`text`) или сбросить к умолчанию (`null`); версия растёт, если текст изменился. */
export const saveTemplate = (key: string, text: string | null) =>
  call<Settings>('PATCH', 'admin/settings', { agent_setup: { templates: { [key]: text } } });

export interface TemplatePreview {
  block: string;
  tokens: number;
  warnings: string[];
  version: number;
  /** Версия после сохранения (равна `version`, если текст не меняется). */
  version_next: number;
}

/** Предпросмотр несохранённого шаблона (с примером решения). */
export const previewTemplate = (agent: AgentKind, lang: Lang, template: string) =>
  call<TemplatePreview>('POST', 'admin/agent-setup/preview', { agent, lang, template });

// ---- подключение агента (§15.5) ----

export interface AgentSetupQuery {
  agent: AgentKind;
  /** `auto` — агент создаст сам (`zam init`), `new` — новый GUID, иначе GUID решения. */
  solution: string;
  name?: string;
  mcp: boolean;
  hooks: boolean;
  /** `undefined` — по настройке сервера `rules.agent_write`. */
  rulesWrite?: boolean;
  lang: Lang;
  /** `--token '<токен>'` в `init_cmd`: заглушку подставляет браузер (`withToken`). Открытый
   * текст токена WebUI серверу в адресе не передаёт (журналы прокси). */
  tokenPlaceholder?: boolean;
}

export interface AgentSetup {
  version: number;
  agent: AgentKind;
  lang: Lang;
  url: string;
  block: string;
  tokens: number;
  tokens_exact: boolean;
  warnings: string[];
  template_custom: boolean;
  solution?: { agent_solution_id: string; name: string };
  solution_toml?: string;
  mcp_json?: unknown;
  claude_settings?: unknown;
  codex_hooks?: unknown;
  install: { windows: string; linux: string };
  init_cmd: { linux: string; windows: string };
}

/** `GET /agent-setup`; администратор — для пользователя `user` (`X-Zam-User`). */
export function agentSetup(f: AgentSetupQuery, user?: string) {
  const p = new URLSearchParams({
    agent: f.agent,
    solution: f.solution,
    mcp: String(f.mcp),
    hooks: String(f.hooks),
    lang: f.lang,
  });
  if (f.name) p.set('name', f.name);
  if (f.rulesWrite !== undefined) p.set('rules_write', String(f.rulesWrite));
  if (f.tokenPlaceholder) p.set('token_placeholder', 'true');
  return call<AgentSetup>('GET', `agent-setup?${p}`, undefined, user);
}

/** Заглушка токена сервера в кавычках оболочки (`'<токен>'`, POSIX shell и PowerShell). */
const TOKEN_SLOT = "'<токен>'";

/** Ответ `/agent-setup` с открытым токеном вместо заглушки — только в командах установки и
 * `zam init` (в блоке и `.mcp.json` — stdio `zam mcp` — токена нет). Токен — `zam_` и base62:
 * кавычки не нужны. */
export function withToken(s: AgentSetup, token: string): AgentSetup {
  if (!/^zam_[0-9A-Za-z]+$/.test(token)) return s;
  const put = (t: string) => t.split(TOKEN_SLOT).join(`'${token}'`);
  return {
    ...s,
    install: { windows: put(s.install.windows), linux: s.install.linux },
    init_cmd: { linux: put(s.init_cmd.linux), windows: put(s.init_cmd.windows) },
  };
}

// ---- настройки пользователя: типы подсказок хуков (§10.3, R5) ----

export type HookKind =
  | 'session_start'
  | 'prompt'
  | 'pre_tool'
  | 'post_tool'
  | 'checkpoint'
  | 'subagent_start'
  | 'file_context'
  | 'session_journal'
  | 'recovery'
  | 'code_map';
export const HOOK_KINDS: HookKind[] = [
  'session_start',
  'prompt',
  'pre_tool',
  'post_tool',
  'checkpoint',
  'subagent_start',
  'file_context',
  'session_journal',
  'recovery',
  'code_map',
];
export type HookFlags = Record<HookKind, boolean>;

/** Настройки пользователя: типы подсказок хуков и проактивность идей (`ideas.proactive`, M5.7 §8). */
export interface UserSettings {
  hooks: HookFlags;
  ideas: { proactive: boolean };
}

/** Свои настройки (`own`) или пользователя `userId` (администратор). */
const settingsPath = (userId: string | null) =>
  userId ? `admin/users/${q(userId)}/settings` : 'auth/settings';
export const getUserSettings = (userId: string | null) =>
  call<UserSettings>('GET', settingsPath(userId));
export const setUserHooks = (userId: string | null, hooks: Partial<HookFlags>) =>
  call<UserSettings>('PATCH', settingsPath(userId), { hooks });
export const setUserIdeas = (userId: string | null, proactive: boolean) =>
  call<UserSettings>('PATCH', settingsPath(userId), { ideas: { proactive } });

// ---- кэш выжимок (§9.4, §15.4) ----

export interface CacheSource {
  /** `n:<id>` | `e:<id>`. */
  id: string;
  rev: number;
  section: string | null;
}

export interface CacheStale {
  target: string;
  section?: string;
  /** `changed` | `deleted` | `superseded` | `retracted` | `new_sources`. */
  reason: string;
}

export interface CacheItem {
  id: string;
  ref: string;
  query: string;
  summary: string;
  tokens: number;
  state: 'valid' | 'stale';
  stale: CacheStale[];
  sources: CacheSource[];
  author_user_id: string;
  author: string | null;
  session_id: string | null;
  machine: string | null;
  area: string | null;
  created_at: string;
  last_used_at: string;
  hits: number;
  promoted_to: string | null;
  space_id: string;
  space?: string;
  agent_solution_id?: string;
  /** Ответ `promote`: созданная заметка или запись. */
  promoted?: unknown;
}

export interface CacheQuery {
  q?: string;
  solution?: string;
  state?: 'valid' | 'stale';
  author?: string;
  limit?: number;
}

/** Выжимки личной базы или решения `solution`; администратор — пользователя `user`. */
export function listCache(f: CacheQuery, user?: string) {
  const p = new URLSearchParams();
  if (f.q) p.set('q', f.q);
  if (f.solution) p.set('solution', f.solution);
  if (f.state) p.set('state', f.state);
  if (f.author) p.set('author', f.author);
  if (f.limit) p.set('limit', String(f.limit));
  return call<{ items: CacheItem[] }>('GET', `cache?${p}`, undefined, user);
}
export const getCache = (id: string, user?: string) =>
  call<CacheItem>('GET', `cache/${q(id)}`, undefined, user);
export const deleteCache = (id: string, user?: string) =>
  call<void>('DELETE', `cache/${q(id)}`, undefined, user);
/** «Превратить в заметку» (`path` — по умолчанию `Выжимки/<запрос>`) или в insight. */
export const promoteCache = (
  id: string,
  to: { as: 'note'; path?: string } | { as: 'insight'; kind?: string },
  user?: string,
) => call<CacheItem>('POST', `cache/${q(id)}/promote`, to, user);

// ---- сессии агентов и выданное им (M4.5 §9.5, M6 §8) ----

/** Выданное сессии по событию: `reread_calls` — вызовов с пометкой «уже выдавалось» (M6 §7.3;
 * у `read` — REST и CLI, у `mcp_read` — чтения из MCP); нет поля — 0. */
export interface TokenStat {
  calls: number;
  tokens: number;
  reread_calls?: number;
}

/** Сессия агента (`GET /sessions`, `GET /sessions/{id}`). `tokens` — по событию
 * (`session_start`, `recovery`, `rules` — резерв Rules этих событий, `prompt`, `post_tool`,
 * `brief`, `pre_tool`, `post_tool_local`, `subagent_local`, `search`, `read`, `mcp`, `mcp_read` —
 * чтения из MCP с токенами 0: их токены — в `mcp`). */
export interface SessionItem {
  id: string;
  agent: string | null;
  solution: string | null;
  area: string | null;
  machine: string | null;
  parent_id: string | null;
  agent_type: string | null;
  started_at: string;
  last_seen_at: string;
  ended_at: string | null;
  memory_writes: number;
  seen_epoch: number;
  /** Контрольная группа агентного A/B (M6 §12). */
  holdout?: boolean;
  tokens?: Record<string, TokenStat>;
  /** Момент снимка сессии (M6 §8а), если он есть: сам снимок — [`sessionSnapshot`]. */
  snapshot_at?: string;
}

/** Снимок сессии (M6 §8а, `GET /sessions/{id}/snapshot`): извлечён из журнала агента клиентом
 * `zam` после маскировки секретов. Текст полей — как прислал агент: выводится только как текст. */
export interface SessionSnapshot {
  session_id: string;
  taken_at: string;
  trigger: string;
  agent: string | null;
  status: 'complete' | 'partial';
  skipped: number;
  bytes: number;
  data: {
    compact_summary?: string;
    prompts: string[];
    files: string[];
    commands: { cmd: string; failed?: boolean }[];
  };
}

/** Снимок сессии `id`; администратор — пользователя `user`. */
export const sessionSnapshot = (id: string, user?: string) =>
  call<SessionSnapshot>('GET', `sessions/${encodeURIComponent(id)}/snapshot`, undefined, user);

/** Свои сессии, свежие первыми, с выданным по событию; администратор — пользователя `user`. */
export function listSessions(f: { solution?: string; limit?: number }, user?: string) {
  const p = new URLSearchParams({ tokens: 'true' });
  if (f.solution) p.set('solution', f.solution);
  if (f.limit) p.set('limit', String(f.limit));
  return call<{ items: SessionItem[] }>('GET', `sessions?${p}`, undefined, user);
}

/** Rules пользователя (`GET /index/status?only=rules`, M6 §8): `warning` — `rules_heavy` при
 * больше 10 правил или L1 в сумме больше 1500 токенов. */
export interface RulesStatus {
  solution: string | null;
  count: number;
  l1_tokens: number;
  warning: 'rules_heavy' | null;
}

export const rulesStatus = (user?: string) =>
  call<{ rules: RulesStatus | null }>('GET', 'index/status?only=rules', undefined, user);

/** Текст плашки `rules_heavy`. */
export const rulesHeavyText = (r: RulesStatus): string =>
  `Правил много: ${r.count}, L1 в сумме ≈ ${r.l1_tokens} токенов — дайджест каждой сессии ` +
  'выдаёт их сверх бюджета. Порог — 10 правил или 1500 токенов: объедините или сократите правила.';

// ---- оценки и показы записей (§10.5) ----

export interface UsageCounts {
  shown: number;
  useful: number;
  useless: number;
}

/** Оценка полезности записи; из WebUI сервер пишет источник `webui`. */
export const entryFeedback = (user: string, id: string, value: 'useful' | 'useless') =>
  call<void>('POST', `entries/${q(id)}/feedback`, { value }, user);

// ---- понятные сообщения об ошибках (RFC 9457) ----

const CERTAINTY_TEXT: Record<string, string> = {
  'evidence-required':
    'Для theorem нужно доказательство: что проверено, каким методом и с каким результатом.',
  'reason-required': 'Укажите причину смены статуса надёжности.',
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
  if (type === '/problems/term-conflict') return e.message;
  if (type === '/problems/task-claimed') return 'Задача захвачена другой сессией.';
  if (type === '/problems/already-answered') return 'Вопрос уже отвечен.';
  if (type === '/problems/idempotency-mismatch')
    return `Повтор запроса с тем же ключом, но другим содержимым: ${e.message}`;
  if (type === '/problems/possible-duplicate') return `Похоже на уже записанный insight: ${e.message}`;
  if (e.status === 409 && typeof p?.rev === 'number')
    return `Запись изменилась (сейчас ревизия ${p.rev}): перечитайте и повторите.`;
  if (e.status === 404) return 'Не найдено: запись удалена или недоступна.';
  if (e.status === 403) return `Недостаточно прав: ${e.message}`;
  return e.message;
}

/** 409 устаревшей ревизии записи журнала (§11.3): в problem — `rev` текущей. */
export const isStaleEntry = (e: unknown): boolean =>
  e instanceof ApiError && e.status === 409 && typeof e.problem?.rev === 'number';

/** 503 `/problems/vault-locked`. */
export const isVaultLocked = (e: unknown): boolean =>
  e instanceof ApiError && e.status === 503 && e.problem?.type === '/problems/vault-locked';
