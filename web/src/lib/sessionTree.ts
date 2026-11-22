// Группировка сессий страницы «Сессии» (M7.2, спецификация §5.8 п. 3): субагенты — под сессией
// верхнего уровня. Сессии субагентов хуки создают с `parent_id` — id главной сессии; внуков и
// самоссылок штатные клиенты не создают, но сервер их принимает — ни одна сессия не должна
// пропасть или попасть в свою же группу (ревью задачи 9, Minor 2). Чистые функции без
// runtime-импортов: их проверяет `node --test` (`web/test/sessionTree.test.ts`).

/** Сессия в объёме, нужном группировке (поля `SessionItem` из `api.ts`). */
export interface TreeSession {
  id: string;
  parent_id: string | null;
  started_at: string;
  last_seen_at: string;
}

/** Группы списка: верхний уровень (порядок списка) и потомки каждого корня (свежие первыми). */
export interface Grouped<S> {
  top: S[];
  children: Map<string, S[]>;
}

/**
 * Корень сессии `s` среди `byId`: подъём по `parent_id`, пока родитель есть в списке. Самоссылка
 * (`parent_id = id`) заканчивает подъём; на цикле (`a` ↔ `b`) сессия — сама себе корень: каждая
 * сессия цикла остаётся на верхнем уровне, ни одна не пропадает.
 */
export function rootOf<S extends TreeSession>(s: S, byId: Map<string, S>): string {
  const seen = new Set<string>([s.id]);
  let cur = s;
  for (;;) {
    const p = cur.parent_id;
    if (!p || p === cur.id) return cur.id;
    const next = byId.get(p);
    if (!next) return cur.id;
    if (seen.has(p)) return s.id;
    seen.add(p);
    cur = next;
  }
}

/** Свежие первыми (`last_seen_at` — RFC 3339 одной длины: сравнение строк), затем по id. */
const fresh = (a: TreeSession, b: TreeSession) =>
  a.last_seen_at === b.last_seen_at ? a.id.localeCompare(b.id) : a.last_seen_at < b.last_seen_at ? 1 : -1;

/**
 * Группы списка `items` (свежие первыми): верхний уровень — сессии, которые сами себе корень
 * ([`rootOf`]); потомки (субагенты и их субагенты) — под корнем, внук — под тем же корнем, что и
 * его родитель. `fetched` — потомки, дочитанные по `GET /sessions?parent=<корень>`: дополняют
 * группу корня сессиями вне списка (повторы id и сам корень не входят).
 */
export function groupSessions<S extends TreeSession>(items: S[], fetched: Record<string, S[]> = {}): Grouped<S> {
  const byId = new Map(items.map((s) => [s.id, s] as const));
  const top: S[] = [];
  const children = new Map<string, S[]>();
  for (const s of items) {
    const r = rootOf(s, byId);
    if (r === s.id) top.push(s);
    else children.set(r, [...(children.get(r) ?? []), s]);
  }
  for (const root of top) {
    const more = (fetched[root.id] ?? []).filter((s) => s.id !== root.id && !byId.has(s.id));
    if (!more.length) continue;
    const seen = new Set<string>();
    const all = [...(children.get(root.id) ?? []), ...more].filter((s) => !seen.has(s.id) && !!seen.add(s.id));
    children.set(root.id, all);
  }
  for (const list of children.values()) list.sort(fresh);
  return { top, children };
}

/**
 * Потомки сессии `id` для карточки: у сессии верхнего уровня — её группа, у прочей (субагент,
 * сессия вне списка) — только дочитанные (`fetched[id]`, без неё самой и повторов).
 */
export function descendantsOf<S extends TreeSession>(id: string, g: Grouped<S>, fetched: Record<string, S[]>): S[] {
  if (g.top.some((s) => s.id === id)) return g.children.get(id) ?? [];
  const seen = new Set<string>([id]);
  return (fetched[id] ?? []).filter((s) => !seen.has(s.id) && !!seen.add(s.id));
}

/**
 * Число потомков сессии может быть неполным (ревью задачи 9, Minor 1). Не дочитаны (`fetched`
 * нет): список — полное окно последних `limit` сессий, а сессия начата не позже самого старого
 * обращения в окне — её субагенты (начаты позже неё) могли остаться за окном. Дочитаны: ответ в
 * пределе сервера (`fetchLimit`) — тоже.
 */
export function maybePartial<S extends TreeSession>(
  s: S,
  items: S[],
  limit: number,
  fetched: S[] | undefined,
  fetchLimit: number,
): boolean {
  if (fetched) return fetched.length >= fetchLimit;
  if (items.length < limit) return false;
  const oldest = items.reduce((m, x) => (x.last_seen_at < m ? x.last_seen_at : m), items[0].last_seen_at);
  return s.started_at <= oldest;
}
