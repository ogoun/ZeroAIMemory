// Hash-маршрутизатор WebUI.
//
// Маршруты:
// - `#/admin` — администрирование (пользователи и токены);
// - `#/admin/index` — состояние векторного индекса и переиндексация;
// - `#/u/<user_id>` — пространства пользователя;
// - `#/u/<user_id>/s/<space_ref>/n/<node_id>[#<якорь>]` — документ, якорь — `id`
//   элемента в percent-encoding (как в href вики-ссылок сервера: `#/n/<id>#h-<slug>`).
// Пустой или неизвестный адрес — `home` без пользователя (Shell выбирает своего).

export type Route =
  | { name: 'admin' }
  | { name: 'index' }
  | { name: 'home'; user: string | null }
  | { name: 'node'; user: string; space: string; node: string; anchor: string | null };

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
  const parts = h.split('/').filter((p) => p !== '');
  if (parts.length === 1 && parts[0] === 'admin') return { name: 'admin' };
  if (parts.length === 2 && parts[0] === 'admin' && parts[1] === 'index') return { name: 'index' };
  if (parts[0] === 'u' && parts[1]) {
    const user = dec(parts[1]);
    if (parts.length === 6 && parts[2] === 's' && parts[4] === 'n') {
      return { name: 'node', user, space: dec(parts[3]), node: dec(parts[5]), anchor };
    }
    if (parts.length === 2) return { name: 'home', user };
  }
  return { name: 'home', user: null };
}

/** Пользователь маршрута (заметки), если он в адресе. */
export const routeUser = (r: Route): string | null =>
  r.name === 'home' || r.name === 'node' ? r.user : null;

/** Адрес маршрута (`#/…`). */
export function href(r: Route): string {
  switch (r.name) {
    case 'admin':
      return '#/admin';
    case 'index':
      return '#/admin/index';
    case 'home':
      return r.user ? `#/u/${enc(r.user)}` : '#/';
    case 'node':
      return (
        `#/u/${enc(r.user)}/s/${enc(r.space)}/n/${enc(r.node)}` + (r.anchor ? `#${r.anchor}` : '')
      );
  }
}

export const current = (): Route => parse(location.hash);

/**
 * Проверка ухода с текущего адреса (несохранённая правка): `false` — остаться.
 * Синхронная: вызывается из `hashchange`, где переход уже случился и откатывается.
 */
export type Guard = (next: Route) => boolean;

let guard: Guard | null = null;
/** Адрес, на котором стоит приложение (для отката запрещённого перехода). */
let shown = location.hash;

/** Ставит проверку ухода; возвращает снятие. */
export function setGuard(g: Guard): () => void {
  guard = g;
  return () => {
    if (guard === g) guard = null;
  };
}

const allowed = (r: Route): boolean => !guard || guard(r);

/**
 * Переход. `replace` — без новой записи истории; `silent` — только адрес,
 * без события маршрута и без проверки ухода.
 */
export function go(r: Route, opts: { replace?: boolean; silent?: boolean } = {}): void {
  const target = href(r);
  if (!opts.silent && !allowed(r)) return;
  if (opts.replace || opts.silent) {
    // replaceState не порождает hashchange: событие — вручную
    history.replaceState(null, '', target);
    shown = location.hash;
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

function onHashChange() {
  const skip = skipGuard;
  skipGuard = false;
  if (!skip && !allowed(current())) {
    // остаться: вернуть прежний адрес без нового события
    history.replaceState(null, '', shown || location.pathname + location.search);
    return;
  }
  shown = location.hash;
  emit();
}

let attached = false;

/** Подписка на смену маршрута; возвращает отписку. */
export function listen(cb: (r: Route) => void): () => void {
  if (!attached) {
    window.addEventListener('hashchange', onHashChange);
    attached = true;
  }
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
