<script lang="ts">
  // Локальный граф связей (M7.2, спецификация §5.10) — ленивый чанк вида «Граф» вкладки «Связи»
  // документа и раздела «Связи» карточки записи (`Links.svelte`). Данные — `GET /graph` (сервер не
  // меняется): глубина 1 или 2 и фильтр типа — параметрами запроса. Больше 60 узлов — 60 ближайших
  // к текущему и «Показано 60 из N» (и при `truncated` сервера). Раскладка — своя
  // (`lib/graphLayout.ts`, без `d3-force`), текущий узел — в центре. Узлы — ссылки в порядке
  // списка «Связи» (исходящие, затем обратные, затем второй шаг) с именем «<тип связи>:
  // <заголовок>»; значок и цвет вида (заметка, запись) — из переменных темы; рёбра — по типу, подпись
  // — при наведении на ребро и при наведении или фокусе узла. Сдвиг — мышью или одним пальцем,
  // масштаб — щипком и кнопками «+», «−», «По размеру» (Pointer Events). Первый показ — узлы сразу
  // на месте; перестройка (глубина, тип) — 0,3 с из прежних мест, при `prefers-reduced-motion` —
  // без анимации. Печать — список (`Links.svelte`).
  import { onDestroy, untrack } from 'svelte';
  import { ApiError, errorText, graph, type Graph, type GraphNode } from '../lib/api';
  import { layout, nearest, reach, type Point, type Reached } from '../lib/graphLayout';

  interface Props {
    user: string;
    kind: 'node' | 'entry';
    id: string;
    /** Адрес узла (заметки); `null` — переход только через `onopen`. */
    hrefOf: (n: GraphNode) => string | null;
    onopen: (n: GraphNode) => void;
    onexpired: () => void;
    /** Вид «Граф» выбран: скрытый граф (список на экране) не запрашивается. */
    active: boolean;
  }

  let { user, kind, id, hrefOf, onopen, onexpired, active }: Props = $props();

  /** Узлов на графе — не больше (спецификация §5.10 п. 1). */
  const MAX = 60;
  /** Типы связей §8.5 — в порядке сервера (`EdgeType::ALL`). */
  const TYPES = [
    'links_to',
    'relates_to',
    'depends_on',
    'part_of',
    'caused_by',
    'fixes',
    'supersedes',
    'derived_from',
    'mentions',
    'contradicts',
  ];
  /** Радиус узла; у текущего — больше на 4. */
  const R = 11;
  const ANIM_MS = 300;
  /** Значки вида (как в дереве, 16 × 16): заметка и запись журнала. */
  const ICON_NOTE = 'M3.5 1.5h6l3 3v10h-9z M9.5 1.5v3h3 M5.5 8h5 M5.5 10.5h5';
  const ICON_ENTRY = 'M3 3.5h10 M3 6.5h10 M3 9.5h7 M3 12.5h5';

  type Box = { x: number; y: number; w: number; h: number };
  type Item = Reached & { n: GraphNode };
  /** Рёбра одной пары «откуда → куда»: одна линия, подпись — типы через запятую. */
  type Line = { key: string; src: string; dst: string; types: string[] };

  const uid = $props.id();
  let depth = $state(1);
  let type = $state('');
  let data = $state.raw<Graph | null>(null);
  let loading = $state(false);
  let error = $state('');
  let seq = 0;

  let svg = $state<SVGSVGElement | null>(null);
  /** Показанные координаты (во время перестройки — промежуточные). */
  let pos = $state.raw(new Map<string, Point>());
  let view = $state<Box>({ x: 0, y: 0, w: 600, h: 400 });
  let fitBox: Box = { x: 0, y: 0, w: 600, h: 400 };
  /** Узел под указателем или в фокусе: его рёбра — с подписью. */
  let hot = $state('');
  let raf = 0;

  const center = $derived(`${kind === 'node' ? 'n' : 'e'}:${id}`);

  /** Для чего загружен граф (пользователь, узел, глубина, тип): возврат к «Графу» без перемен —
   * без запроса (ревью r1, m3). */
  let loadedFor = '';

  $effect(() => {
    const a = [user, kind, id, depth, type] as const;
    const on = active;
    untrack(() => {
      const key = a.join(' ');
      if (!on || key === loadedFor) return;
      loadedFor = key;
      load(...a);
    });
  });

  /** Чей граф загружен: другой узел или запись — прежний граф не показывается до ответа. */
  let shownFor = '';

  async function load(u: string, k: 'node' | 'entry', i: string, d: number, t: string) {
    const my = ++seq;
    loading = true;
    error = '';
    if (shownFor !== `${u} ${k} ${i}`) {
      shownFor = `${u} ${k} ${i}`;
      data = null;
      pos = new Map();
    }
    try {
      const g = await graph(u, { ...(k === 'node' ? { node: i } : { entry: i }), depth: d, types: t ? [t] : [] });
      if (my === seq) data = g;
    } catch (e) {
      if (my !== seq) return;
      // отказ — повтор при следующем показе «Графа»
      loadedFor = '';
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** 60 ближайших, линии между ними и порядок обхода «как в списке». */
  const model = $derived.by(() => {
    if (!data) return null;
    const c = center;
    const all = data.nodes.map((n) => ({ id: n.ref, n }));
    const edges = data.edges.flatMap((e) => (e.dst && e.dst !== e.src ? [{ src: e.src, dst: e.dst, type: e.type }] : []));
    const cut = nearest(all, edges, c, MAX);
    const by = new Map(cut.nodes.map((x) => [x.id, x.n]));
    const kept = edges.filter((e) => by.has(e.src) && by.has(e.dst));
    const order: Item[] = reach(cut.nodes, kept, c).map((r) => ({ ...r, n: by.get(r.id)! }));
    const lines = new Map<string, Line>();
    for (const e of kept) {
      const key = `${e.src}>${e.dst}`;
      const l = lines.get(key) ?? lines.set(key, { key, src: e.src, dst: e.dst, types: [] }).get(key)!;
      if (!l.types.includes(e.type)) l.types.push(e.type);
    }
    return { order, kept, lines: [...lines.values()], total: cut.total };
  });

  const cap = $derived(
    model && data && (model.total > model.order.length || data.truncated)
      ? `Показано ${model.order.length} из ${model.total}${data.truncated ? '+' : ''}`
      : '',
  );

  $effect(() => {
    const m = model;
    const el = svg;
    if (m && el) untrack(() => place(m.order, m.kept, el));
  });

  onDestroy(() => cancelAnimationFrame(raf));

  /** Раскладка под размер холста, вид «по размеру» и перестройка из прежних мест (первый показ и
   * `prefers-reduced-motion` — сразу). */
  function place(order: Item[], kept: { src: string; dst: string }[], el: SVGSVGElement) {
    const r = el.getBoundingClientRect();
    const w = r.width || 600;
    const h = r.height || 400;
    const next = layout(order, kept, { width: w, height: h, center });
    fit(next, w, h);
    cancelAnimationFrame(raf);
    const from = pos;
    const c0 = next.get(center) ?? { x: w / 2, y: h / 2 };
    // первый показ — сразу на месте; анимация — только перестройка (видно, что изменилось)
    if (!from.size || matchMedia('(prefers-reduced-motion: reduce)').matches) {
      pos = next;
      return;
    }
    const t0 = performance.now();
    const frame = (t: number) => {
      const k = Math.min(1, (t - t0) / ANIM_MS);
      const e = 1 - (1 - k) ** 3;
      const cur = new Map<string, Point>();
      for (const [nid, p] of next) {
        const a = from.get(nid) ?? c0;
        cur.set(nid, { x: a.x + (p.x - a.x) * e, y: a.y + (p.y - a.y) * e });
      }
      pos = cur;
      if (k < 1) raf = requestAnimationFrame(frame);
    };
    frame(t0);
  }

  /** «По размеру»: все узлы с подписями; малый граф не крупнее 1:1 (единица — пиксель холста). */
  function fit(p: Map<string, Point>, w: number, h: number) {
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const q of p.values()) {
      x0 = Math.min(x0, q.x);
      y0 = Math.min(y0, q.y);
      x1 = Math.max(x1, q.x);
      y1 = Math.max(y1, q.y);
    }
    if (!Number.isFinite(x0)) return;
    // поля: по бокам — половина подписи, сверху — узел, снизу — узел и подпись
    const bw = Math.max(x1 - x0 + 160, w);
    const bh = Math.max(y1 - y0 + 70, h);
    fitBox = { x: (x0 + x1 - bw) / 2, y: (y0 + y1 - bh) / 2 + 10, w: bw, h: bh };
    view = { ...fitBox };
  }

  /** Масштаб `f` (> 1 — крупнее) вокруг точки `p` (по умолчанию — середина вида); итог — от 1/4
   * до 8 масштабов «по размеру». */
  function scaled(v: Box, f: number, p: Point = { x: v.x + v.w / 2, y: v.y + v.h / 2 }): Box {
    const w = Math.min(fitBox.w * 4, Math.max(fitBox.w / 8, v.w / f));
    const k = w / v.w;
    return { x: p.x - (p.x - v.x) * k, y: p.y - (p.y - v.y) * k, w, h: v.h * k };
  }

  // ---- сдвиг и щипок (Pointer Events) ----

  const pts = new Map<number, Point>();
  let start: Point = { x: 0, y: 0 };
  /** Был сдвиг или щипок: следующий щелчок — не переход по узлу. */
  let dragged = false;

  function down(e: PointerEvent) {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    // первичный указатель — других нет: забыть отпущенные за холстом без захвата (перо, мышь)
    if (e.isPrimary) pts.clear();
    pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pts.size === 1) {
      start = { x: e.clientX, y: e.clientY };
      dragged = false;
    } else dragged = true;
  }

  function move(e: PointerEvent) {
    const p = pts.get(e.pointerId);
    if (!p || !svg) return;
    // кнопку отпустили за холстом до захвата (или окно потеряло фокус): указатель не тянет вид
    // (ревью r1, m1); у касания `buttons` — 1, пока палец на экране
    if (!(e.buttons & 1)) {
      pts.delete(e.pointerId);
      return;
    }
    const r = svg.getBoundingClientRect();
    // пикселей на единицу вида (`preserveAspectRatio` по умолчанию — вписать)
    const s = Math.min(r.width / view.w, r.height / view.h);
    if (pts.size === 1) {
      if (!dragged) {
        if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < 4) return;
        // захват — только когда начался сдвиг: иначе щелчок по узлу ушёл бы холсту
        dragged = true;
        svg.setPointerCapture(e.pointerId);
      }
      view = { ...view, x: view.x - (e.clientX - p.x) / s, y: view.y - (e.clientY - p.y) / s };
    } else if (pts.size === 2) {
      const [a, b] = [...pts.values()];
      const m0 = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      const d0 = Math.hypot(a.x - b.x, a.y - b.y);
      p.x = e.clientX;
      p.y = e.clientY;
      const m1 = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      const d1 = Math.hypot(a.x - b.x, a.y - b.y);
      const v = { ...view, x: view.x - (m1.x - m0.x) / s, y: view.y - (m1.y - m0.y) / s };
      // точка вида под серединой пальцев остаётся на месте
      const at = {
        x: v.x + (m1.x - r.left - (r.width - v.w * s) / 2) / s,
        y: v.y + (m1.y - r.top - (r.height - v.h * s) / 2) / s,
      };
      view = d0 > 0 && d1 > 0 ? scaled(v, d1 / d0, at) : v;
    }
    p.x = e.clientX;
    p.y = e.clientY;
  }

  const up = (e: PointerEvent) => pts.delete(e.pointerId);
  /** Холст потерял захват (отпускание, отмена) — указатель забыт; захват, переданный холсту от
   * узла (касание), сюда не относится: событие узла всплывает с другой целью. */
  const lost = (e: PointerEvent) => e.target === svg && pts.delete(e.pointerId);

  /** Узел в фокусе с клавиатуры вне видимой части (после «+» или щипка) — вид сдвигается к нему
   * без смены масштаба (ревью r1, m2): SVG сам к фокусу не прокручивает. Фокус указателем (нажатие
   * у края, начало сдвига) вид не трогает. */
  function reveal(nid: string) {
    const p = pos.get(nid);
    if (!p) return;
    const m = R + 24;
    const v = view;
    if (p.x - m >= v.x && p.x + m <= v.x + v.w && p.y - m >= v.y && p.y + m <= v.y + v.h) return;
    view = { ...v, x: p.x - v.w / 2, y: p.y - v.h / 2 };
  }

  function swallow(e: MouseEvent) {
    if (!dragged) return;
    e.preventDefault();
    e.stopPropagation();
    dragged = false;
  }

  /** Переход по узлу; ссылка с адресом и модификатором (Ctrl, ⌘, Shift — новая вкладка, окно) —
   * браузеру: и щелчком, и Enter (ревью r1, m6). */
  function activate(e: MouseEvent | KeyboardEvent, n: GraphNode, h: string | null) {
    if (h && (e.ctrlKey || e.metaKey || e.shiftKey)) return;
    e.preventDefault();
    onopen(n);
  }

  /** Отрезок ребра от края узла до края узла; подпись — ближе к началу (у встречной пары — своя). */
  function seg(l: Line) {
    const p = pos.get(l.src);
    const q = pos.get(l.dst);
    if (!p || !q) return null;
    const dx = q.x - p.x;
    const dy = q.y - p.y;
    const d = Math.hypot(dx, dy) || 1;
    const a = l.src === center ? R + 4 : R;
    const b = (l.dst === center ? R + 4 : R) + 3;
    return {
      x1: p.x + (dx / d) * a,
      y1: p.y + (dy / d) * a,
      x2: q.x - (dx / d) * b,
      y2: q.y - (dy / d) * b,
      tx: p.x + dx * 0.42,
      ty: p.y + dy * 0.42,
    };
  }

  const short = (s: string) => (s.length > 24 ? `${s.slice(0, 23)}…` : s);
</script>

{#snippet body(n: GraphNode, r: number)}
  <circle {r} />
  <path class="ico" d={n.ref.startsWith('n:') ? ICON_NOTE : ICON_ENTRY} transform="translate(-6 -6) scale(0.75)" />
  <text y={r + 14}>{short(n.title)}</text>
{/snippet}

<div class="graph" aria-busy={loading}>
  <div class="row controls">
    <label class="row">Глубина
      <select aria-label="Глубина" bind:value={depth}>
        <option value={1}>1 шаг</option>
        <option value={2}>2 шага</option>
      </select>
    </label>
    <label class="row">Тип
      <select aria-label="Тип связи" bind:value={type}>
        <option value="">все</option>
        {#each TYPES as t (t)}<option value={t}>{t}</option>{/each}
      </select>
    </label>
    <span class="row zoom">
      <button type="button" aria-label="Увеличить" title="Увеличить" onclick={() => (view = scaled(view, 1.25))}>+</button>
      <button type="button" aria-label="Уменьшить" title="Уменьшить" onclick={() => (view = scaled(view, 0.8))}>−</button>
      <button type="button" onclick={() => (view = { ...fitBox })}>По размеру</button>
    </span>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <p class="muted cap" role="status">{cap}</p>
  {#if model && model.order.length > 1}
    <svg
      bind:this={svg}
      class="canvas"
      role="group"
      aria-label="Граф связей"
      viewBox={`${view.x} ${view.y} ${view.w} ${view.h}`}
      onpointerdown={down}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={up}
      onlostpointercapture={lost}
      onclickcapture={swallow}
    >
      <defs>
        <marker id={`${uid}-arrow`} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
          <path d="M0 0L8 4L0 8z" />
        </marker>
      </defs>
      <g aria-hidden="true">
        {#each model.lines as l (l.key)}
          {@const s = seg(l)}
          {#if s}
            <g class={`edge t-${l.types[0]}`} class:hot={hot === l.src || hot === l.dst}>
              <line class="hit" x1={s.x1} y1={s.y1} x2={s.x2} y2={s.y2} />
              <line x1={s.x1} y1={s.y1} x2={s.x2} y2={s.y2} marker-end={`url(#${uid}-arrow)`} />
              <text x={s.tx} y={s.ty}>{l.types.join(', ')}</text>
            </g>
          {/if}
        {/each}
      </g>
      {#each model.order as o (o.id)}
        {@const p = pos.get(o.id)}
        {#if p}
          {#if o.id === center}
            <g
              class="node center"
              class:entry={!o.id.startsWith('n:')}
              role="img"
              aria-label={`Текущая: ${o.n.title}`}
              transform={`translate(${p.x} ${p.y})`}
            >
              {@render body(o.n, R + 4)}
            </g>
          {:else}
            {@const h = hrefOf(o.n)}
            <!-- место — у обёртки: типы Svelte знают `<a>` только как ссылку HTML (без `transform`) -->
            <g transform={`translate(${p.x} ${p.y})`}>
              <a
                class="node"
                class:entry={!o.id.startsWith('n:')}
                href={h ?? undefined}
                role={h ? undefined : 'link'}
                tabindex="0"
                aria-label={`${o.type || 'связь не показана'}: ${o.n.title}`}
                onclick={(e) => activate(e, o.n, h)}
                onkeydown={(e) => e.key === 'Enter' && activate(e, o.n, h)}
                onfocus={(e) => ((hot = o.id), e.currentTarget.matches(':focus-visible') && reveal(o.id))}
                onblur={() => (hot = '')}
                onpointerenter={() => (hot = o.id)}
                onpointerleave={() => (hot = '')}
              >
                {@render body(o.n, R)}
                <title>{o.n.path ?? o.n.title}</title>
              </a>
            </g>
          {/if}
        {/if}
      {/each}
    </svg>
  {:else if data && !loading}
    <p class="muted">{type ? 'Связей этого типа нет.' : 'Связей нет.'}</p>
  {:else if loading}
    <p class="muted">Загрузка…</p>
  {/if}
</div>

<style>
  .graph {
    margin-top: 8px;
  }
  .controls {
    gap: 8px 16px;
  }
  .zoom {
    gap: 4px;
  }
  /* живая область — всегда в дереве доступности (ревью r1, m5): пустая — нулевой высоты */
  .cap {
    margin: 6px 0 0;
    font-size: 13px;
  }
  .cap:empty {
    margin: 0;
  }
  .canvas {
    display: block;
    width: 100%;
    height: clamp(260px, 55vh, 480px);
    margin-top: 8px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    /* сдвиг и щипок — свои (Pointer Events): браузер не прокручивает и не масштабирует */
    touch-action: none;
    user-select: none;
    cursor: grab;
  }
  .edge line {
    stroke: var(--muted);
    stroke-width: 1.5;
  }
  /* широкая невидимая линия — цель наведения подписи ребра (стиль типа её не красит) */
  .edge .hit {
    stroke: transparent;
    stroke-width: 12;
    stroke-dasharray: none;
  }
  marker path {
    fill: var(--muted);
  }
  .edge text,
  .node text {
    font-size: 12px;
    text-anchor: middle;
    paint-order: stroke;
    stroke: var(--panel);
    stroke-width: 3px;
    stroke-linejoin: round;
  }
  .edge text {
    font-size: 11px;
    fill: var(--muted);
    visibility: hidden;
  }
  .edge:hover text,
  .edge.hot text {
    visibility: visible;
    fill: var(--text);
  }
  .edge.hot line:not(.hit) {
    stroke-width: 2.5;
  }
  .t-relates_to line {
    stroke-dasharray: 6 4;
  }
  .t-depends_on line {
    stroke: var(--accent);
  }
  .t-part_of line {
    stroke: var(--callout-tip);
  }
  .t-caused_by line,
  .t-fixes line {
    stroke: var(--callout-warning);
  }
  .t-supersedes line,
  .t-mentions line {
    stroke-dasharray: 2 3;
  }
  .t-derived_from line {
    stroke: var(--callout-important);
    stroke-dasharray: 6 4;
  }
  .t-contradicts line {
    stroke: var(--danger);
  }
  .node {
    cursor: pointer;
  }
  .node circle {
    fill: var(--accent);
    stroke: var(--panel);
    stroke-width: 2;
  }
  .node.entry circle {
    fill: var(--callout-important);
  }
  .center {
    cursor: inherit;
  }
  .center circle {
    stroke: var(--text);
    stroke-width: 2.5;
  }
  .ico {
    fill: none;
    stroke: var(--accent-text);
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .node text {
    fill: var(--text);
  }
  .node:hover circle,
  .node:focus-visible circle {
    stroke: var(--text);
    stroke-width: 3;
  }
  @media (pointer: coarse) {
    .zoom {
      gap: var(--touch-gap);
    }
  }
</style>
