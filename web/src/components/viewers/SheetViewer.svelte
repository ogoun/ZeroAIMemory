<script lang="ts">
  // Просмотр листов XLSX/XLS/ODS/CSV (M5): страницы строк из превью сервера
  // (`GET /previews/{sha}?kind=sheet`), виртуальная прокрутка (в DOM — только видимые строки),
  // закреплённые шапка и номера строк, переключение листов (tablist, стрелки), переход к
  // диапазону `locator` из выдачи поиска. Лист `locator` — по имени; лист с секретом в имени —
  // по номеру `sheet:#N` (`ref_no` превью).
  import { untrack } from 'svelte';
  import { ApiError, errorText, previewSheet, type PreviewSheetRef, type SheetPreview } from '../../lib/api';
  import { colName, type CellRange, type Locator } from '../../lib/attachments';

  interface Props {
    user: string;
    sha: string;
    /** Лист и диапазон из выдачи поиска. */
    locator: Extract<Locator, { kind: 'sheet' }> | null;
    onexpired: () => void;
  }

  let { user, sha, locator, onexpired }: Props = $props();

  /** Строк в запросе превью (сервер: ≤ 1000). */
  const PAGE = 200;
  const ROW_H = 28;
  const OVERSCAN = 12;
  /** Столбцов в DOM: шире — подсказка «скачайте файл». */
  const MAX_COLS = 200;

  let sheets = $state<PreviewSheetRef[]>([]);
  let active = $state(0);
  let name = $state('');
  let header = $state<string[]>([]);
  /** Номер строки шапки на листе (данные — со следующей). */
  let headerRow = $state(0);
  let total = $state(0);
  let cols = $state(0);
  let warnings = $state<string[]>([]);
  /** Страницы строк листа по номеру страницы (с 0). */
  let pages = $state<Record<number, string[][]>>({});
  let error = $state('');
  let loading = $state(true);
  /** Диапазон из выдачи — на показанном листе. */
  let range = $state<CellRange | null>(null);

  let scroller = $state<HTMLDivElement | null>(null);
  let scrollTop = $state(0);
  let viewH = $state(600);
  let gen = 0;
  const inflight = new Set<string>();

  const shownCols = $derived(Math.min(cols, MAX_COLS));
  const letters = $derived(Array.from({ length: shownCols }, (_, i) => colName(i)));
  const gridId = `sheet-grid-${Math.random().toString(36).slice(2, 8)}`;

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = `Не удалось показать лист: ${errorText(e)}`;
  }

  /** Лист `locator` в книге: по имени (не секрет) или по номеру `sheet:#N`. */
  function sheetOf(l: Extract<Locator, { kind: 'sheet' }>, all: PreviewSheetRef[]): number | null {
    if (l.no !== null) {
      const byRef = all.find((s) => s.ref_no === l.no);
      if (byRef) return byRef.no;
      // номеров `ref_no` нет (в книге нет листа с секретом) — номер по порядку книги
      return all.some((s) => s.ref_no !== null) ? null : (all.find((s) => s.no === l.no)?.no ?? null);
    }
    return all.find((s) => !s.secret && s.name === l.name)?.no ?? null;
  }

  function apply(p: SheetPreview) {
    sheets = p.sheets;
    active = p.sheet;
    name = p.name;
    header = p.header;
    total = p.total_rows;
    cols = Math.max(p.cols, p.header.length);
    warnings = p.warnings;
    headerRow = p.total_rows > 0 || p.header.length ? p.first_row - 1 : 0;
    pages = { 0: p.rows };
  }

  /** Открыть лист `no`; `jump` — диапазон, к которому прокрутить. */
  async function openSheet(no: number, jump: CellRange | null) {
    const my = ++gen;
    loading = true;
    error = '';
    inflight.clear();
    try {
      const p = await previewSheet(user, sha, no, 1, PAGE);
      if (my !== gen) return;
      apply(p);
      range = jump;
      loading = false;
      if (scroller) {
        scroller.scrollTop = 0;
        scroller.scrollLeft = 0;
      }
      if (jump) requestAnimationFrame(() => scrollToRange(jump));
    } catch (e) {
      if (my === gen) {
        loading = false;
        fail(e);
      }
    }
  }

  async function start() {
    const my = ++gen;
    loading = true;
    error = '';
    try {
      const first = await previewSheet(user, sha, 1, 1, PAGE);
      if (my !== gen) return;
      const l = locator;
      const target = l ? sheetOf(l, first.sheets) : null;
      if (l && target === null) error = `Лист из выдачи (${l.no !== null ? `№${l.no}` : `«${l.name}»`}) не найден в книге.`;
      if (target !== null && target !== first.sheet) {
        await openSheet(target, l!.range);
        return;
      }
      apply(first);
      range = target !== null ? l!.range : null;
      loading = false;
      if (range) {
        const r = range;
        requestAnimationFrame(() => scrollToRange(r));
      }
    } catch (e) {
      if (my === gen) {
        loading = false;
        fail(e);
      }
    }
  }

  $effect(() => {
    void user;
    void sha;
    void locator;
    untrack(() => start());
  });

  /** Индекс строки данных (с 0) по номеру строки листа. */
  const idxOf = (row: number) => row - headerRow - 1;

  function scrollToRange(r: CellRange) {
    if (!scroller) return;
    scroller.scrollTop = Math.max(0, idxOf(r.row1)) * ROW_H;
    scroller.scrollLeft = Math.max(0, r.col1 - 1) * 160;
    scrollTop = scroller.scrollTop;
  }

  // видимые строки (с запасом) и подгрузка их страниц
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  const last = $derived(Math.min(total - 1, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN));
  const visible = $derived.by(() => {
    const out: { row: number; idx: number; cells: string[] | null }[] = [];
    for (let i = first; i <= last; i++) {
      const page = pages[Math.floor(i / PAGE)];
      out.push({ row: headerRow + 1 + i, idx: i, cells: page ? (page[i % PAGE] ?? []) : null });
    }
    return out;
  });

  $effect(() => {
    if (loading || total === 0) return;
    const need = new Set<number>();
    for (let i = first; i <= last; i += PAGE) need.add(Math.floor(i / PAGE));
    need.add(Math.floor(last / PAGE));
    for (const p of need) untrack(() => fetchPage(p));
  });

  async function fetchPage(p: number) {
    const key = `${active}:${p}`;
    if (pages[p] || inflight.has(key)) return;
    inflight.add(key);
    const my = gen;
    try {
      const r = await previewSheet(user, sha, active, headerRow + 1 + p * PAGE, PAGE);
      if (my !== gen) return;
      pages = { ...pages, [p]: r.rows };
    } catch (e) {
      if (my === gen) fail(e);
    } finally {
      inflight.delete(key);
    }
  }

  const inRange = (row: number, col: number) =>
    range !== null && row >= range.row1 && row <= range.row2 && col >= range.col1 && col <= range.col2;

  function onscroll() {
    if (scroller) scrollTop = scroller.scrollTop;
  }

  // листы: клавиатура как у tablist (стрелки, Home, End)
  let tabEls = $state<HTMLButtonElement[]>([]);
  function select(i: number) {
    const s = sheets[i];
    if (!s || s.no === active) return;
    const jump = locator && sheetOf(locator, sheets) === s.no ? locator.range : null;
    void openSheet(s.no, jump);
  }
  function ontabkey(e: KeyboardEvent, i: number) {
    const n = sheets.length;
    let j = -1;
    if (e.key === 'ArrowRight') j = (i + 1) % n;
    else if (e.key === 'ArrowLeft') j = (i - 1 + n) % n;
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = n - 1;
    if (j < 0) return;
    e.preventDefault();
    tabEls[j]?.focus();
    select(j);
  }
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if sheets.length > 1}
  <div class="tabs" role="tablist" aria-label="Листы">
    {#each sheets as s, i (s.no)}
      <button
        type="button"
        role="tab"
        bind:this={tabEls[i]}
        aria-selected={s.no === active}
        aria-controls={gridId}
        tabindex={s.no === active ? 0 : -1}
        title={s.secret ? 'Имя листа скрыто: в нём найден секрет' : undefined}
        onclick={() => select(i)}
        onkeydown={(e) => ontabkey(e, i)}>{s.name}</button
      >
    {/each}
  </div>
{/if}
{#if range}
  <p class="range muted" data-testid="sheet-range">
    Диапазон из выдачи: <strong>{range.text}</strong>
    <button type="button" class="link" onclick={() => range && scrollToRange(range)}>Показать</button>
  </p>
{/if}
{#each warnings as w, i (i)}<p class="warn">Лист показан не полностью: {w}</p>{/each}
{#if cols > MAX_COLS}
  <p class="warn">Показаны первые {MAX_COLS} столбцов из {cols}: остальные — в скачанном файле.</p>
{/if}
{#if loading && !header.length}
  <p class="muted">Загрузка листа…</p>
{:else if !loading && total === 0 && !header.length}
  <p class="muted">Лист пуст.</p>
{/if}
<div
  id={gridId}
  class="grid"
  role="grid"
  aria-label={`Лист «${name}»`}
  aria-rowcount={headerRow + total}
  aria-colcount={shownCols}
  aria-busy={loading}
  tabindex="0"
  style:--cols={shownCols}
  style:--row-h={`${ROW_H}px`}
  bind:this={scroller}
  bind:clientHeight={viewH}
  {onscroll}
>
  <div class="head" role="rowgroup">
    <div class="row letters" aria-hidden="true">
      <div class="rn"></div>
      {#each letters as l (l)}<div class="cell">{l}</div>{/each}
    </div>
    {#if header.length}
      <div class="row hrow" role="row" aria-rowindex={headerRow}>
        <div class="rn" role="rowheader">{headerRow}</div>
        {#each letters as _, c (c)}
          <div class="cell" role="columnheader" class:in-range={inRange(headerRow, c)}>{header[c] ?? ''}</div>
        {/each}
      </div>
    {/if}
  </div>
  <div class="body" role="rowgroup" style:height={`${total * ROW_H}px`}>
    {#each visible as r (r.row)}
      <div class="row" role="row" aria-rowindex={r.row} style:top={`${r.idx * ROW_H}px`}>
        <div class="rn" role="rowheader">{r.row}</div>
        {#each letters as _, c (c)}
          <div class="cell" role="gridcell" class:in-range={inRange(r.row, c)} class:wait={r.cells === null}>
            {r.cells?.[c] ?? ''}
          </div>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  .tabs {
    display: flex;
    gap: 2px;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    margin-bottom: 8px;
  }
  .tabs button {
    flex: none;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: transparent;
    padding: 6px 12px;
    color: var(--muted);
    margin-bottom: -1px;
  }
  .tabs button[aria-selected='true'] {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .range {
    margin: 0 0 8px;
    font-size: 13px;
  }
  .link {
    border: 0;
    background: none;
    padding: 0 4px;
    color: var(--accent);
    text-decoration: underline;
  }
  .grid {
    position: relative;
    height: calc(100vh - var(--header-h) - 220px);
    min-height: 360px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
    font-size: 13px;
  }
  .grid:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .row {
    display: grid;
    grid-template-columns: 56px repeat(var(--cols), 160px);
    height: var(--row-h);
    width: max-content;
  }
  .head {
    position: sticky;
    top: 0;
    z-index: 2;
    width: max-content;
    background: var(--panel);
  }
  .body {
    position: relative;
  }
  .body .row {
    position: absolute;
    left: 0;
  }
  .cell,
  .rn {
    padding: 0 8px;
    line-height: var(--row-h);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    border-right: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }
  .rn {
    position: sticky;
    left: 0;
    z-index: 1;
    text-align: right;
    color: var(--muted);
    background: var(--code-bg);
  }
  .letters .cell,
  .letters .rn {
    color: var(--muted);
    background: var(--code-bg);
    text-align: center;
    font-size: 12px;
  }
  .hrow .cell {
    font-weight: 600;
    background: var(--panel);
  }
  .cell.in-range {
    background: var(--flash);
  }
  .cell.wait {
    background: var(--hover);
  }
</style>
