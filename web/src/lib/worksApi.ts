// REST страниц M7.3 (спецификация §6.1–§6.7, §6.9): «Внутренние работы» (настройки исполнителя
// `works.*`, проверка подключения, очередь и ручной запуск, журнал отправок, токены), «Проверка
// памяти» (статус, ручной прогон, «Принять как норму») и «Кандидаты в Rules» (список, карточка,
// «Принять», «Отклонить»). Модуль раздела, как `code.ts`: импортируют только ленивые страницы, в
// стартовую загрузку он не идёт (оболочке нужны лишь число кандидатов — `api.ts` — и точки шапки —
// `attention.svelte.ts`). Страницы — своего пользователя: сессия пользователя — он сам,
// администратор — его пользователь (блок администратора — M7.4, §6.13).
import { call, getEntry, getNode, listSpaces, spaceRef, type Space, type UserSettings } from './api';
import type { CheckRun, CheckStatus } from './checkVerdict';
import { JOURNAL_OF_KIND } from './journals';
import type { Route } from './router';
import type { WorkKind, WorksMode } from './worksText';

// ---- ссылка на запись или заметку (`i:<id>`, `n:<id>`) → адрес карточки ----

const spacesOf = new Map<string, Promise<Space[]>>();

/**
 * Адрес записи журнала или заметки по ссылке `<буква>:<id>` (цель пробы, основание кандидата):
 * пространство — из самой записи (`space_id`), адрес — с его ссылкой (`spaceRef`). Ссылка не на
 * запись или заметку, запись удалена или недоступна — `null`.
 */
export async function refRoute(user: string, ref: string): Promise<Route | null> {
  const m = /^([a-z]):([0-9A-Za-z]{1,64})$/.exec(ref.trim());
  if (!m) return null;
  const [, letter, id] = m;
  let list = spacesOf.get(user);
  if (!list) {
    list = listSpaces(user).then((r) => r.spaces);
    spacesOf.set(user, list);
    list.catch(() => spacesOf.delete(user));
  }
  const spaces = await list;
  const sp = (sid: string) => spaces.find((s) => s.id === sid);
  if (letter === 'n') {
    const n = (await getNode(user, id)).node;
    const s = sp(n.space_id);
    return s ? { name: 'node', user, space: spaceRef(s), node: n.id, anchor: null } : null;
  }
  // запись журнала — по id (буква ссылки вида не меняет): журнал — по её виду
  const e = await getEntry(user, id);
  const s = sp(e.space_id);
  const journal = JOURNAL_OF_KIND[e.kind];
  return s && journal ? { name: 'journal', user, space: spaceRef(s), journal, entry: e.id } : null;
}

// ---- настройки исполнителя (§6.1 п. 2, §6.2 п. 1) ----

/** Подключение `works.model`: запись личного Vault — `v:<id>` (адрес — её `data.url`, ключ — её
 * значение), закреплённый при сохранении адрес — `pinned`. */
export interface ModelConn {
  protocol: 'openai';
  vault: string;
  model: string;
  auth: 'key' | 'none';
  max_tokens: number;
  token_field: 'auto' | 'max_tokens' | 'max_completion_tokens';
  body_extra: Record<string, unknown>;
  pinned?: { scheme: string; host: string; port: number; path: string };
}

/** Пауза исполнителя (`{class, since}`; у `vault_changed` — полные адреса «был → стал»). */
export interface WorksPause {
  class: string;
  since: string;
  from?: string;
  to?: string;
}

/** Поле `works` настроек пользователя. */
export interface WorksSettings {
  /** Общий режим (с умолчанием сервера). */
  mode: WorksMode;
  /** Переопределения видов. */
  kinds: Partial<Record<WorkKind, WorksMode>>;
  model: ModelConn | null;
  limits: { daily_tokens: number; monthly_tokens: number } | null;
  check: 'on' | 'off';
  pause: WorksPause | null;
  /** Действующий режим вида и причина `off`. */
  effective: Record<string, { mode: WorksMode; reason: string | null }>;
}

/** Правка `works` (`PATCH /auth/settings {works}`): `kinds.<вид> = null` снимает вид. */
export interface WorksPatch {
  mode?: WorksMode;
  kinds?: Partial<Record<WorkKind, WorksMode | null>>;
  model?: (Omit<ModelConn, 'pinned'> & { confirm_url?: string }) | null;
  check?: 'on' | 'off';
}

/** Ответ `PATCH`: все настройки и предупреждения (`works_model_external: <хост>`). */
export type SettingsSaved = UserSettings & { works: WorksSettings; warnings?: string[] };

/** Свои настройки (`GET /auth/settings`). */
export const worksSettings = () => call<UserSettings & { works: WorksSettings }>('GET', 'auth/settings');

/** Правка настроек исполнителя. */
export const patchWorks = (works: WorksPatch) => call<SettingsSaved>('PATCH', 'auth/settings', { works });

/** Проверка подключения (§6.2 п. 9): фиксированный запрос без данных памяти. На паузе
 * `vault_changed` — 409 `{code: vault_changed, was, now}` без обращения. */
export interface ModelTest {
  ok: boolean;
  ms: number | null;
  class?: string;
  hint?: string;
  thinking?: boolean;
}
export const testModel = () => call<ModelTest>('POST', 'works/model/test');

// ---- очередь, токены, журнал (§6.2 пп. 7–8, §6.3) ----

export interface WorkItem {
  n: number;
  target: string;
  rev: number;
  kind: string;
  title: string;
}

/** Работа очереди (`j:<id>`). */
export interface Work {
  id: string;
  kind: string;
  space: string;
  mode: 'agent' | 'model';
  origin: 'nightly' | 'manual';
  night: string | null;
  state: string;
  items: WorkItem[];
  claimed_session: string | null;
  claimed_until: string | null;
  attempts: number;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  result: unknown;
  error_class: string | null;
}

/** `GET /works`: очередь и последние работы, действующие режимы видов, пауза. */
export interface WorksList {
  works: Work[];
  modes: Record<string, { mode: WorksMode; reason: string | null }>;
  paused?: WorksPause;
}
export const listWorks = (limit = 30) => call<WorksList>('GET', `works?limit=${limit}`);

/** `GET /works/usage`: токены суток и месяца UTC к пределам, ночь сервера. */
export interface WorksUsage {
  day: { tokens: number; limit: number };
  month: { tokens: number; limit: number };
  ceiling: { daily: number; monthly: number };
  last_night: string | null;
  night: { at: string; tz: string; window_min: number };
  modes: Record<string, { mode: WorksMode; reason: string | null }>;
  paused?: WorksPause;
}
export const worksUsage = () => call<WorksUsage>('GET', 'works/usage');

/** Ручной запуск вида (§6.3 п. 2): агент — работы в очереди, модель — сразу (202). */
export interface WorksRun {
  mode: 'agent' | 'model';
  run: string | null;
  queued: string[];
}
export const runWorks = (kind: WorkKind) => call<WorksRun>('POST', 'works/run', { kind });

/** Строка журнала отправок — метаданные, без текста пакета и ответа. */
export interface WorkCall {
  ts: string;
  work: string | null;
  kind: string;
  host: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
  estimated: boolean;
  ms: number;
  outcome: string;
  http_status: number | null;
  external: boolean;
  items: number | null;
  run: string | null;
}
export const workCalls = (days: number) => call<{ days: number; calls: WorkCall[] }>('GET', `works/calls?days=${days}`);

// ---- проверка памяти (§6.5) ----

export const checkStatus = () => call<CheckStatus>('GET', 'check/status');
/** Прогоны, новые первыми (без провалов проб): у каждого — `accepted_at`, эпизод, повод. */
export const checkRuns = (limit: number) => call<{ items: CheckRun[] }>('GET', `check/runs?limit=${limit}`);
/** Ручной прогон (202 `{run}`); идёт — 409 `/problems/check-running`. */
export const runCheck = () => call<{ run: string; status: string }>('POST', 'check/run');
/** «Принять как норму» — последний законченный ночной прогон эпизода. */
export const acceptCheck = (run: string) => call<CheckRun>('POST', 'check/accept', { run });

// ---- кандидаты в Rules (§6.7) ----

export interface ProposalSource {
  ref: string;
  rev: number | null;
  current_rev?: number;
  kind?: string;
  title?: string;
  /** `false` — запись удалена или недоступна (как несуществующая). */
  live: boolean;
}

export interface Proposal {
  id: string;
  type: 'rule';
  status: 'proposed' | 'accepted' | 'declined' | 'withdrawn' | 'stale' | 'skipped';
  space: string;
  path: string;
  content: string;
  reason: string | null;
  basis: string[];
  targets: { target: string; rev: number }[];
  author_kind: 'agent' | 'model';
  author: string | null;
  work: string | null;
  session: string | null;
  /** В тексте есть команды shell (`$ …`, `cmd /c`, `powershell`). */
  shell: boolean;
  created_at: string;
  decided_at: string | null;
  decided_by: string | null;
  decision_note: string | null;
  /** Созданная заметка Rules (`n:<id>`). */
  result_ref: string | null;
}

/** Карточка: ещё сумма Rules в дайджесте сейчас и с кандидатом, источники с заголовками. */
export interface ProposalCard extends Proposal {
  rules_tokens: { now: number; after: number };
  sources: ProposalSource[];
}

export const listProposals = (status?: Proposal['status']) =>
  call<{ proposals: Proposal[] }>('GET', `proposals?limit=200${status ? `&status=${status}` : ''}`);
export const getProposal = (id: string) => call<ProposalCard>('GET', `proposals/${encodeURIComponent(id)}`);
/** «Принять»: путь и текст можно поправить; заметку Rules создаёт тот же код, что у WebUI. */
export const acceptProposal = (id: string, edit: { path?: string; content?: string }) =>
  call<{ proposal: Proposal; note: { id: string; path: string } }>('POST', `proposals/${encodeURIComponent(id)}/accept`, edit);
export const declineProposal = (id: string, note?: string) =>
  call<Proposal>('POST', `proposals/${encodeURIComponent(id)}/decline`, note ? { note } : {});
