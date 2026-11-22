// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Группировка сессий страницы «Сессии» (M7.2 §5.8 п. 3, ревью задачи 9 — Minor 1, 2, 5): субагенты
// под сессией верхнего уровня, внук — под тем же корнем, самоссылка и цикл не прячут сессию, окно
// списка и предел ответа — признак неполного числа.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { descendantsOf, groupSessions, maybePartial, rootOf, type TreeSession } from '../src/lib/sessionTree.ts';

/** Сессия: id, родитель, начало и последнее обращение (минуты от 00:00 одной даты). */
function s(id: string, parent: string | null, start: number, last = start): TreeSession {
  const at = (m: number) => `2026-11-22T${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}:00.000000Z`;
  return { id, parent_id: parent, started_at: at(start), last_seen_at: at(last) };
}

const ids = (list: TreeSession[]) => list.map((x) => x.id);

test('sessions_group_subagents_under_parent', () => {
  // список — свежие первыми, как у сервера
  const items = [s('p:a2', 'p', 30, 40), s('p:a1', 'p', 20, 35), s('p', null, 10, 50), s('q', null, 5, 8)].sort(
    (a, b) => (a.last_seen_at < b.last_seen_at ? 1 : -1),
  );
  const g = groupSessions(items);
  assert.deepEqual(ids(g.top), ['p', 'q']);
  assert.deepEqual(ids(g.children.get('p') ?? []), ['p:a2', 'p:a1']);
  assert.equal(g.children.get('q'), undefined);
});

test('sessions_grandchild_and_orphan_not_lost', () => {
  // внук — под корнем `p`; субагент, чей родитель вне списка, — на верхнем уровне
  const items = [s('p', null, 10, 50), s('p:a1', 'p', 20, 40), s('p:a1:c1', 'p:a1', 25, 30), s('o:a1', 'o', 1, 2)];
  const g = groupSessions(items);
  assert.deepEqual(ids(g.top), ['p', 'o:a1']);
  assert.deepEqual(ids(g.children.get('p') ?? []), ['p:a1', 'p:a1:c1']);
  // каждая сессия — ровно в одном месте
  const placed = [...ids(g.top), ...[...g.children.values()].flatMap(ids)].sort();
  assert.deepEqual(placed, ids(items).sort());
});

test('sessions_self_parent_and_cycle_stay_top', () => {
  const items = [s('loop', 'loop', 1), s('x', 'y', 2), s('y', 'x', 3), s('x:k', 'x', 4)];
  const byId = new Map(items.map((x) => [x.id, x] as const));
  assert.equal(rootOf(items[0], byId), 'loop');
  const g = groupSessions(items);
  // самоссылка — не своя группа; цикл — обе сессии наверху, потомок цикла не пропадает
  assert.deepEqual(ids(g.top).sort(), ['loop', 'x', 'x:k', 'y']);
  assert.equal(g.children.get('loop'), undefined);
  const placed = [...ids(g.top), ...[...g.children.values()].flatMap(ids)].sort();
  assert.deepEqual(placed, ids(items).sort());
});

test('sessions_fetched_complete_group_without_repeats', () => {
  const items = [s('p', null, 10, 50), s('p:a1', 'p', 20, 40)];
  // `?parent=p`: тот же a1, a2 вне окна, внук; сам `p` (самоссылка на сервере) — не в группе
  const fetched = { p: [s('p:a1', 'p', 20, 40), s('p:a2', 'p', 11, 12), s('p:a1:c1', 'p:a1', 21, 45), s('p', 'p', 10, 50)] };
  const g = groupSessions(items, fetched);
  assert.deepEqual(ids(g.children.get('p') ?? []), ['p:a1:c1', 'p:a1', 'p:a2']);
  // карточка: у верхнего уровня — группа, у прочей сессии — только дочитанное, без неё самой
  assert.deepEqual(ids(descendantsOf('p', g, fetched)), ['p:a1:c1', 'p:a1', 'p:a2']);
  assert.deepEqual(ids(descendantsOf('p:a1', g, { 'p:a1': [s('p:a1', 'p:a1', 1), s('p:a1:c1', 'p:a1', 21)] })), ['p:a1:c1']);
  assert.deepEqual(descendantsOf('p:a1', g, {}), []);
});

test('sessions_partial_count_at_window_edge', () => {
  const items = [s('new', null, 100, 120), s('old', null, 10, 90), s('mid', null, 60, 95)];
  // окно не полное — числа точные
  assert.equal(maybePartial(items[1], items, 200, undefined, 500), false);
  // окно полное (предел 3): самое старое обращение — 90; `old` начат в 10 — его субагенты могли
  // остаться за окном, `new` начат в 100 — нет
  assert.equal(maybePartial(items[1], items, 3, undefined, 500), true);
  assert.equal(maybePartial(items[0], items, 3, undefined, 500), false);
  // дочитанные — точные, если ответ не в пределе сервера
  assert.equal(maybePartial(items[1], items, 3, [s('old:a', 'old', 11)], 500), false);
  assert.equal(maybePartial(items[1], items, 3, [s('old:a', 'old', 11), s('old:b', 'old', 12)], 2), true);
});
