// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Локальный граф связей (M7.2, спецификация §5.10 п. 2): своя силовая раскладка без DOM —
// детерминированная, без NaN, узлы не совпадают, 60 узлов быстрее 50 мс; отбор 60 ближайших к
// центру (пре-флайт N30) и порядок обхода «как в списке» с типом связи для подписи узла.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { layout, nearest, reach, type LayoutEdge } from '../src/lib/graphLayout.ts';

const W = 600;
const H = 400;

/** Псевдослучайные числа (LCG): одни и те же графы на каждом прогоне. */
function lcg(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

/** Граф: центр `c`, узлы `n1…n(k-1)`; рёбра — остов (у каждого узла ребро к одному из прежних)
 * и случайные до `m` рёбер. */
function graph(k: number, m: number, seed = 7) {
  const rnd = lcg(seed);
  const nodes = [{ id: 'c' }, ...Array.from({ length: k - 1 }, (_, i) => ({ id: `n${i + 1}` }))];
  const edges: LayoutEdge[] = [];
  for (let i = 1; i < k; i++) edges.push({ src: nodes[Math.floor(rnd() * i)].id, dst: nodes[i].id });
  while (edges.length < m) {
    const a = nodes[Math.floor(rnd() * k)].id;
    const b = nodes[Math.floor(rnd() * k)].id;
    if (a !== b) edges.push({ src: a, dst: b });
  }
  return { nodes, edges };
}

/** Координаты конечные, попарные расстояния — не меньше `min`. */
function assertSane(pos: Map<string, { x: number; y: number }>, ids: string[], min = 8) {
  assert.equal(pos.size, ids.length);
  for (const id of ids) {
    const p = pos.get(id);
    assert.ok(p, `нет позиции ${id}`);
    assert.ok(Number.isFinite(p.x) && Number.isFinite(p.y), `${id}: ${p.x}, ${p.y}`);
  }
  for (let i = 0; i < ids.length; i++) {
    for (let j = i + 1; j < ids.length; j++) {
      const a = pos.get(ids[i])!;
      const b = pos.get(ids[j])!;
      const d = Math.hypot(a.x - b.x, a.y - b.y);
      assert.ok(d >= min, `${ids[i]} и ${ids[j]} ближе ${min}: ${d}`);
    }
  }
}

const plain = (m: Map<string, { x: number; y: number }>) => [...m].map(([id, p]) => [id, p.x, p.y]);

test('layout_deterministic_and_finite', () => {
  // центр, три исходящие, обратная и второй шаг — как у заметки e2e
  const nodes = ['c', 'a', 'b', 'g', 'd', 'e'].map((id) => ({ id }));
  const edges = [
    { src: 'c', dst: 'a' },
    { src: 'c', dst: 'b' },
    { src: 'c', dst: 'g' },
    { src: 'd', dst: 'c' },
    { src: 'a', dst: 'e' },
  ];
  const o = { width: W, height: H, center: 'c' };
  const p1 = layout(nodes, edges, o);
  const p2 = layout(nodes, edges, o);
  assert.deepEqual(plain(p1), plain(p2), 'два вызова — те же координаты');
  assertSane(p1, nodes.map((n) => n.id));
  // центр — в середине области
  assert.deepEqual(p1.get('c'), { x: W / 2, y: H / 2 });

  // звезда из 40 листьев, ребро-петля, ребро к неизвестному узлу и повтор ребра — без NaN и
  // совпадений; узел вне связей тоже получает место
  const star = [{ id: 'c' }, ...Array.from({ length: 40 }, (_, i) => ({ id: `s${i}` })), { id: 'alone' }];
  const starEdges = [
    ...star.slice(1, 41).map((n) => ({ src: 'c', dst: n.id })),
    { src: 's1', dst: 's1' },
    { src: 's2', dst: 'нет такого' },
    { src: 'c', dst: 's3' },
  ];
  const ps = layout(star, starEdges, o);
  assertSane(ps, star.map((n) => n.id));
  assert.deepEqual(ps.get('c'), { x: W / 2, y: H / 2 });
  assert.deepEqual(plain(ps), plain(layout(star, starEdges, o)));

  // пусто; один узел — в середине; без центра — конечные и не совпадают
  assert.equal(layout([], [], o).size, 0);
  assert.deepEqual(layout([{ id: 'c' }], [], o).get('c'), { x: W / 2, y: H / 2 });
  const free = graph(12, 20, 3);
  assertSane(layout(free.nodes, free.edges, { width: W, height: H }), free.nodes.map((n) => n.id));
  // повтор id — одна позиция; неверные размеры и число итераций — не NaN
  const dup = layout([{ id: 'c' }, { id: 'x' }, { id: 'x' }], [{ src: 'c', dst: 'x' }], {
    width: Number.NaN,
    height: 0,
    iterations: -5,
    center: 'c',
  });
  assertSane(dup, ['c', 'x']);
});

test('layout_60_nodes_fast', () => {
  const { nodes, edges } = graph(60, 120);
  assert.equal(edges.length, 120);
  const o = { width: W, height: H, center: 'c' };
  layout(nodes, edges, o); // прогрев JIT
  const times: number[] = [];
  let last = new Map<string, { x: number; y: number }>();
  for (let i = 0; i < 5; i++) {
    const t0 = performance.now();
    last = layout(nodes, edges, o);
    times.push(performance.now() - t0);
  }
  times.sort((a, b) => a - b);
  const median = times[2];
  assert.ok(median < 50, `медиана ${median.toFixed(1)} мс (прогоны: ${times.map((t) => t.toFixed(1)).join(', ')})`);
  assertSane(last, nodes.map((n) => n.id));
  assert.deepEqual(last.get('c'), { x: W / 2, y: H / 2 });
});

test('nearest_60_of_n', () => {
  // 100 узлов: 10 на первом шаге от центра, 30 — на втором, 59 — на третьем; направление рёбер
  // не важно (обратные ссылки — тоже шаг); порядок входа перемешан
  const step1 = Array.from({ length: 10 }, (_, i) => `a${i}`);
  const step2 = Array.from({ length: 30 }, (_, i) => `b${i}`);
  const step3 = Array.from({ length: 59 }, (_, i) => `z${i}`);
  const edges: LayoutEdge[] = [
    ...step1.map((id, i) => (i % 2 ? { src: 'c', dst: id } : { src: id, dst: 'c' })),
    ...step2.map((id, i) => ({ src: step1[i % 10], dst: id })),
    ...step3.map((id, i) => (i % 3 ? { src: step2[i % 30], dst: id } : { src: id, dst: step2[i % 30] })),
  ];
  const rnd = lcg(11);
  const ids = ['c', ...step1, ...step2, ...step3];
  const shuffled = ids.map((id) => ({ id, k: rnd() })).sort((x, y) => x.k - y.k).map(({ id }) => ({ id, title: id }));
  const r = nearest(shuffled, edges, 'c');
  assert.equal(r.total, 100, 'всего — все узлы');
  assert.equal(r.nodes.length, 60, 'показано 60');
  const shown = new Set(r.nodes.map((n) => n.id));
  // центр, весь первый и второй шаг, 19 узлов третьего — ближайшие
  for (const id of ['c', ...step1, ...step2]) assert.ok(shown.has(id), `${id} не показан`);
  assert.equal(step3.filter((id) => shown.has(id)).length, 19);
  // узлы — те же объекты (заголовки на месте), первым — центр
  assert.equal(r.nodes[0].id, 'c');
  assert.ok(r.nodes.every((n) => n.title === n.id));
  // предел больше числа узлов — все; свой предел
  assert.equal(nearest(shuffled.slice(0, 5), edges, 'c').nodes.length, 5);
  const ten = nearest(shuffled, edges, 'c', 11);
  assert.deepEqual(new Set(ten.nodes.map((n) => n.id)), new Set(['c', ...step1]));
  assert.equal(ten.total, 100);
  // недостижимый узел — после достижимых
  const lone = nearest([{ id: 'x' }, { id: 'c' }, { id: 'a0' }], edges, 'c', 2);
  assert.deepEqual(lone.nodes.map((n) => n.id), ['c', 'a0']);
  assert.equal(lone.total, 3);
});

test('reach_list_order_and_types', () => {
  // порядок «как в списке» вкладки «Связи»: сначала исходящие центра в порядке рёбер, затем
  // обратные ссылки, дальше — второй шаг в порядке обхода; тип — у ребра, которым узел найден
  const edges = [
    { src: 'd', dst: 'c', type: 'relates_to' },
    { src: 'c', dst: 'a', type: 'links_to' },
    { src: 'a', dst: 'e', type: 'depends_on' },
    { src: 'c', dst: 'b', type: 'links_to' },
    { src: 'c', dst: 'a', type: 'part_of' },
    { src: 'c', dst: 'g', type: 'mentions' },
    { src: 'f', dst: 'd', type: 'fixes' },
  ];
  const nodes = ['g', 'f', 'e', 'd', 'c', 'b', 'a'].map((id) => ({ id }));
  const r = reach(nodes, edges, 'c');
  assert.deepEqual(
    r.map((x) => [x.id, x.step, x.type]),
    [
      ['c', 0, ''],
      ['a', 1, 'links_to'],
      ['b', 1, 'links_to'],
      ['g', 1, 'mentions'],
      ['d', 1, 'relates_to'],
      ['e', 2, 'depends_on'],
      ['f', 2, 'fixes'],
    ],
  );
  // центр вне узлов — пусто; узел без связей — в конце, шаг бесконечный
  assert.deepEqual(reach(nodes, edges, 'нет'), []);
  const lone = reach([{ id: 'c' }, { id: 'x' }], [], 'c');
  assert.deepEqual(lone.map((x) => [x.id, x.step]), [['c', 0], ['x', Infinity]]);
});
