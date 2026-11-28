// Тренд ночной проверки на странице «Проверка памяти» (M7.4, спецификация §7.7 п. 1): точки,
// масштаб и разрывы для своего SVG по `trend` из `GET /check/status` — до 30 прогонов, новые
// первыми (`crates/zam-server/src/api/check.rs`, `TREND_MAX`). Два графика: доли (`hit_search` и
// `hit_prompt` части `test`, ось 0–100 %) и задержка `p95` поиска (мс, своя ось от нуля) — у разных
// единиц нет общей оси. Ось X — прогоны по времени слева направо, у каждого своё место: пропуск
// (прогон не `done` — пропущен, предел времени, сбой, идёт — или значения нет) — разрыв линии, а не
// соседние точки, соединённые через него. Прогон с вердиктом `regression` — точка «ухудшение»
// (страница рисует её цветом и знаком ▼). Подпись для чтения с экрана — `trendSummary`.
// Модуль без DOM; runtime-импорт — только `.ts` (его читает `node --test`, `web/test/trend.test.ts`).
import { msText, pct, type TrendRow } from './checkVerdict.ts';
import { countText, type Forms } from './plural.ts';

/** «прогон» — «за 1 прогон», «за 3 прогона», «за 12 прогонов». */
const RUN_FORMS: Forms = ['прогон', 'прогона', 'прогонов'];

/** Ряд тренда. */
export type TrendKey = 'hit_search' | 'hit_prompt' | 'p95';

/** Подписи рядов (легенда, таблица, подпись для чтения с экрана). */
export const TREND_TITLE: Record<TrendKey, string> = {
  hit_search: 'Найдено поиском (test)',
  hit_prompt: 'Среди подсказок к запросу (test)',
  p95: 'Задержка поиска p95',
};

/** Область графика, px: размер и поля под подписи осей. */
export interface TrendBox {
  width: number;
  height: number;
  left: number;
  right: number;
  top: number;
  bottom: number;
}

/** Точка ряда: место прогона по X, значение по Y. */
export interface TrendPoint {
  run: string;
  /** Номер прогона по времени (0 — самый старый из показанных). */
  i: number;
  x: number;
  y: number;
  value: number;
  /** Прогон с вердиктом «ухудшение». */
  worse: boolean;
}

/** Ряд: все точки и непрерывные отрезки между пропусками (линия рисуется по отрезкам). */
export interface TrendLine {
  key: TrendKey;
  points: TrendPoint[];
  segments: TrendPoint[][];
}

/** Деление оси Y. */
export interface TrendTick {
  value: number;
  y: number;
}

export interface TrendChart {
  box: TrendBox;
  /** Прогоны по времени (старые слева). */
  runs: TrendRow[];
  /** X каждого прогона. */
  xs: number[];
  lines: TrendLine[];
  /** Верх оси Y: у долей — 1, у миллисекунд — «круглое» число не меньше наибольшего. */
  max: number;
  ticks: TrendTick[];
}

/** Прогоны по времени: сервер отдаёт новые первыми, график — старые слева. */
export const chronological = (rows: readonly TrendRow[]): TrendRow[] => [...rows].reverse();

/** Значение ряда у прогона; пропуск (прогон не `done`, значения нет) — `null`. */
export function trendValue(r: TrendRow, key: TrendKey): number | null {
  if (r.status !== 'done') return null;
  const v = r[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

/** «Круглый» верх оси: наименьшее 1, 2 или 5 × 10ᵏ, не меньшее `v` (`v ≤ 0` — 1). */
export function niceMax(v: number): number {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}

/** X прогонов: поровну по ширине области; один прогон — посередине. */
export function xPositions(n: number, box: TrendBox): number[] {
  const w = Math.max(0, box.width - box.left - box.right);
  if (n <= 0) return [];
  if (n === 1) return [box.left + w / 2];
  return Array.from({ length: n }, (_, i) => box.left + (w * i) / (n - 1));
}

/**
 * График рядов `keys` одной единицы (`share` — доли 0..1, ось 0–100 %; `ms` — от нуля до
 * `niceMax` наибольшего значения): точки, отрезки между пропусками, деления оси (0, середина,
 * верх). `rows` — как у сервера, новые первыми.
 */
export function trendChart(rows: readonly TrendRow[], keys: readonly TrendKey[], unit: 'share' | 'ms', box: TrendBox): TrendChart {
  const runs = chronological(rows);
  const xs = xPositions(runs.length, box);
  const values = keys.flatMap((k) => runs.map((r) => trendValue(r, k)).filter((v): v is number => v !== null));
  const max = unit === 'share' ? 1 : niceMax(Math.max(0, ...values));
  const h = Math.max(0, box.height - box.top - box.bottom);
  const yOf = (v: number) => box.top + h * (1 - Math.min(Math.max(v / max, 0), 1));
  const lines = keys.map((key): TrendLine => {
    const points: TrendPoint[] = [];
    const segments: TrendPoint[][] = [];
    let seg: TrendPoint[] = [];
    runs.forEach((r, i) => {
      const v = trendValue(r, key);
      if (v === null) {
        // пропуск — разрыв: отрезок до него закрыт, следующая точка начнёт новый
        if (seg.length) segments.push(seg);
        seg = [];
        return;
      }
      const p: TrendPoint = { run: r.run, i, x: xs[i], y: yOf(v), value: v, worse: r.verdict === 'regression' };
      points.push(p);
      seg.push(p);
    });
    if (seg.length) segments.push(seg);
    return { key, points, segments };
  });
  const ticks = [0, max / 2, max].map((value) => ({ value, y: yOf(value) }));
  return { box, runs, xs, lines, max, ticks };
}

/** Путь SVG отрезка (`M x y L x y …`), координаты — до десятых. */
export const segmentPath = (seg: readonly TrendPoint[]): string =>
  seg.map((p, i) => `${i ? 'L' : 'M'}${round1(p.x)} ${round1(p.y)}`).join(' ');

const round1 = (v: number) => Math.round(v * 10) / 10;

/** Значение ряда строкой: доли — «64 %», p95 — «1 800 мс». */
export const trendText = (key: TrendKey, v: number | null): string => (key === 'p95' ? msText(v) : pct(v));

/**
 * Подпись графика для чтения с экрана: число прогонов и период, по каждому ряду — последнее
 * значение и разброс, число ухудшений и пропусков. Те же числа — в скрытой таблице страницы.
 */
export function trendSummary(rows: readonly TrendRow[]): string {
  const runs = chronological(rows);
  if (!runs.length) return 'Прогонов ещё не было.';
  const first = runs[0].date;
  const last = runs[runs.length - 1].date;
  const period = first === last ? first : `с ${first} по ${last}`;
  const parts = (['hit_search', 'hit_prompt', 'p95'] as const).map((k) => {
    const vs = runs.map((r) => trendValue(r, k)).filter((v): v is number => v !== null);
    if (!vs.length) return `${TREND_TITLE[k].toLowerCase()} — нет данных`;
    const lastV = [...runs].reverse().map((r) => trendValue(r, k)).find((v) => v !== null) ?? null;
    const lo = Math.min(...vs);
    const hi = Math.max(...vs);
    const span = lo === hi ? '' : `, от ${trendText(k, lo)} до ${trendText(k, hi)}`;
    return `${TREND_TITLE[k].toLowerCase()} — последний ${trendText(k, lastV)}${span}`;
  });
  const worse = runs.filter((r) => r.verdict === 'regression').length;
  const gaps = runs.filter((r) => r.status !== 'done').length;
  const tail = [worse ? `прогонов с ухудшением — ${worse}` : 'ухудшений нет', ...(gaps ? [`пропусков — ${gaps}`] : [])];
  return `Тренд за ${countText(runs.length, RUN_FORMS)} (${period}): ${parts.join('; ')}; ${tail.join(', ')}.`;
}
