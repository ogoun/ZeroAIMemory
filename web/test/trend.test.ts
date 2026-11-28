// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Тренд ночной проверки (M7.4, спецификация §7.7 п. 1): точки и масштаб своего SVG, разрыв линии
// на пропущенном прогоне, подпись для чтения с экрана.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { TrendRow } from '../src/lib/checkVerdict.ts';
import { chronological, niceMax, segmentPath, trendChart, trendSummary, xPositions, type TrendBox } from '../src/lib/trend.ts';

const box: TrendBox = { width: 340, height: 140, left: 40, right: 20, top: 10, bottom: 30 };

let n = 0;
/** Прогон тренда; `date` — по порядку создания (сервер отдаёт новые первыми). */
const row = (x: Partial<TrendRow> = {}): TrendRow => {
  n += 1;
  return {
    run: `01KQ0000000000000000000R${String(n).padStart(2, '0')}`,
    date: `2026-11-${String(n).padStart(2, '0')}`,
    trigger: 'nightly',
    status: 'done',
    verdict: 'ok',
    hit_search: 0.5,
    hit_prompt: 0.25,
    negatives_shown: 0.1,
    p95: 120,
    ...x,
  };
};

test('trend_points_scale', () => {
  // сервер: новые первыми — график: старые слева
  const old = row({ hit_search: 0, p95: 80 });
  const mid = row({ hit_search: 0.5, p95: 120, verdict: 'regression' });
  const last = row({ hit_search: 1, p95: 340 });
  const rows = [last, mid, old];
  assert.deepEqual(chronological(rows).map((r) => r.run), [old.run, mid.run, last.run]);

  // X — поровну по ширине области между полями, один прогон — посередине
  assert.deepEqual(xPositions(3, box), [40, 180, 320]);
  assert.deepEqual(xPositions(1, box), [180]);
  assert.deepEqual(xPositions(0, box), []);

  // доли: 0 — низ области, 1 — верх, 0.5 — середина
  const share = trendChart(rows, ['hit_search', 'hit_prompt'], 'share', box);
  assert.equal(share.max, 1);
  const hs = share.lines.find((l) => l.key === 'hit_search')!;
  assert.deepEqual(
    hs.points.map((p) => [p.x, p.y, p.value]),
    [
      [40, 110, 0],
      [180, 60, 0.5],
      [320, 10, 1],
    ],
  );
  assert.deepEqual(share.ticks, [
    { value: 0, y: 110 },
    { value: 0.5, y: 60 },
    { value: 1, y: 10 },
  ]);
  // ухудшение — у прогона с вердиктом `regression`, у всех рядов
  assert.deepEqual(hs.points.map((p) => p.worse), [false, true, false]);
  assert.deepEqual(share.lines[1].points.map((p) => p.worse), [false, true, false]);
  assert.equal(segmentPath(hs.segments[0]), 'M40 110 L180 60 L320 10');

  // миллисекунды: ось от нуля до «круглого» верха не меньше наибольшего (340 → 500)
  const ms = trendChart(rows, ['p95'], 'ms', box);
  assert.equal(ms.max, 500);
  // y = верх + высота · (1 − v / 500): 80 → 94, 120 → 86, 340 → 42
  assert.deepEqual(ms.lines[0].points.map((p) => Math.round(p.y * 1000) / 1000), [94, 86, 42]);
  assert.deepEqual(ms.ticks.map((t) => t.value), [0, 250, 500]);
  assert.deepEqual([niceMax(0), niceMax(1), niceMax(1.2), niceMax(130), niceMax(200), niceMax(1999), niceMax(-5)], [1, 1, 2, 200, 200, 2000, 1]);
});

test('trend_gap_for_skipped', () => {
  // пропущенный прогон посередине — разрыв линии: два отрезка, у пропуска своё место по X без точки
  const a = row({ hit_search: 0.4 });
  const b = row({ hit_search: 0.6 });
  const skipped = row({ status: 'skipped', verdict: null, hit_search: null, hit_prompt: null, p95: null });
  const c = row({ hit_search: 0.5 });
  const failedWithNumbers = row({ status: 'failed', verdict: null, hit_search: 0.9 });
  const d = row({ hit_search: 0.7, hit_prompt: null });
  const rows = [d, failedWithNumbers, c, skipped, b, a];
  const ch = trendChart(rows, ['hit_search', 'hit_prompt'], 'share', box);
  assert.equal(ch.xs.length, 6);
  const hs = ch.lines[0];
  // прогон не `done` — пропуск, даже если числа есть (сбой: метрики неполны)
  assert.deepEqual(hs.points.map((p) => p.i), [0, 1, 3, 5]);
  assert.deepEqual(hs.segments.map((s) => s.map((p) => p.i)), [[0, 1], [3], [5]]);
  assert.equal(hs.points[2].x, ch.xs[3]);
  // значения нет (`hit_prompt` у последнего — подсказки выключены) — тоже разрыв
  const hp = ch.lines[1];
  assert.deepEqual(hp.segments.map((s) => s.map((p) => p.i)), [[0, 1], [3]]);
  // подпись: число прогонов, период, последнее и разброс, пропуски
  assert.equal(
    trendSummary(rows),
    'Тренд за 6 прогонов (с ' +
      a.date +
      ' по ' +
      d.date +
      '): найдено поиском (test) — последний 70 %, от 40 % до 70 %; среди подсказок к запросу (test) — последний 25 %; ' +
      'задержка поиска p95 — последний 120 мс; ухудшений нет, пропусков — 2.',
  );
  assert.equal(trendSummary([]), 'Прогонов ещё не было.');
  assert.match(trendSummary([row({ verdict: 'regression' })]), /^Тренд за 1 прогон \(2026-11-\d\d\): .*прогонов с ухудшением — 1\.$/);
});
