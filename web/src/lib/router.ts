// Hash-маршрутизатор WebUI.
//
// Маршруты:
// - `#/admin` — администрирование (пользователи и токены);
// - `#/admin/index` — состояние векторного индекса и переиндексация;
// - `#/admin/solutions` — реестр решений и запросы доступа; `#/admin/settings` — настройки;
// - `#/admin/backups` — копии сервера, `#/admin/audit` — журнал аудита (M7.2, §5.6);
// - `#/account` — свой аккаунт (смена пароля, свои токены); `#/my/solutions` — «Мои решения»:
//   доступ к своим решениям (у пользователя, не администратора, §15.1);
// - `#/agent-setup` — «Подключение агента» (§15.5); `#/cache` — «Кэш выжимок» (§15.4);
// - `#/sessions[/<id сессии>]` — «Сессии»: сессии агентов и выданное им по событию (M6 §8),
//   необязательно — открытая сессия (M7.2: на узкой области карточка вместо списка);
// - `#/works` — «Внутренние работы», `#/check` — «Проверка памяти», `#/proposals[/<o:id>]` —
//   «Кандидаты в Rules», необязательно — открытый кандидат (M7.3 §6.9); `#/works?user=<id>` —
//   «Внутренние работы» пользователя глазами администратора (M7.4 §7.7 п. 2);
// - `#/u/<user_id>` — пространства пользователя;
// - `#/u/<user_id>/s/<space_ref>/n/<node_id>[#<якорь>]` — документ, якорь — `id`
//   элемента в percent-encoding (как в href вики-ссылок сервера: `#/n/<id>#h-<slug>`);
// - `#/u/<user_id>/s/<space_ref>/n/<node_id>/a/<sha256>/<имя файла>[/<locator>]` — просмотр
//   вложения заметки (M5), `locator` — место в документе (`page:12`, `sheet:Итоги!A1:F40`,
//   `slide:3`, `para:40`): переход из выдачи поиска;
// - `#/u/<user_id>/s/<space_ref>/j/<журнал>[/<entry_id>]` — журнал пространства
//   (`changelog`, `releases`, `reports`, `code`, `insights`, `glossary`, `tasks`, `ideas`,
//   `questions`, `vault`), необязательно — открытая запись (у `reports` — страница отчёта, у `code` —
//   карточка символа: `путь#qual` в percent-encoding, M5.6).
// Пустой или неизвестный адрес — `home` без пользователя (Shell выбирает своего).
import { createGuards, type Guard } from './guards';

/** Журналы пространства: у решения — все, у Knowledge — без Changelog, Releases, Отчётов и
 * Кода (`code` — страница карты кода решения, M5.6). */
export type Journal =
  | 'changelog'
  | 'releases'
  | 'reports'
  | 'code'
  | 'insights'
  | 'glossary'
  | 'tasks'
  | 'ideas'
  | 'questions'
  | 'vault';
export const JOURNALS: Journal[] = [
  'changelog',
  'releases',
  'reports',
  'code',
  'insights',
  'glossary',
  'tasks',
  'ideas',
  'questions',
  'vault',
];
const isJournal = (s: string): s is Journal => (JOURNALS as string[]).includes(s);

export type Route =
  | { name: 'admin' }
  | { name: 'index' }
  | { name: 'solutions' }
  | { name: 'settings' }
  | { name: 'backups' }
  | { name: 'audit' }
  | { name: 'account' }
  | { name: 'mysolutions' }
  | { name: 'agentsetup' }
  | { name: 'cache' }
  | { name: 'sessions'; session?: string | null }
  | { name: 'works'; user?: string | null }
  | { name: 'check' }
  | { name: 'proposals'; proposal: string | null }
  | { name: 'journal'; user: string; space: string; journal: Journal; entry: string | null }
  | { name: 'home'; user: string | null }
  | { name: 'node'; user: string; space: string; node: string; anchor: string | null }
  | {
      name: 'attachment';
      user: string;
      space: string;
      node: string;
      sha: string;
      file: string;
      locator: string | null;
    };

const enc = encodeURIComponent;

function dec(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** Разбирает `location.hash` (с ведущим `#` или без). */
export function parse(hash: string): Route {
  let h = hash.startsWith('#') ? hash.slice(1) : hash;
  let anchor: string | null = null;
  const cut = h.indexOf('#');
  if (cut >= 0) {
    anchor = h.slice(cut + 1) || null;
    h = h.slice(0, cut);
  }
  // параметры адреса (`?user=` у «Внутренних работ»); прочие маршруты их не читают — `?` в
  // сегментах кодируется (`enc`)
  let query: URLSearchParams | null = null;
  const qi = h.indexOf('?');
  if (qi >= 0) {
    query = new URLSearchParams(h.slice(qi + 1));
    h = h.slice(0, qi);
  }
  const parts = h.split('/').filter((p) => p !== '');
  if (parts.length === 1 && parts[0] === 'admin') return { name: 'admin' };
  if (parts.length === 1 && parts[0] === 'account') return { name: 'account' };
  if (parts.length === 1 && parts[0] === 'agent-setup') return { name: 'agentsetup' };
  if (parts.length === 1 && parts[0] === 'cache') return { name: 'cache' };
  if ((parts.length === 1 || parts.length === 2) && parts[0] === 'sessions') {
    return { name: 'sessions', session: parts[1] ? dec(parts[1]) : null };
  }
  if (parts.length === 1 && parts[0] === 'works') return { name: 'works', user: query?.get('user') || null };
  if (parts.length === 1 && parts[0] === 'check') return { name: 'check' };
  if ((parts.length === 1 || parts.length === 2) && parts[0] === 'proposals') {
    return { name: 'proposals', proposal: parts[1] ? dec(parts[1]) : null };
  }
  if (parts.length === 2 && parts[0] === 'my' && parts[1] === 'solutions') return { name: 'mysolutions' };
  if (parts.length === 2 && parts[0] === 'admin') {
    if (parts[1] === 'index') return { name: 'index' };
    if (parts[1] === 'solutions') return { name: 'solutions' };
    if (parts[1] === 'settings') return { name: 'settings' };
    if (parts[1] === 'backups') return { name: 'backups' };
    if (parts[1] === 'audit') return { name: 'audit' };
  }
  if (parts[0] === 'u' && parts[1]) {
    const user = dec(parts[1]);
    if ((parts.length === 6 || parts.length === 7) && parts[2] === 's' && parts[4] === 'j') {
      const journal = dec(parts[5]);
      if (isJournal(journal)) {
        const entry = parts[6] ? dec(parts[6]) : null;
        return { name: 'journal', user, space: dec(parts[3]), journal, entry };
      }
    }
    if ((parts.length === 9 || parts.length === 10) && parts[2] === 's' && parts[4] === 'n' && parts[6] === 'a') {
      return {
        name: 'attachment',
        user,
        space: dec(parts[3]),
        node: dec(parts[5]),
        sha: dec(parts[7]),
        file: dec(parts[8]),
        locator: parts[9] ? dec(parts[9]) : null,
      };
    }
    if (parts.length === 6 && parts[2] === 's' && parts[4] === 'n') {
      return { name: 'node', user, space: dec(parts[3]), node: dec(parts[5]), anchor };
    }
    if (parts.length === 2) return { name: 'home', user };
  }
  return { name: 'home', user: null };
}

/** Пользователь маршрута (заметки), если он в адресе. */
export const routeUser = (r: Route): string | null =>
  r.name === 'home' || r.name === 'node' || r.name === 'journal' || r.name === 'attachment' ? r.user : null;

/** Адрес маршрута (`#/…`). */
export function href(r: Route): string {
  switch (r.name) {
    case 'admin':
      return '#/admin';
    case 'index':
      return '#/admin/index';
    case 'solutions':
      return '#/admin/solutions';
    case 'settings':
      return '#/admin/settings';
    case 'backups':
      return '#/admin/backups';
    case 'audit':
      return '#/admin/audit';
    case 'account':
      return '#/account';
    case 'mysolutions':
      return '#/my/solutions';
    case 'agentsetup':
      return '#/agent-setup';
    case 'cache':
      return '#/cache';
    case 'sessions':
      return '#/sessions' + (r.session ? `/${enc(r.session)}` : '');
    case 'works':
      return '#/works' + (r.user ? `?user=${enc(r.user)}` : '');
    case 'check':
      return '#/check';
    case 'proposals':
      return '#/proposals' + (r.proposal ? `/${enc(r.proposal)}` : '');
    case 'journal':
      return (
        `#/u/${enc(r.user)}/s/${enc(r.space)}/j/${r.journal}` + (r.entry ? `/${enc(r.entry)}` : '')
      );
    case 'home':
      return r.user ? `#/u/${enc(r.user)}` : '#/';
    case 'node':
      return (
        `#/u/${enc(r.user)}/s/${enc(r.space)}/n/${enc(r.node)}` + (r.anchor ? `#${r.anchor}` : '')
      );
    case 'attachment':
      return (
        `#/u/${enc(r.user)}/s/${enc(r.space)}/n/${enc(r.node)}/a/${enc(r.sha)}/${enc(r.file)}` +
        (r.locator ? `/${enc(r.locator)}` : '')
      );
  }
}

export const current = (): Route => parse(location.hash);

// ---- проверки ухода (M7.3, спецификация §6.14 п. 4) ----
//
// Стек проверок (`guards.ts`): каждая форма с набранным ставит свою (`pushGuard`), переход по
// адресу спрашивает один раз — синхронным `window.confirm` текстом верхней грязной (решение 92:
// роутер решает переход сразу; асинхронный вопрос — отдельная задача); согласие отпускает все.
// Вызывается из `go` и из `hashchange`, где переход уже случился и откатывается. Закрытие и
// перезагрузка вкладки — один `beforeunload` на весь стек.

const guards = createGuards<Route>((q) => window.confirm(q));

/** Поставить проверку ухода (`dirty(next)` — `next`: адрес, куда уходят); возвращает её снятие. */
export function pushGuard(g: Guard<Route>): () => void {
  return guards.push(g);
}

/** Адрес, на котором стоит приложение (для отката запрещённого перехода). */
let shown = location.hash;

// ---- место в истории (M7.2, ревью задачи 10, круги 3–5) ----
//
// Отменённый переход не должен менять историю: откат — обратный шаг ровно на показанную запись.
// `hashchange` не говорит, чем он вызван, поэтому роутер помнит показанную запись:
// - с Navigation API (Baseline с 2026-01: Chromium 102+, Firefox 147+, Safari 26.2+) — её ключ
//   (`navigation.currentEntry.key`); разница шагов — по одному снимку `navigation.entries()` (место
//   ключа показанной минус место текущей). Абсолютный индекс не годится: на пределе истории вкладки
//   (50 записей в Chromium) во время `hashchange` новой записи он на единицу больше настоящего, а
//   разница в одном снимке точна (ревью r5, N10). Возврат распознаётся по ключу, и запоздавший тоже.
//   Ключ точен и для записей прежней версии до перезагрузки вкладки (ревью r4, N9);
// - без Navigation API (или без ключа показанной в списке) — по номеру записи
//   `history.state.zamIdx`. Счёт относительный, от обрезки истории не зависит: запись без номера —
//   новая (ссылка `<a href>`, `location.hash =`), с номером — переход по истории. Номер ведётся и при
//   Navigation API (признак для тестов).
// Новая запись откатывается «назад» на прежнюю (дубля прежнего адреса нет, отменённая остаётся
// «впереди» со своим номером), «назад»/«вперёд» — на ту же разницу обратно (прежняя запись цела).
// Событие отката (`hashchange` возврата) роутер поглощает; подписчики `onCancel` узнают об отмене
// (панель журнала снимает «идёт закрытие»). Шаг между соседними записями с одинаковым адресом
// (удаление из карточки — `go(список, {replace})`) даёт только `popstate` — показанная запись
// берётся из него (ревью r4, N8). Откат, не пришедший за `RESTORE_MS` (вышел за пределы истории),
// ставит адрес показанной записи на текущую `replaceState`: адрес и экран совпадают (ревью r5, N11).
// Ограничение запасного пути (браузер без Navigation API, вкладка открыта до выпуска): записи
// прежней версии номера не имеют и при проходе по ним считаются новыми (номера идут в обратную
// сторону); откат, пришедший не на ожидаемый адрес, второй раз не спрашивает и ставит прежний адрес
// `replaceState` на место той записи; откат за пределы истории — как выше.

type NavEntry = { key: string };
type Nav = { currentEntry?: NavEntry | null; entries?: () => NavEntry[] };
const nav = (globalThis as { navigation?: Nav }).navigation;

/** Ключ текущей записи (Navigation API) или `null`. */
const navKey = (): string | null => {
  const k = nav?.currentEntry?.key;
  return typeof k === 'string' && k !== '' ? k : null;
};

/** Номер показанной записи (относительный счёт роутера). */
let idx = 0;
/** Ключ показанной записи (Navigation API) или `null`. */
let shownKey: string | null = null;
/** Ждём `hashchange` возврата после отката (адрес показанной записи). */
let restoring: string | null = null;
let restoreTimer: ReturnType<typeof setTimeout> | undefined;
const RESTORE_MS = 1000;

/** Снять ожидание возврата. */
const settle = () => {
  restoring = null;
  if (restoreTimer !== undefined) clearTimeout(restoreTimer);
  restoreTimer = undefined;
};

const numbered = (): number | null => {
  const s = history.state as { zamIdx?: unknown } | null;
  return s && typeof s.zamIdx === 'number' ? s.zamIdx : null;
};

/** Номер записи — в её `history.state` (адрес прежний). */
const stamp = (n: number) => history.replaceState({ ...(history.state ?? {}), zamIdx: n }, '', location.href);

/** Номер записи, на которую пришли (без номера — следующая за показанной), — в её `history.state`. */
const arrived = (): number => {
  const at = numbered() ?? idx + 1;
  if (numbered() !== at) stamp(at);
  return at;
};

/** Разница шагов от текущей записи до показанной по одному снимку списка Navigation API; `null` —
 * API нет или ключей нет в списке. */
const navDelta = (): number | null => {
  const k = navKey();
  if (k === null || shownKey === null || typeof nav?.entries !== 'function') return null;
  const list = nav.entries();
  const from = list.findIndex((x) => x.key === shownKey);
  const to = list.findIndex((x) => x.key === k);
  return from < 0 || to < 0 ? null : from - to;
};

/** Откат не пришёл за `RESTORE_MS`: вышел за пределы истории — адрес показанной записи на текущую
 * (адрес и экран совпадают); уже применён, а `hashchange` ещё в пути — подождать его. */
function lost() {
  restoreTimer = undefined;
  const back = restoring;
  if (back === null) return;
  const k = navKey();
  if ((k !== null && k === shownKey) || location.hash === back) {
    restoreTimer = setTimeout(settle, RESTORE_MS);
    return;
  }
  // показанная теперь — текущая запись (с адресом прежней)
  restoring = null;
  history.replaceState(history.state, '', back);
  idx = numbered() ?? idx;
  shownKey = k ?? shownKey;
}

idx = numbered() ?? 0;
if (numbered() !== idx) stamp(idx);
shownKey = navKey();

/** Показанная запись истории (ключ Navigation API, без него — номер). Сменилась без перехода
 * (откат не пришёл — `lost`) — панель журнала закрывается заменой адреса, а не «назад». */
export const shownEntryId = (): string => shownKey ?? `#${idx}`;

const cancelled = new Set<() => void>();

/** Подписка на отмену перехода проверкой ухода; возвращает отписку. */
export function onCancel(cb: () => void): () => void {
  cancelled.add(cb);
  return () => cancelled.delete(cb);
}

const allowed = (r: Route): boolean => guards.allow(r);

// ---- слои истории (M7.3, спецификация §6.14 п. 1, решение 93) ----
//
// Окно формы на телефоне — во весь экран и выглядит как страница: системную «Назад» человек ждёт как
// «закрыть». Окно при открытии кладёт запись истории с тем же адресом и меткой слоя
// (`history.state.zamLayer`, `pushLayer`); она становится показанной записью роутера (откат
// запрещённого перехода возвращает на неё). «Назад» с неё — `popstate` без смены адреса: роутер
// вызывает `close` слоя (окно закрывается или спрашивает о несохранённом и тогда кладёт слой
// заново), а не переход. Закрытие кнопками снимает запись (`drop`: `history.back()`, которое роутер
// поглощает); переход, пока слой на вершине (`go` — после «Добавить» к карточке созданного, запись из
// Ctrl+K), заменяет запись слоя адресом перехода (`history.replaceState` и разбор маршрута), а не
// делает `back()` и `go()`: иначе «Назад» снял бы не тот шаг. Переход, пока `back()` снятия ещё в
// пути, и новый слой — после него (порядок шагов истории). Окна планшета и компьютера и
// окна-подтверждения слоёв не кладут.

/** Запись слоя: `on` — на вершине истории, `dropping` — снимается (`back()` в пути), `gone` — снята,
 * заменена переходом или ушли с неё «Назад»; `pending` — ждёт снятия прежнего слоя. */
interface Layer {
  id: number;
  close: () => void;
  state: 'pending' | 'on' | 'dropping' | 'gone';
}

/** Запись слоя окна. Закрытие с переходом (после «Добавить» — к карточке созданного) отдельного
 * вызова не требует: любой `go`, пока слой на вершине, заменяет его запись адресом перехода. */
export interface LayerHandle {
  /** Снять запись слоя (окно закрыто кнопкой): `history.back()` без перехода роутера. Слой уже снят,
   * заменён или ушли с него — ничего. */
  drop: () => void;
}

const layers: Layer[] = [];
let layerSeq = 0;
/** Действия, ждущие снятия слоя (`back()` ещё в пути): переходы и новые слои — по порядку. */
let afterDrop: (() => void)[] = [];

const layerOf = (s: unknown): number | null => {
  const v = (s as { zamLayer?: unknown } | null)?.zamLayer;
  return typeof v === 'number' ? v : null;
};
const dropping = () => layers.some((l) => l.state === 'dropping');
/** Слой `l` — верхний и текущая запись истории — его. */
const onTop = (l: Layer) => l.state === 'on' && layers[layers.length - 1] === l && layerOf(history.state) === l.id;

function forget(l: Layer) {
  l.state = 'gone';
  const i = layers.indexOf(l);
  if (i >= 0) layers.splice(i, 1);
}

/** Снятие слоёв закончилось: отложенные действия — по порядку. */
function flushAfterDrop() {
  if (dropping()) return;
  const q = afterDrop;
  afterDrop = [];
  for (const f of q) f();
}

/** Снять запись верхнего слоя `l`: `history.back()`, его `popstate` роутер поглощает. */
function startDrop(l: Layer) {
  l.state = 'dropping';
  history.back();
  // «назад» не пришёл (история обрезана): слой снят, отложенное — дальше
  setTimeout(() => {
    if (l.state !== 'dropping') return;
    forget(l);
    flushAfterDrop();
  }, RESTORE_MS);
}

/** Запись слоя окна во весь экран: `popstate` с неё вызывает `close` (а не переход). */
export function pushLayer(close: () => void): LayerHandle {
  const me: Layer = { id: ++layerSeq, close, state: 'pending' };
  const put = () => {
    if (me.state !== 'pending') return;
    me.state = 'on';
    layers.push(me);
    idx += 1;
    history.pushState({ zamIdx: idx, zamLayer: me.id }, '', location.href);
    // показанная запись — слой: откат запрещённого перехода возвращает на него
    shownKey = navKey() ?? shownKey;
  };
  if (dropping()) afterDrop.push(put);
  else put();
  return {
    drop() {
      if (me.state === 'pending') me.state = 'gone';
      if (me.state !== 'on') return;
      if (!onTop(me)) return forget(me);
      startDrop(me);
    },
  };
}

/**
 * Переход. `replace` — без новой записи истории; `silent` — только адрес,
 * без события маршрута и без проверки ухода. Слой окна на вершине истории — его запись заменяется
 * адресом перехода; снятие слоя ещё в пути — переход после него.
 */
export function go(r: Route, opts: { replace?: boolean; silent?: boolean } = {}): void {
  if (!opts.silent && !allowed(r)) return;
  if (dropping()) {
    afterDrop.push(() => navigate(r, opts));
    return;
  }
  navigate(r, opts);
}

/** Переход после проверки ухода (`go`). */
function navigate(r: Route, opts: { replace?: boolean; silent?: boolean }): void {
  const target = href(r);
  const top = layers[layers.length - 1];
  if (top && onTop(top)) {
    if (target === location.hash && !opts.silent) {
      // тот же адрес: запись слоя снимается (иначе «Назад» дал бы пустой шаг), событие — после
      startDrop(top);
      afterDrop.push(emit);
      return;
    }
    // переход с окна во весь экран: запись слоя становится записью адреса (номер — её)
    forget(top);
    opts = { ...opts, replace: true };
  }
  if (opts.replace || opts.silent) {
    // replaceState не порождает hashchange: событие — вручную; номер записи сохраняется
    history.replaceState({ zamIdx: idx }, '', target);
    shown = location.hash;
    shownKey = navKey() ?? shownKey;
    if (!opts.silent) emit();
  } else if (location.hash === target) {
    emit();
  } else {
    // проверка уже пройдена: hashchange её не повторяет
    const before = location.href;
    skipGuard = true;
    location.hash = target;
    // равнозначный адрес (другая percent-кодировка) не порождает hashchange
    if (location.href === before) skipGuard = false;
  }
}

const listeners = new Set<(r: Route) => void>();
let skipGuard = false;

function emit() {
  const r = current();
  for (const l of listeners) l(r);
}

/** `hashchange`: проверка ухода, откат или событие маршрута. Отменённый переход и возврат после
 * отката до прочих слушателей окна (панель журнала) не доходят — для них адрес не менялся: роутер
 * подписан при загрузке модуля, раньше любого компонента (см. ниже). */
function onHashChange(e: HashChangeEvent) {
  const k = navKey();
  // возврат на показанную запись (по ключу — и запоздавший; без Navigation API — по ожидаемому
  // адресу): событие маршрута не нужно
  if ((k !== null && k === shownKey) || (restoring !== null && location.hash === restoring)) {
    settle();
    idx = numbered() ?? idx;
    shownKey = k ?? shownKey;
    e.stopImmediatePropagation();
    return;
  }
  if (restoring !== null && k === null) {
    // запасной путь: откат пришёл не туда (номера записей разошлись с их местом — история до
    // номеров): второй раз не спрашивать — прежний адрес на месте этой записи; вопрос был один
    history.replaceState(history.state, '', restoring);
    settle();
    idx = numbered() ?? idx;
    e.stopImmediatePropagation();
    return;
  }
  // с Navigation API откат приходит точно на показанную запись: другой переход за время ожидания —
  // переход человека
  settle();
  const skip = skipGuard;
  skipGuard = false;
  // отменённая новая запись тоже получает номер: шаг «вперёд» к ней потом распознаётся как шаг по
  // истории, а не как новая запись (иначе номер совпал бы с текущей)
  const at = arrived();
  if (!skip && !allowed(current())) {
    // остаться, не меняя историю: обратный шаг на показанную запись — новая запись — «назад» на
    // прежнюю (дубля нет); шаг по истории — на ту же разницу обратно (прежняя запись цела)
    const delta = navDelta() ?? idx - at;
    if (delta === 0) {
      // место не различает записи (история до номеров): прежний адрес на месте этой записи
      history.replaceState(history.state, '', shown || location.pathname + location.search);
    } else {
      restoring = shown;
      history.go(delta);
      restoreTimer = setTimeout(lost, RESTORE_MS);
    }
    e.stopImmediatePropagation();
    for (const cb of cancelled) cb();
    return;
  }
  idx = at;
  shown = location.hash;
  shownKey = k;
  emit();
}

/** `popstate` без смены адреса — шаг по истории между соседними записями с одинаковым адресом
 * (удаление из карточки: `go(список, {replace})` после перехода со списка; возврат после `lost`):
 * `hashchange` нет, но показанная запись — другая; без этого следующий откат шагал бы не туда. */
function onPopState() {
  if (restoring !== null || location.hash !== shown) {
    // другой адрес (новая запись по ссылке или `location.hash =` — у неё тоже `popstate`, шаг по
    // истории на другой адрес): решает `hashchange` — с проверкой ухода; слой окна не трогается
    // (отменённый переход возвращает на его запись)
    flushAfterDrop();
    return;
  }
  // тот же адрес — ушли с записи слоя («Назад» или его снятие `drop`): слои выше текущей записи
  // сняты; «Назад» закрывает окно слоя, снятие — нет (окно уже закрыто)
  const at = layerOf(history.state);
  const closes: (() => void)[] = [];
  while (layers.length && layers[layers.length - 1].id !== at) {
    const l = layers.pop()!;
    if (l.state === 'on') closes.push(l.close);
    l.state = 'gone';
  }
  idx = numbered() ?? idx;
  shownKey = navKey() ?? shownKey;
  for (const c of closes) c();
  flushAfterDrop();
}

// Подписка на окно — при загрузке модуля (финальное ревью M7.2, M2), а не в `listen`: оболочка
// зовёт `listen` в своём `onMount`, а он идёт после монтирования детей. Первым роутер оказывался
// лишь потому, что журналы монтируются после загрузки пространств (`journalLoaded` в
// `Shell.svelte`); компонент со слушателем окна, смонтированный сразу с оболочкой, подписался бы
// раньше, и `stopImmediatePropagation` отменённого перехода его не остановил бы.
window.addEventListener('hashchange', onHashChange);
window.addEventListener('popstate', onPopState);
// закрытие и перезагрузка вкладки при набранном в любой форме стека
window.addEventListener('beforeunload', (e) => {
  if (guards.anyDirty()) e.preventDefault();
});

/** Подписка на смену маршрута; возвращает отписку. */
export function listen(cb: (r: Route) => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/** Разбор href вики-ссылки сервера `#/n/<id>[#<якорь>]`; иначе `null`. */
export function wikiTarget(h: string): { node: string; anchor: string | null } | null {
  const m = /^#\/n\/([^#/]+)(?:#(.*))?$/.exec(h);
  if (!m) return null;
  return { node: dec(m[1]), anchor: m[2] ? m[2] : null };
}

/** Декодированный якорь — `id` элемента в документе. */
export const anchorId = (a: string): string => dec(a);
