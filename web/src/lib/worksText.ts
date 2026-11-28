// Внутренние работы и кандидаты в Rules в WebUI (M7.3, спецификация §6.1–§6.3, §6.7, §6.9 пп. 1,
// 3): тексты режимов, причин и паузы, подсказка о классе адреса записи Vault до сохранения (LAN, вне
// LAN, петля — по диапазонам сервера §6.2 п. 6, вложенный IPv4 — как у сервера; свой адрес машины
// сервера и адреса имени клиент не знает — решает сервер при сохранении), `body_extra` с рецептами,
// время ночи с поясом сервера и местным временем, журнал отправок, итог работы, кандидаты. Класс
// **сохранённого** подключения (плашка «вне LAN») — поле `external` ответа настроек, не отсюда.
// Тексты — для человека: без ключей конфигурации и номеров этапов.
// Модуль без DOM и без runtime-импортов, кроме `.ts`: его читает `node --test`
// (`web/test/worksText.test.ts`).
import { plural } from './plural.ts';
import { localTime } from './time.ts';

/** Режим исполнителя: выкл, агент в своей сессии, подключённая модель. */
export type WorksMode = 'off' | 'agent' | 'model';
/** Вид внутренней работы M7.3; `tidy` — уборка памяти (M7.5, спецификация §8.7–§8.8). */
export type WorkKind = 'probes' | 'rules' | 'tidy';

export const WORK_KINDS: WorkKind[] = ['probes', 'rules', 'tidy'];
export const WORKS_MODES: WorksMode[] = ['off', 'agent', 'model'];

export const MODE_TITLE: Record<WorksMode, string> = { off: 'выкл', agent: 'агент', model: 'модель' };

/** Пояснение режима (§6.9 п. 1: «с пояснением каждого»). */
export const MODE_NOTE: Record<WorksMode, string> = {
  off: 'Работы не проводятся: ни агент, ни модель данных памяти не получают.',
  agent:
    'Работу делает агент в своей сессии: строка дайджеста при старте предложит memory_recall event=work, когда он закончит текущий шаг; токены — агента.',
  model:
    'Работу ночью делает подключённая модель: сервер отправляет ей маскированный пакет (без Human, Vault и записей участников) и принимает итог той же проверкой, что у агента.',
};

export const KIND_TITLE: Record<WorkKind, string> = { probes: 'Пробы ночной проверки', rules: 'Кандидаты в Rules', tidy: 'Уборка памяти' };
/** Вид в строках журнала и очереди (коротко). */
export const KIND_SHORT: Record<string, string> = { probes: 'пробы', rules: 'кандидаты в Rules', tidy: 'уборка', test: 'проверка подключения' };

/** Причина `off` действующего режима вида. */
export function reasonText(reason: string | null | undefined): string {
  switch (reason) {
    case 'disabled':
      return 'внутренние работы выключены на сервере (настройка администратора)';
    case 'model_access_off':
      return 'обращения к модели запрещены на сервере (настройка администратора)';
    case 'no_model':
      return 'нет подключения модели';
    case 'paused':
      return 'исполнитель на паузе';
    case 'blocked':
      return 'администратор запретил отправку на адрес подключения';
    default:
      return reason ?? '';
  }
}

/** Почему администратор запретил адрес сохранённого подключения (`works.model.blocked`, M7.6
 * §9.9, решение 173). */
export function blockedText(reason: string): string {
  switch (reason) {
    case 'model_access_off':
      return 'Отправка данных памяти модели на сервере выключена.';
    case 'model_access_lan':
      return 'Разрешена только локальная сеть, а адрес подключения — вне её.';
    case 'loopback_requires_admin':
      return 'Адреса машины сервера (localhost, 127.0.0.1 и соседние служебные) не разрешены.';
    case 'http_external':
      return 'Вне локальной сети нужен адрес https://.';
    default:
      return reason;
  }
}

/** Ночная проверка выключена на сервере (`check.server = off` у `GET /works/usage`): почему
 * выключатель «Ночная проверка моей памяти» не действует (финальное ревью M7.3, Minor 12). */
export function checkOffText(reason: string | null | undefined): string {
  if (reason === 'disabled') {
    return 'На сервере внутренние работы выключены администратором — ночной проверки нет, выключатель не действует.';
  }
  return 'На сервере ночная проверка выключена администратором — выключатель не действует; ручной прогон — на странице «Проверка памяти».';
}

/** Пауза исполнителя (§6.2 п. 5): что случилось. */
export const PAUSE_TITLE: Record<string, string> = {
  auth: 'модель не приняла ключ (HTTP 401/403)',
  not_found: 'модель или адрес не найдены (HTTP 404)',
  blocked: 'администратор запретил отправку на этот адрес',
  vault: 'запись Vault подключения недоступна: удалена, перенесена, Vault заблокирован или в записи нет ключа',
  vault_changed: 'адрес подключения изменён',
};

/** Пауза: что делать. */
export function pauseHint(cls: string): string {
  switch (cls) {
    case 'vault_changed':
      return 'Адрес записи Vault поменяли после сохранения подключения. Если меняли вы — подтвердите новый адрес; если нет — верните прежний адрес записи и сохраните подключение.';
    case 'auth':
      return 'Проверьте значение (ключ) записи Vault подключения, затем сохраните подключение или проверьте его — пауза снимется.';
    case 'not_found':
      return 'Проверьте имя модели и адрес записи Vault (с /v1, как у поставщика), затем сохраните подключение или проверьте его.';
    case 'blocked':
      return 'Адрес запрещён настройками сервера: куда можно отправлять данные и адреса машины сервера решает администратор. Когда он разрешит адрес, нажмите «Проверить подключение» — пауза снимется; или подключите другой адрес.';
    case 'vault':
      return 'Выберите запись личного Vault заново и сохраните подключение.';
    default:
      return 'Сохраните подключение или проверьте его — пауза снимется.';
  }
}

// ---- адрес записи Vault (§6.2 п. 6) ----

/** Класс адреса: петля, link-local, LAN, вне LAN; имя — узнаёт сервер при сохранении. */
export type Reach = 'loopback' | 'link_local' | 'lan' | 'external' | 'name';

/** Адрес записи Vault подключения: `data.url`, иначе `data.address` (как у сервера). */
export function vaultAddress(data: { url?: string; address?: string } | null | undefined): string {
  const u = (data?.url ?? '').trim();
  return u || (data?.address ?? '').trim();
}

function v4(s: string): number[] | null {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(s);
  if (!m) return null;
  const b = m.slice(1).map(Number);
  return b.every((x) => x <= 255) ? b : null;
}

/** IPv4, вложенный в адрес IPv6 (как `embedded_v4` сервера): IPv4-совместимый `::a.b.c.d` (кроме
 * `::` и `::1`), NAT64 `64:ff9b::/96`, 6to4 `2002::/16`. */
function embedded4(s: number[]): number[] | null {
  const octets = (a: number, b: number) => [a >> 8, a & 0xff, b >> 8, b & 0xff];
  if (s.slice(0, 6).every((x) => x === 0) && !(s[6] === 0 && (s[7] === 0 || s[7] === 1))) return octets(s[6], s[7]);
  if (s[0] === 0x64 && s[1] === 0xff9b && s.slice(2, 6).every((x) => x === 0)) return octets(s[6], s[7]);
  if (s[0] === 0x2002) return octets(s[1], s[2]);
  return null;
}

function reach4([a, b]: number[]): Reach {
  if (a === 127 || a === 0) return 'loopback';
  if (a === 169 && b === 254) return 'link_local';
  if (a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127)) return 'lan';
  return 'external';
}

/** IPv6 → восемь 16-битных групп (`::` и хвост IPv4 разрешены); не адрес — `null`. */
function v6(s: string): number[] | null {
  let t = s;
  // хвост IPv4 (`::ffff:127.0.0.1`) — две шестнадцатеричные группы
  const last = t.lastIndexOf(':');
  if (t.slice(last + 1).includes('.')) {
    const four = v4(t.slice(last + 1));
    if (!four) return null;
    t = `${t.slice(0, last + 1)}${((four[0] << 8) | four[1]).toString(16)}:${((four[2] << 8) | four[3]).toString(16)}`;
  }
  const halves = t.split('::');
  if (halves.length > 2) return null;
  const part = (x: string) => (x === '' ? [] : x.split(':'));
  const head = part(halves[0]);
  const rest = halves.length === 2 ? part(halves[1]) : [];
  const all = [...head, ...rest];
  if (!all.every((g) => /^[0-9a-f]{1,4}$/i.test(g))) return null;
  if (halves.length === 1 ? all.length !== 8 : all.length > 7) return null;
  const hex = (g: string) => parseInt(g, 16);
  const fill: number[] = halves.length === 2 ? Array(8 - all.length).fill(0) : [];
  return [...head.map(hex), ...fill, ...rest.map(hex)];
}

/** Класс хоста адреса (литерал IP — по диапазонам §6.2 п. 6; `localhost` — петля; имя — `name`). */
export function reachOfHost(host: string): Reach {
  const h = host.toLowerCase().replace(/^\[|\]$/g, '').replace(/\.$/, '');
  if (h === 'localhost' || h.endsWith('.localhost')) return 'loopback';
  const four = v4(h);
  if (four) return reach4(four);
  const six = h.includes(':') ? v6(h) : null;
  if (!six) return 'name';
  if (six.slice(0, 7).every((x) => x === 0) && (six[7] === 1 || six[7] === 0)) return 'loopback';
  // IPv4 в IPv6 (`::ffff:0:0/96`) — как IPv4
  if (six.slice(0, 5).every((x) => x === 0) && six[5] === 0xffff) {
    return reach4([six[6] >> 8, six[6] & 0xff, six[7] >> 8, six[7] & 0xff]);
  }
  // вложенный IPv4 только ужесточает класс (как у сервера): петля и link-local внутри — их класс,
  // частный адрес внутри LAN не делает (такие адреса маршрутизируются наружу)
  const inner = embedded4(six);
  if (inner) {
    const r = reach4(inner);
    if (r === 'loopback' || r === 'link_local') return r;
  }
  if ((six[0] & 0xffc0) === 0xfe80) return 'link_local';
  if ((six[0] & 0xfe00) === 0xfc00) return 'lan';
  return 'external';
}

/** Разбор адреса записи для показа до сохранения: хост (как в предупреждении сервера —
 * `host[:порт]`), класс и замечание, если сервер адрес не примет. */
export interface AddressView {
  host: string;
  reach: Reach;
  /** Сервер не примет (схема, «?», «#», «user@»). */
  problem: string;
}

export function addressView(addr: string): AddressView | null {
  const s = addr.trim();
  if (!s) return null;
  let u: URL;
  try {
    u = new URL(s);
  } catch {
    return { host: '', reach: 'name', problem: 'не адрес: нужен http:// или https:// и хост (с /v1, как у поставщика)' };
  }
  let problem = '';
  if (u.protocol !== 'http:' && u.protocol !== 'https:') problem = 'схема — только http или https';
  else if (s.includes('@')) problem = 'в адресе не должно быть «user@»: ключ — значение записи Vault';
  else if (s.includes('?') || s.includes('#')) problem = 'в адресе не должно быть «?…» и «#…»';
  const reach = reachOfHost(u.hostname);
  if (!problem && u.protocol === 'http:' && reach === 'external') problem = 'http:// — только в LAN: ключ вне сети открытым текстом не уходит';
  return { host: u.host, reach, problem };
}

/** Класс адреса — подписью у выбора записи (подсказка до сохранения: окончательно решает сервер). */
export function reachText(r: Reach): string {
  switch (r) {
    case 'lan':
      return 'в локальной сети (если это не адрес самой машины сервера — сервер уточнит при сохранении)';
    case 'external':
      return 'вне локальной сети: данные памяти уйдут за её пределы';
    case 'loopback':
      return 'эта же машина: подойдёт, только если такие адреса разрешил администратор сервера';
    case 'link_local':
      return 'служебный локальный адрес: подойдёт, только если такие адреса разрешил администратор сервера';
    default:
      return 'имя: в локальной сети оно или нет, сервер проверит при сохранении';
  }
}

/** Закреплённый адрес подключения (`works.model.pinned`). */
export interface Pinned {
  scheme: string;
  host: string;
  port: number;
  path: string;
}

/** Хост закреплённого адреса, как в предупреждении и аудите сервера: `host` или `host:порт`
 * (порт схемы по умолчанию опущен, IPv6 — в скобках). */
export function pinnedHost(p: Pinned): string {
  const h = p.host.includes(':') && !p.host.startsWith('[') ? `[${p.host}]` : p.host;
  const def = p.scheme === 'https' ? 443 : 80;
  return p.port === def ? h : `${h}:${p.port}`;
}

/** Полный закреплённый адрес: «https://api.example.com/v1» (как «был → стал» паузы). */
export const pinnedUrl = (p: Pinned): string => `${p.scheme}://${pinnedHost(p)}${p.path}`;

// ---- протокол подключения (M7.6 §9.2) ----

/** Протокол `works.model.protocol`. */
export type Protocol = 'openai' | 'anthropic';
export const PROTOCOLS: Protocol[] = ['openai', 'anthropic'];
export const PROTOCOL_TITLE: Record<Protocol, string> = {
  openai: 'OpenAI-совместимый (Chat Completions)',
  anthropic: 'Anthropic Messages',
};
/** Протокол из ответа сервера (незнакомое значение — как OpenAI-совместимый). */
export const protocolOf = (p: string | null | undefined): Protocol => (p === 'anthropic' ? 'anthropic' : 'openai');

/** Адрес записи Vault — адрес API модели: `http(s)://хост…` (решение 166). Описания хостов
 * («192.168.0.210 (SSH…)», «локальная машина») выбором подключения не показываются. */
export function isModelUrl(addr: string): boolean {
  const s = addr.trim();
  if (!/^https?:\/\/[^\s/]/i.test(s) || /\s/.test(s)) return false;
  try {
    return !!new URL(s).hostname;
  } catch {
    return false;
  }
}

/** Пункт выбора записи подключения: «<имя> — <адрес>». */
export const vaultChoiceLabel = (title: string, addr: string): string => `${title} — ${addr.trim()}`;

/** Адрес без хвостовых «/». */
const trimSlash = (s: string) => s.trim().replace(/\/+$/, '');

/** Куда уйдёт запрос к модели (как сервер, `request_url`): OpenAI-совместимый —
 * `<адрес>/chat/completions`, Anthropic — `<адрес>/v1/messages`, у адреса с `/v1` — без удвоения. */
export function requestUrlOf(p: Protocol, addr: string): string {
  const b = trimSlash(addr);
  if (p === 'openai') return `${b}/chat/completions`;
  return /\/v1$/i.test(b) ? `${b}/messages` : `${b}/v1/messages`;
}

/** Предупреждение о пути адреса до сохранения (решение 168) и исправленный адрес для «Исправить
 * адрес»: у OpenAI-совместимого адрес без пути — запросы уйдут мимо `/v1`; полная точка входа
 * вместо базы (`…/chat/completions`, `…/v1/messages`) — путь удвоится. Нет замечания — `null`. */
export function pathWarning(p: Protocol, addr: string): { text: string; fixed: string } | null {
  if (!isModelUrl(addr)) return null;
  const b = trimSlash(addr);
  const path = new URL(b).pathname.replace(/\/+$/, '');
  if (p === 'openai') {
    if (/\/chat\/completions$/i.test(path)) {
      const fixed = b.replace(/\/chat\/completions$/i, '');
      return { text: `Адрес — уже точка входа: запрос уйдёт на ${b}/chat/completions; нужен адрес без /chat/completions — ${fixed}`, fixed };
    }
    if (!path) {
      return { text: `Адрес без /v1 — запрос уйдёт на ${b}/chat/completions; обычно нужен ${b}/v1`, fixed: `${b}/v1` };
    }
    return null;
  }
  if (/\/messages$/i.test(path)) {
    const fixed = b.replace(/\/messages$/i, '');
    return { text: `Адрес — уже точка входа: запрос уйдёт на ${requestUrlOf(p, b)}; нужен адрес без /messages — ${fixed}`, fixed };
  }
  return null;
}

// ---- body_extra (§6.2 п. 1) ----

/** Ключи тела запроса, которые задаёт сервер (OpenAI-совместимый). */
export const RESERVED_BODY_KEYS = ['model', 'messages', 'stream', 'max_tokens', 'max_completion_tokens'];
/** Ключи тела Anthropic Messages, которые задаёт сервер, и параметры выборки (версия API
 * `2026-10-21` отвергает их у всех моделей, §9.2 п. 2). */
export const ANTHROPIC_RESERVED_BODY_KEYS = ['model', 'messages', 'system', 'stream', 'max_tokens', 'temperature', 'top_p', 'top_k'];
export const BODY_EXTRA_MAX_BYTES = 4096;

const reservedOf = (p: Protocol) => (p === 'anthropic' ? ANTHROPIC_RESERVED_BODY_KEYS : RESERVED_BODY_KEYS);

/** Рецепты `body_extra` — особенности поставщиков (таблица руководства, §6.2 п. 3, §9.2 п. 7). */
export const RECIPES: { name: string; title: string; value: Record<string, unknown> }[] = [
  {
    name: 'qwen-local',
    title: 'Qwen на llama.cpp или vLLM: без размышлений',
    value: { chat_template_kwargs: { enable_thinking: false } },
  },
  { name: 'dashscope', title: 'Qwen (DashScope): без размышлений', value: { enable_thinking: false } },
  { name: 'json', title: 'Ответ — JSON-объект', value: { response_format: { type: 'json_object' } } },
];
/** Рецепты Anthropic Messages: глубина размышлений и их выключение (у моделей, где можно). */
export const ANTHROPIC_RECIPES: typeof RECIPES = [
  { name: 'effort-low', title: 'Меньше размышлений (effort: low)', value: { output_config: { effort: 'low' } } },
  { name: 'thinking-off', title: 'Без размышлений (где модель позволяет)', value: { thinking: { type: 'disabled' } } },
];
export const recipesOf = (p: Protocol) => (p === 'anthropic' ? ANTHROPIC_RECIPES : RECIPES);

export type BodyExtra = { value: Record<string, unknown> } | { error: string };

/** `body_extra` из текста поля: пусто — `{}`; JSON-объект до 4 КиБ без ключей сервера протокола
 * `p` (у Anthropic — и без параметров выборки). */
export function parseBodyExtra(text: string, p: Protocol = 'openai'): BodyExtra {
  const t = text.trim();
  if (!t) return { value: {} };
  let v: unknown;
  try {
    v = JSON.parse(t);
  } catch (e) {
    return { error: `не JSON: ${(e as Error).message}` };
  }
  if (!v || typeof v !== 'object' || Array.isArray(v)) return { error: 'нужен JSON-объект: {"ключ": значение}' };
  const bad = Object.keys(v).filter((k) => reservedOf(p).includes(k));
  const sampling = bad.filter((k) => ['temperature', 'top_p', 'top_k'].includes(k));
  if (sampling.length) return { error: `ключи ${sampling.join(', ')} Anthropic не принимает (версия API 2026-10-21) — уберите их` };
  if (bad.length) return { error: `ключи ${bad.join(', ')} задаёт сервер — уберите их` };
  const size = new TextEncoder().encode(JSON.stringify(v)).length;
  if (size > BODY_EXTRA_MAX_BYTES) return { error: `${size} байт — больше 4 КиБ` };
  return { value: v as Record<string, unknown> };
}

/** Текст поля `body_extra` для объекта (пустой объект — пустое поле). */
export const bodyExtraText = (v: Record<string, unknown> | null | undefined): string =>
  v && Object.keys(v).length ? JSON.stringify(v, null, 2) : '';

/** Рецепт в поле: ключи рецепта — поверх набранного (верхним уровнем, как сервер сливает тело);
 * набранное не JSON-объект — `null` (поле не трогается). */
export function withRecipe(text: string, recipe: Record<string, unknown>): string | null {
  let base: Record<string, unknown> = {};
  if (text.trim()) {
    try {
      const v: unknown = JSON.parse(text);
      if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
      base = v as Record<string, unknown>;
    } catch {
      return null;
    }
  }
  return JSON.stringify({ ...base, ...recipe }, null, 2);
}

// ---- время ночи (§6.1 пп. 3–4; WebUI — пояс сервера рядом с местным) ----

/** Смещение пояса `tz` (мин) в момент `utc`: IANA через `Intl`, `UTC`, `±HH:MM`. */
export function tzOffsetMin(utc: number, tz: string): number {
  const fixed = /^([+-])(\d{2}):(\d{2})$/.exec(tz);
  if (fixed) return (fixed[1] === '-' ? -1 : 1) * (Number(fixed[2]) * 60 + Number(fixed[3]));
  if (tz.toUpperCase() === 'UTC') return 0;
  const f = new Intl.DateTimeFormat('en-US', {
    timeZone: tz,
    hourCycle: 'h23',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
  const p = Object.fromEntries(f.formatToParts(new Date(utc)).map((x) => [x.type, x.value]));
  const wall = Date.UTC(Number(p.year), Number(p.month) - 1, Number(p.day), Number(p.hour), Number(p.minute), Number(p.second));
  return Math.round((wall - utc) / 60_000);
}

/** Ближайшее (сегодня или завтра в поясе `tz`) начало ночи `at` (`HH:MM`) — момент UTC (мс). */
export function nextSlot(at: string, tz: string, now: number): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(at);
  if (!m) return null;
  let off: number;
  try {
    off = tzOffsetMin(now, tz);
  } catch {
    return null;
  }
  const wall = new Date(now + off * 60_000);
  for (const add of [0, 1]) {
    const guess = Date.UTC(wall.getUTCFullYear(), wall.getUTCMonth(), wall.getUTCDate() + add, Number(m[1]), Number(m[2]));
    // местное время пояса → UTC: смещение в найденный момент (переход на летнее время — вторым шагом)
    let t = guess - tzOffsetMin(guess, tz) * 60_000;
    const o2 = tzOffsetMin(t, tz);
    t = guess - o2 * 60_000;
    if (t > now) return t;
  }
  return null;
}

/** Окно ночи: «2 ч», «1 ч 30 мин», «45 мин». */
export function windowText(min: number): string {
  const h = Math.floor(min / 60);
  const m = min % 60;
  if (!h) return `${m} мин`;
  return m ? `${h} ч ${m} мин` : `${h} ч`;
}

/** «04:30 Europe/Moscow (у вас 04:30), окно 2 ч» (§6.9 п. 1). `viewerTz` — пояс зрителя (тесты). */
export function nightText(n: { at: string; tz: string; window_min: number }, now: number, viewerTz?: string): string {
  const slot = nextSlot(n.at, n.tz, now);
  const local = slot === null ? '' : localTime(new Date(slot).toISOString(), { hour: '2-digit', minute: '2-digit', ...(viewerTz ? { timeZone: viewerTz } : {}) });
  const at = /^\d:/.test(n.at) ? `0${n.at}` : n.at;
  return `${at} ${n.tz}${local ? ` (у вас ${local})` : ''}, окно ${windowText(n.window_min)}`;
}

// ---- токены и журнал отправок (§6.2 пп. 7–8) ----

const NUM = new Intl.NumberFormat('ru-RU');
const PCT = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });

/** Число с разрядами: «12 345». */
export const num = (n: number): string => NUM.format(n);

// ---- личный предел токенов (M7.6 §9.9, решение 172) ----

/** Личный предел строкой: «400 000 в сутки · 900 000 в месяц»; нет — «нет (потолок сервера)». */
export function limitsText(l: { daily_tokens: number; monthly_tokens: number } | null | undefined): string {
  if (!l) return 'нет (потолок сервера)';
  return `${num(l.daily_tokens)} в сутки · ${num(l.monthly_tokens)} в месяц`;
}

/** Поля окна «Предел токенов» → вход `works.limits`: пустое поле — потолок сервера, оба пустые —
 * личного предела нет (`null`); целое от 0 до потолка; иначе — текст ошибки. */
export function parseLimits(
  daily: string,
  monthly: string,
  ceiling: { daily: number; monthly: number },
): { value: { daily_tokens: number; monthly_tokens: number } | null } | { error: string } {
  const d = daily.trim();
  const m = monthly.trim();
  if (!d && !m) return { value: null };
  const one = (raw: string, cap: number, what: string): number | string => {
    if (!raw) return cap;
    const n = Number(raw.replace(/\s/g, ''));
    if (!Number.isInteger(n) || n < 0) return `${what}: целое число токенов, 0 или больше.`;
    if (n > cap) return `${what}: не больше потолка сервера — ${num(cap)}.`;
    return n;
  };
  const dv = one(d, ceiling.daily, 'В сутки');
  if (typeof dv === 'string') return { error: dv };
  const mv = one(m, ceiling.monthly, 'В месяц');
  if (typeof mv === 'string') return { error: mv };
  return { value: { daily_tokens: dv, monthly_tokens: mv } };
}

/** Расход к пределу: «12 345 из 2 000 000 (0,6 %)»; предел 0 — «обращений нет: предел 0». */
export function usageText(tokens: number, limit: number): string {
  if (limit <= 0) return `${num(tokens)} — обращений нет: предел 0`;
  return `${num(tokens)} из ${num(limit)} (${PCT.format((tokens / limit) * 100)} %)`;
}

/** Итог строки журнала отправок и класса ошибки — по-русски (§6.2 п. 5). */
export const OUTCOME_TEXT: Record<string, string> = {
  ok: 'ok',
  auth: 'ключ не принят',
  not_found: 'не найдено',
  bad_request: 'запрос отвергнут',
  quota: 'квота поставщика',
  rate: 'частота запросов',
  unavailable: 'модель недоступна',
  thinking: 'предел съели размышления',
  max_tokens: 'ответ обрезан пределом',
  refused: 'отказ модели',
  bad_response: 'ответ не по протоколу',
  bad_json: 'ответ не JSON',
  secret_in_payload: 'секрет в пакете',
  blocked: 'адрес запрещён',
  vault: 'запись Vault недоступна',
  vault_changed: 'адрес изменён',
  limit: 'предел токенов',
  aborted: 'оборвано остановкой',
  internal: 'сбой сервера',
};

/** Итог строки журнала: «ok», «ключ не принят (auth, HTTP 401)». */
export function outcomeText(outcome: string, http: number | null | undefined): string {
  if (outcome === 'ok') return 'ok';
  const t = OUTCOME_TEXT[outcome];
  const code = http ? `${outcome}, HTTP ${http}` : outcome;
  return t ? `${t} (${code})` : code;
}

/** Итог проверки подключения (`POST /works/model/test`; M7.6, решение 169 — `url` и `models`). */
export interface TestReply {
  ok: boolean;
  ms: number | null;
  class?: string;
  hint?: string;
  url?: string;
  models?: string[];
}

/** Неудачная проверка подключения — понятной причиной и полным адресом запроса (решение 169):
 * ключ не подошёл, по адресу нет API, нет такой модели (с именами моделей сервера), сервер не
 * отвечает; прочее — класс и подсказка сервера. */
export function testFailText(r: TestReply, model: string): string {
  const cls = r.class ?? '';
  let why: string;
  if (cls === 'auth') why = 'ключ не подошёл — проверьте значение записи Vault';
  else if (cls === 'not_found' && r.models?.length) {
    const more = r.models.length >= 50 ? ', …' : '';
    why = `по адресу отвечает API, но модели «${model}» на нём нет — есть: ${r.models.join(', ')}${more}`;
  } else if (cls === 'not_found') why = 'по адресу нет API — проверьте адрес (у OpenAI-совместимых — с /v1)';
  else if (cls === 'unavailable') why = 'сервер не отвечает — нет соединения или истёк срок ответа';
  else why = `${cls ? outcomeText(cls, null) : 'не работает'}${r.hint ? `. ${r.hint}` : ''}`;
  return `Подключение не работает: ${why}.${r.url ? ` Адрес запроса: ${r.url}` : ''}`;
}

/** Токены строки журнала: «1 200 → 85», оценка — «≈ 1 200 → 85». */
export const callTokensText = (c: { input_tokens: number; output_tokens: number; estimated: boolean }): string =>
  `${c.estimated ? '≈ ' : ''}${num(c.input_tokens)} → ${num(c.output_tokens)}`;

/** Подсказка к «—» в журнале отправок: длительности нет — ответа модели не было. */
export const CALL_NO_MS_HINT = 'ответа не было: обращение оборвано, отказано до отправки или сбой сервера';

/** Длительность строки журнала для столбца «мс»: число или «—», если ответа не было (`ms: null` у
 * `aborted`, `internal`, `limit`, …) — не «0» (финальное ревью M7.3, Minor 2). */
export const callMsText = (ms: number | null | undefined): string => (ms == null ? '—' : num(ms));

/** Длительность с единицей (карточка журнала на телефоне): «1 234 мс» или «— (ответа не было: …)». */
export const callDurationText = (ms: number | null | undefined): string =>
  ms == null ? `— (${CALL_NO_MS_HINT})` : `${num(ms)} мс`;

// ---- работы очереди (§6.3) ----

export const STATE_TEXT: Record<string, string> = {
  queued: 'в очереди',
  running: 'идёт',
  done: 'готово',
  failed: 'сбой',
  expired: 'истекла',
  cancelled: 'снята',
};

export const ORIGIN_TEXT: Record<string, string> = { nightly: 'ночь', manual: 'вручную' };

/** Итог работы (`result`): «принято 3 · не записано 1 · отклонено 1 · отказ 1 · без итога 2»;
 * «не записано» — принятое, которое закрытие работы не записало (`warnings`: предел, повтор). */
export function workResultText(result: unknown): string {
  if (!result || typeof result !== 'object') return '';
  const r = result as Record<string, unknown>;
  const n = (k: string) => (Array.isArray(r[k]) ? (r[k] as unknown[]).length : 0);
  const parts = [`принято ${n('accepted')}`];
  if (n('warnings')) parts.push(`не записано ${n('warnings')}`);
  if (n('rejected')) parts.push(`отклонено ${n('rejected')}`);
  if (n('skip')) parts.push(`отказ ${n('skip')}`);
  if (n('skipped')) parts.push(`без итога ${n('skipped')}`);
  if (typeof r.skip_reason === 'string' && r.skip_reason) parts.push(`отказ от работы: ${r.skip_reason}`);
  return parts.join(' · ');
}

// ---- кандидаты в Rules (§6.7 п. 4) ----

export const PROPOSAL_STATUS_TEXT: Record<string, string> = {
  proposed: 'ждёт решения',
  accepted: 'принят',
  declined: 'отклонён',
  withdrawn: 'отозван автором',
  stale: 'устарел',
  skipped: 'отказ исполнителя',
};

/** Автор кандидата: «агент, сессия s-1», «модель qwen3» (`author` модели — `model:<имя>`). */
export function proposalAuthorText(kind: string, author: string | null | undefined, session?: string | null): string {
  if (kind === 'model') return `модель ${(author ?? '').replace(/^model:/, '') || '?'}`;
  const s = session ?? author;
  return s ? `агент, сессия ${s}` : 'агент';
}

// ---- сводка администратора `GET /admin/works` (M7.4 §7.6 п. 2, §7.7 п. 2) ----

/** Действующие режимы видов строкой: «пробы: агент · кандидаты в Rules: выкл · уборка: выкл»; вид
 * без строки — «выкл». */
export function modesSummary(modes: Record<string, { mode: WorksMode }> | null | undefined): string {
  return WORK_KINDS.map((k) => `${KIND_SHORT[k]}: ${MODE_TITLE[modes?.[k]?.mode ?? 'off'] ?? modes?.[k]?.mode}`).join(' · ');
}

/** Очередь строкой: «2 в очереди · 1 идёт», пусто — «пусто». */
export function queueText(queued: number | null | undefined, running: number | null | undefined): string {
  const parts = [...(queued ? [`${num(queued)} в очереди`] : []), ...(running ? [`${num(running)} идёт`] : [])];
  return parts.length ? parts.join(' · ') : 'пусто';
}

/** Вердикт прогона коротко (сводка, без чисел флагов — они на странице «Проверка памяти»). */
const VERDICT_SHORT: Record<string, string> = {
  ok: 'в норме',
  regression: 'ухудшение',
  baseline: 'накопление базы',
  changed: 'изменилось',
  degraded: 'поиск без векторов',
};
/** Прогон без вердикта коротко. */
const RUN_SHORT: Record<string, string> = { skipped: 'пропущен', timeout: 'предел времени', failed: 'сбой' };

/** Последний законченный прогон проверки строкой: вердикт, иначе состояние; прогонов нет —
 * «не было». */
export function adminCheckText(c: { status: string | null; verdict: string | null } | null | undefined): string {
  if (!c?.status) return 'не было';
  if (c.verdict) return VERDICT_SHORT[c.verdict] ?? c.verdict;
  return RUN_SHORT[c.status] ?? c.status;
}

/** Сумма Rules в дайджесте и прирост (§6.7 п. 4): «Rules в каждой сессии: 1 840 → 1 990 токенов
 * (+150)», «0 → 43 токена (+43)» — форма по второму числу. */
export function rulesTokensText(t: { now: number; after: number } | null | undefined): string {
  if (!t) return '';
  const d = t.after - t.now;
  const word = plural(t.after, 'токен', 'токена', 'токенов');
  return `Rules в каждой сессии: ${num(t.now)} → ${num(t.after)} ${word} (${d >= 0 ? '+' : '−'}${num(Math.abs(d))})`;
}
