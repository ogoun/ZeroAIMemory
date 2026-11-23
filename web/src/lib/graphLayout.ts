// Локальный граф связей (M7.2, спецификация §5.10 п. 2): своя силовая раскладка без DOM и без
// `d3-force` (решение координатора 15: новых зависимостей npm нет), отбор ближайших к центру узлов
// и порядок обхода «как в списке» вкладки «Связи». Модуль без runtime-импортов: его читает
// `node --test` (web/test/graphLayout.test.ts) и ленивый чанк `GraphView`.
//
// Раскладка — Фрухтерман — Рейнгольд: отталкивание всех пар k²/d, притяжение по рёбрам d²/k,
// притяжение к середине (по вертикали — сильнее в отношении сторон: раскладка повторяет форму
// области), шаг ограничен «температурой», которая линейно падает до нуля за фиксированное число
// итераций. Длина ребра k — от площади на узел, в пределах 50…150 (не больше трети меньшей
// стороны): подписи читаемы без увеличения у малых графов, а 60 узлов не разлетаются.
// Детерминированная: начальные позиции — по кругу, угол от хэша id (FNV-1a), радиус — по числу
// шагов от центра; случайных чисел нет. Центр закреплён в середине области. После итераций —
// разведение пар ближе `MIN_GAP` (узлы не совпадают). 60 узлов и 120 рёбер — около 4 мс (O(n²) на
// итерацию).

export interface LayoutNode {
  id: string;
}

export interface LayoutEdge {
  src: string;
  dst: string;
  /** Тип связи — для подписи узла в {@link reach}. */
  type?: string;
}

export interface Point {
  x: number;
  y: number;
}

export interface LayoutOptions {
  /** Число итераций (по умолчанию 300). */
  iterations?: number;
  /** Размер области (по умолчанию 600 × 400); центр — в её середине. */
  width?: number;
  height?: number;
  /** id центрального узла: закреплён в середине, остальные — кольцами по шагам от него. */
  center?: string;
}

/** Ближе этого узлы после раскладки не стоят (единицы области). */
export const MIN_GAP = 28;

/** Узел обхода: шаг от центра и тип ребра, которым он найден (у центра — пусто). */
export interface Reached {
  id: string;
  /** Шаг от центра; недостижимый узел — `Infinity`. */
  step: number;
  type: string;
}

/** FNV-1a по кодовым единицам UTF-16: 0 ≤ h < 2³². */
function hash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
  return h >>> 0;
}

/** Число из `v` или `def`, если `v` не конечное положительное. */
const positive = (v: number | undefined, def: number): number =>
  v !== undefined && v > 0 && Number.isFinite(v) ? v : def;

/**
 * Обход в ширину от `center` по рёбрам в обе стороны: у каждого узла сначала исходящие рёбра (в
 * порядке `edges`), затем входящие — поэтому первый шаг идёт в порядке списка «Связи» (исходящие,
 * затем обратные ссылки). Узлы — без повторов id; недостижимые — в конце в порядке `nodes`.
 * Центра нет среди `nodes` — пусто.
 */
export function reach(nodes: LayoutNode[], edges: LayoutEdge[], center: string): Reached[] {
  const known = new Set(nodes.map((n) => n.id));
  if (!known.has(center)) return [];
  const out = new Map<string, LayoutEdge[]>();
  const inc = new Map<string, LayoutEdge[]>();
  for (const e of edges) {
    if (!known.has(e.src) || !known.has(e.dst) || e.src === e.dst) continue;
    (out.get(e.src) ?? out.set(e.src, []).get(e.src)!).push(e);
    (inc.get(e.dst) ?? inc.set(e.dst, []).get(e.dst)!).push(e);
  }
  const seen = new Map<string, Reached>([[center, { id: center, step: 0, type: '' }]]);
  const order: Reached[] = [seen.get(center)!];
  for (let i = 0; i < order.length; i++) {
    const at = order[i];
    const visit = (id: string, e: LayoutEdge) => {
      if (seen.has(id)) return;
      const r = { id, step: at.step + 1, type: e.type ?? '' };
      seen.set(id, r);
      order.push(r);
    };
    for (const e of out.get(at.id) ?? []) visit(e.dst, e);
    for (const e of inc.get(at.id) ?? []) visit(e.src, e);
  }
  for (const n of nodes) {
    if (seen.has(n.id)) continue;
    const r = { id: n.id, step: Infinity, type: '' };
    seen.set(n.id, r);
    order.push(r);
  }
  return order;
}

/**
 * Не больше `max` узлов, ближайших к `center` по шагам (при равном шаге — в порядке обхода
 * {@link reach}), и число всех узлов (без повторов id) — для «показано 60 из N». Узлы — те же
 * объекты, в порядке обхода.
 */
export function nearest<T extends LayoutNode>(
  nodes: T[],
  edges: LayoutEdge[],
  center: string,
  max = 60,
): { nodes: T[]; total: number } {
  const by = new Map<string, T>();
  for (const n of nodes) if (!by.has(n.id)) by.set(n.id, n);
  const order = reach(nodes, edges, center);
  const ids = order.length ? order.map((r) => r.id) : [...by.keys()];
  return { nodes: ids.slice(0, Math.max(0, max)).map((id) => by.get(id)!), total: by.size };
}

/**
 * Координаты узлов в области `width × height` (единицы SVG). Рёбра с неизвестным концом и петли
 * не учитываются; повтор id — один узел. Результат конечный для любых входных данных.
 */
export function layout(nodes: LayoutNode[], edges: LayoutEdge[], o: LayoutOptions = {}): Map<string, Point> {
  const w = positive(o.width, 600);
  const h = positive(o.height, 400);
  const iters = Math.min(2000, Math.floor(positive(o.iterations, 300)));
  const ids: string[] = [];
  const at = new Map<string, number>();
  for (const n of nodes) if (!at.has(n.id)) at.set(n.id, ids.push(n.id) - 1);
  const n = ids.length;
  const res = new Map<string, Point>();
  if (!n) return res;
  const cx = w / 2;
  const cy = h / 2;
  const c = o.center !== undefined ? (at.get(o.center) ?? -1) : -1;
  const links: number[] = [];
  for (const e of edges) {
    const a = at.get(e.src);
    const b = at.get(e.dst);
    if (a !== undefined && b !== undefined && a !== b) links.push(a, b);
  }
  // идеальная длина ребра; кольца начальных позиций — по шагам от центра
  const kmax = Math.min(150, Math.max(50, Math.min(w, h) / 3));
  const k = Math.min(kmax, Math.max(50, 0.9 * Math.sqrt((w * h) / n)));
  const steps = new Map<string, number>();
  if (c >= 0) for (const r of reach(nodes, edges, ids[c])) steps.set(r.id, r.step);
  const far = Math.max(1, ...[...steps.values()].filter(Number.isFinite)) + 1;
  const x = new Float64Array(n);
  const y = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    const hv = hash(ids[i]);
    const a = (hv / 2 ** 32) * 2 * Math.PI;
    const s = i === c ? 0 : c >= 0 ? Math.min(steps.get(ids[i]) ?? far, far) : 1;
    // другие биты хэша сдвигают радиус: близкие углы разных id не дают одной точки (совпавшие
    // точки разводит отталкивание — направление от номеров пары)
    const r = s * k * (0.8 + ((hv >>> 7) % 97) / 400);
    x[i] = cx + r * Math.cos(a);
    y[i] = cy + r * Math.sin(a);
  }
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);
  const k2 = k * k;
  const t0 = k;
  // притяжение к середине; по вертикали — сильнее в отношении сторон (раскладка повторяет форму)
  const gx = 0.2;
  const gy = (gx * w) / h;
  for (let it = 0; it < iters; it++) {
    dx.fill(0);
    dy.fill(0);
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let ex = x[i] - x[j];
        let ey = y[i] - y[j];
        let d2 = ex * ex + ey * ey;
        if (d2 < 1e-6) {
          // совпали: направление — от номеров пары (детерминированно)
          const a = (i * 7 + j * 13) * 2.399963;
          ex = Math.cos(a) * 0.01;
          ey = Math.sin(a) * 0.01;
          d2 = 1e-4;
        }
        const f = k2 / d2;
        dx[i] += ex * f;
        dy[i] += ey * f;
        dx[j] -= ex * f;
        dy[j] -= ey * f;
      }
    }
    for (let l = 0; l < links.length; l += 2) {
      const a = links[l];
      const b = links[l + 1];
      const ex = x[a] - x[b];
      const ey = y[a] - y[b];
      const f = Math.sqrt(ex * ex + ey * ey) / k;
      dx[a] -= ex * f;
      dy[a] -= ey * f;
      dx[b] += ex * f;
      dy[b] += ey * f;
    }
    const t = t0 * (1 - it / iters) + 0.5;
    for (let i = 0; i < n; i++) {
      if (i === c) continue;
      const mx = dx[i] - (x[i] - cx) * gx;
      const my = dy[i] - (y[i] - cy) * gy;
      const m = Math.sqrt(mx * mx + my * my);
      if (m > 0) {
        const s = Math.min(m, t) / m;
        x[i] += mx * s;
        y[i] += my * s;
      }
    }
  }
  separate(x, y, c);
  for (let i = 0; i < n; i++) res.set(ids[i], { x: i === c ? cx : x[i], y: i === c ? cy : y[i] });
  return res;
}

/** Разводит пары ближе {@link MIN_GAP}: каждая сдвигается поровну (закреплённый `fixed` — нет). */
function separate(x: Float64Array, y: Float64Array, fixed: number): void {
  const n = x.length;
  for (let round = 0; round < 100; round++) {
    let moved = false;
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let ex = x[j] - x[i];
        let ey = y[j] - y[i];
        let d = Math.sqrt(ex * ex + ey * ey);
        if (d >= MIN_GAP) continue;
        if (d < 1e-6) {
          const a = (i * 7 + j * 13) * 2.399963;
          ex = Math.cos(a);
          ey = Math.sin(a);
          d = 1;
        }
        const push = (MIN_GAP - d) / d + 1e-9;
        const [si, sj] = i === fixed ? [0, 1] : j === fixed ? [1, 0] : [0.5, 0.5];
        x[i] -= ex * push * si;
        y[i] -= ey * push * si;
        x[j] += ex * push * sj;
        y[j] += ey * push * sj;
        moved = true;
      }
    }
    if (!moved) return;
  }
}
