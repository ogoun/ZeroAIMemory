// REST страниц M7.3 (спецификация §6.1–§6.7, §6.9): «Внутренние работы» (настройки исполнителя
// `works.*`, проверка подключения, очередь и ручной запуск, журнал отправок, токены), «Проверка
// памяти» (статус, ручной прогон, «Принять как норму») и «Кандидаты в Rules» (список, карточка,
// «Принять», «Отклонить»). Модуль раздела, как `code.ts`: импортируют только ленивые страницы, в
// стартовую загрузку он не идёт (оболочке нужны лишь число кандидатов — `api.ts` — и точки шапки —
// `attention.svelte.ts`). Страницы — своего пользователя: сессия пользователя — он сам,
// администратор — его пользователь. M7.4 (§7.6 пп. 2–3, §7.7 пп. 1–2): сводка работ всех
// пользователей (`GET /admin/works`), «Внутренние работы» пользователя глазами администратора
// (`user` у функций очереди и настроек: чтение — `?user=`, запуск — `X-Zam-User`, настройки —
// `/admin/users/{id}/settings`), набор проб — список, ручная проба и снятие. M7.5 (§8.8 п. 1):
// «Предложения» — кандидаты в Rules и уборки (`type=tidy`: список, «Применить», «Отклонить»).
import { call, getEntry, getNode, listSpaces, spaceRef, type Space, type UserSettings } from './api';
import type { CheckRun, CheckStatus } from './checkVerdict';
import { JOURNAL_OF_KIND } from './journals';
import type { Route } from './router';
import type { TidyFields } from './tidyCard';
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

/** Вход подключения `works.model` (`PATCH`): запись личного Vault — `v:<id>` (адрес — её
 * `data.url`, ключ — её значение); протокол — OpenAI-совместимый или Anthropic Messages (M7.6
 * §9.2; у `anthropic` поле предела — только `auto`). */
export interface ModelConnInput {
  protocol: 'openai' | 'anthropic';
  vault: string;
  model: string;
  auth: 'key' | 'none';
  max_tokens: number;
  token_field: 'auto' | 'max_tokens' | 'max_completion_tokens';
  body_extra: Record<string, unknown>;
}

/** Подключение в ответе настроек: вход, закреплённый при сохранении адрес (`pinned`) и поля для
 * показа — `host` и `external` («вне LAN» по закреплённому адресу и классу, сохранённому при
 * закреплении, а на паузе «адрес изменён» — по «стал»; сервер считает без DNS, задача 8, ревью
 * r1). Поля показа и `pinned` в `PATCH` не отправляются (вход сервера строгий). */
export interface ModelConn extends ModelConnInput {
  pinned?: { scheme: string; host: string; port: number; path: string };
  host?: string | null;
  external?: boolean;
}

/** Вход для `PATCH` из подключения ответа — только поля входа. */
export const connInput = (m: ModelConnInput): ModelConnInput => ({
  protocol: m.protocol,
  vault: m.vault,
  model: m.model,
  auth: m.auth,
  max_tokens: m.max_tokens,
  token_field: m.token_field,
  body_extra: m.body_extra,
});

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
  model?: (ModelConnInput & { confirm_url?: string }) | null;
  check?: 'on' | 'off';
}

/** Ответ `PATCH`: все настройки и предупреждения (`works_model_external: <хост>`). */
export type SettingsSaved = UserSettings & { works: WorksSettings; warnings?: string[] };

/** Настройки пользователя: свои — `GET /auth/settings`; чужие (администратор, `user`) —
 * `GET /admin/users/{id}/settings` (та же форма ответа). */
const settingsPath = (user?: string | null) => (user ? `admin/users/${encodeURIComponent(user)}/settings` : 'auth/settings');
/** `?user=` чужого пользователя (администратор) у чтения очереди, токенов и журнала. */
const userQ = (user: string | null | undefined, sep: '?' | '&') => (user ? `${sep}user=${encodeURIComponent(user)}` : '');

/** Настройки исполнителя (`user` — чужой пользователь администратора). */
export const worksSettings = (user?: string | null) => call<UserSettings & { works: WorksSettings }>('GET', settingsPath(user));

/** Правка настроек исполнителя (`user` — чужой пользователь администратора; аудит — от имени
 * администратора). */
export const patchWorks = (works: WorksPatch, user?: string | null) => call<SettingsSaved>('PATCH', settingsPath(user), { works });

/** Проверка подключения (§6.2 п. 9): фиксированный запрос без данных памяти. На паузе
 * `vault_changed` — 409 `{code: vault_changed, was, now}` без обращения. Неудача после отправки —
 * с полным адресом запроса `url`, у `not_found` — имена моделей сервера `models`, если он их
 * отдаёт (M7.6, решение 169). */
export interface ModelTest {
  ok: boolean;
  ms: number | null;
  class?: string;
  hint?: string;
  thinking?: boolean;
  url?: string;
  models?: string[];
}
export const testModel = (user?: string | null) => call<ModelTest>('POST', `works/model/test${userQ(user, '?')}`);

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
export const listWorks = (limit = 30, user?: string | null) => call<WorksList>('GET', `works?limit=${limit}${userQ(user, '&')}`);

/** `GET /works/usage`: токены суток и месяца UTC к пределам, ночь сервера. */
export interface WorksUsage {
  day: { tokens: number; limit: number };
  month: { tokens: number; limit: number };
  ceiling: { daily: number; monthly: number };
  last_night: string | null;
  night: { at: string; tz: string; window_min: number };
  modes: Record<string, { mode: WorksMode; reason: string | null }>;
  /** Ночная проверка сервера и пользователя (как `check` у `GET /check/status`): `server = off` —
   * администратор её выключил (`reason`: `disabled` | `check_off`), выключатель пользователя не
   * действует. */
  check: { server: 'nightly' | 'off'; user: 'on' | 'off'; reason: string | null };
  paused?: WorksPause;
}
export const worksUsage = (user?: string | null) => call<WorksUsage>('GET', `works/usage${userQ(user, '?')}`);

/** Ручной запуск вида (§6.3 п. 2): агент — работы в очереди, модель — сразу (202). Чужой
 * пользователь администратора — заголовком `X-Zam-User` (у `POST /works/run` нет `?user=`). */
export interface WorksRun {
  mode: 'agent' | 'model';
  run: string | null;
  queued: string[];
}
export const runWorks = (kind: WorkKind, user?: string | null) => call<WorksRun>('POST', 'works/run', { kind }, user ?? undefined);

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
  /** Длительность, мс; `null` — ответа не было (`aborted`, `internal`, отказ до отправки:
   * `limit`, `unavailable` без соединения, …): показывается «—», а не 0. */
  ms: number | null;
  outcome: string;
  http_status: number | null;
  external: boolean;
  items: number | null;
  run: string | null;
}
export const workCalls = (days: number, user?: string | null) =>
  call<{ days: number; calls: WorkCall[] }>('GET', `works/calls?days=${days}${userQ(user, '&')}`);

// ---- сводка администратора (M7.4 §7.6 п. 2, признак `admin_works`) ----

/** Строка сводки `GET /admin/works`: режимы видов, пауза, очередь, упавшие за 7 суток, последняя
 * ночь, токены суток и месяца, последний законченный прогон проверки. База пользователя не
 * открылась — только `{user, login, error}`. */
export interface AdminWorksRow {
  user: string;
  login: string;
  error?: string;
  modes?: Record<string, { mode: WorksMode; reason: string | null }>;
  paused?: WorksPause;
  queued?: number;
  running?: number;
  failed_7d?: number;
  last_night?: string | null;
  tokens?: { day: number; month: number };
  check?: { status: string | null; verdict: string | null };
}
/** Сводка работ всех пользователей — только администратор. */
export const adminWorks = () => call<{ users: AdminWorksRow[] }>('GET', 'admin/works');

// ---- проверка памяти (§6.5) ----

export const checkStatus = () => call<CheckStatus>('GET', 'check/status');
/** Прогоны, новые первыми (без провалов проб): у каждого — `accepted_at`, эпизод, повод;
 * `trigger`/`status` — фильтр сервера. */
export const checkRuns = (limit: number, f: { trigger?: 'nightly' | 'manual'; status?: string } = {}) =>
  call<{ items: CheckRun[] }>(
    'GET',
    `check/runs?limit=${limit}${f.trigger ? `&trigger=${f.trigger}` : ''}${f.status ? `&status=${f.status}` : ''}`,
  );
/** Последний законченный ночной прогон — по нему открыта ли регрессия и что можно принять
 * (правило строки дайджеста сервера). */
export const lastNightlyRun = async (): Promise<CheckRun | null> =>
  (await checkRuns(1, { trigger: 'nightly', status: 'done' })).items[0] ?? null;
/** Ручной прогон (202 `{run}`); идёт — 409 `/problems/check-running`. */
export const runCheck = () => call<{ run: string; status: string }>('POST', 'check/run');
/** «Принять как норму» — последний законченный ночной прогон эпизода. */
export const acceptCheck = (run: string) => call<CheckRun>('POST', 'check/accept', { run });

// ---- набор проб (M7.4 §7.6 п. 3, признак `check_probes_manage`) ----

export type ProbeKind = 'probe' | 'cache' | 'negative' | 'manual';

/** Проба ночной проверки (`GET /check/probes`): цели — ссылки с ревизией при создании. */
export interface Probe {
  id: string;
  space: string | null;
  question: string;
  kind: ProbeKind;
  targets: { ref: string; rev: number }[];
  split: 'dev' | 'test';
  overlap: number | null;
  author_kind: 'agent' | 'model' | 'human' | 'system';
  author: string | null;
  work: string | null;
  status: 'active' | 'retired';
  retired_reason: string | null;
  created_at: string;
  retired_at: string | null;
}

/** Фильтры списка проб (сервер): пустое — без фильтра. */
export interface ProbeFilter {
  kind?: ProbeKind | '';
  status?: 'active' | 'retired' | '';
  split?: 'dev' | 'test' | '';
}

/** Сколько проб показывает список (сервер — до 1000, по умолчанию 200). */
export const PROBES_PAGE = 200;

export const listProbes = (f: ProbeFilter) => {
  const q = new URLSearchParams({ limit: String(PROBES_PAGE) });
  if (f.kind) q.set('kind', f.kind);
  if (f.status) q.set('status', f.status);
  if (f.split) q.set('split', f.split);
  return call<{ items: Probe[] }>('GET', `check/probes?${q}`);
};
/** Ручная проба (201 `{id: "x:<id>"}`): повтор вопроса — 409 `/problems/probe-duplicate`, предел
 * проб у цели — 409 `/problems/probes-limit`, цель не найдена или из другого пространства — 422. */
export const addProbe = (question: string, targets: string[]) => call<{ id: string }>('POST', 'check/probes', { question, targets });
/** Снять пробу (204) — только человек; причина 3..=500 символов, аудит `check.probe.retire`. */
export const retireProbe = (id: string, reason: string) => call<void>('DELETE', `check/probes/${encodeURIComponent(id)}`, { reason });

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
  /** `rule` — кандидат в Rules (M7.3); `tidy` — кандидат уборки (M7.5 §8.7, признак `tidy`). */
  type: 'rule' | 'tidy';
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
  /** В тексте есть команды shell (`$ …`, `cmd /c`, `powershell`, конвейер `curl … | sh`, `iex`,
   * `rm -rf`, `chmod +x`, `sudo`, …; эвристика сервера `works::rules::has_shell`). */
  shell: boolean;
  created_at: string;
  decided_at: string | null;
  decided_by: string | null;
  decision_note: string | null;
  /** Созданная заметка Rules (`n:<id>`). */
  result_ref: string | null;
}

/** Карточка: ещё сумма Rules в дайджесте сейчас и с кандидатом (у `rule`), источники с
 * заголовками. */
export interface ProposalCard extends Proposal {
  rules_tokens?: { now: number; after: number };
  sources: ProposalSource[];
}

/** Предложение уборки (M7.5 §8.7, `type: tidy`): тип кандидата, сигналы, записи (L1 и ревизия на
 * момент предложения), допустимые операции, выбранная операция и `stale` — запись изменилась
 * после предложения («Применить» — 409). */
export type TidyProposal = Proposal & TidyFields;
export type TidyProposalCard = ProposalCard & TidyFields;

/** Список предложений типа `type` (`rule` — по умолчанию у сервера); `status` — один статус, без
 * него — все, кроме `skipped`. */
export const listProposals = (type: Proposal['type'], status?: Proposal['status']) =>
  call<{ proposals: (Proposal & Partial<TidyFields>)[] }>(
    'GET',
    `proposals?type=${type}&limit=200${status ? `&status=${status}` : ''}`,
  );
export const getProposal = (id: string) => call<ProposalCard & Partial<TidyFields>>('GET', `proposals/${encodeURIComponent(id)}`);
/** «Применить» предложение уборки — только сессия WebUI (§8.7 п. 3): `text` — правка человеком
 * текста `merge` или `append`; ревизия записи не та — 409 `code: stale`, решено раньше — 409
 * `code: not_open`. `result` — новая запись `merge`, цель операции или созданный термин. */
export const applyTidy = (id: string, body: { text?: string }) =>
  call<{ proposal: Proposal & Partial<TidyFields>; result: string; warnings?: string[] }>(
    'POST',
    `proposals/${encodeURIComponent(id)}/accept`,
    body,
  );
/** «Принять»: путь и текст можно поправить; заметку Rules создаёт тот же код, что у WebUI. */
export const acceptProposal = (id: string, edit: { path?: string; content?: string }) =>
  call<{ proposal: Proposal; note: { id: string; path: string } }>('POST', `proposals/${encodeURIComponent(id)}/accept`, edit);
export const declineProposal = (id: string, note?: string) =>
  call<Proposal>('POST', `proposals/${encodeURIComponent(id)}/decline`, note ? { note } : {});
