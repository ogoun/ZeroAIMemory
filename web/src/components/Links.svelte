<script lang="ts">
  // Вкладка «Связи» (§8.5): исходящие связи узла или записи (с неразрешёнными ссылками) и
  // обратные ссылки — списком; переход к заметке или записи журнала. С M7.2 (спецификация §5.10)
  // — переключатель «Список / Граф» (ARIA `tablist`, клавиатура — `lib/tabs.ts`): граф — ленивый
  // чанк `GraphView`; список — вид по умолчанию (и на телефоне) и вид печати.
  import { untrack } from 'svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import LazyFail from './LazyFail.svelte';
  import {
    ApiError,
    entryBacklinks,
    entryLinks,
    errorText,
    getEntry,
    listSpaces,
    nodeBacklinks,
    nodeLinks,
    spaceRef,
    type EdgeView,
    type GraphNode,
    type Space,
  } from '../lib/api';
  import { JOURNAL_OF_KIND } from '../lib/entryRefs';
  import { go, href } from '../lib/router';
  import { onTabKey } from '../lib/tabs';

  interface Props {
    user: string;
    kind: 'node' | 'entry';
    id: string;
    onexpired: () => void;
  }

  let { user, kind, id, onexpired }: Props = $props();

  type Item = { edge: EdgeView; other: GraphNode | null; ref: string | null };
  type View = 'list' | 'graph';

  const VIEWS: [View, string][] = [
    ['list', 'Список'],
    ['graph', 'Граф'],
  ];
  const uid = $props.id();
  let view = $state<View>('list');
  let viewTabs = $state<HTMLButtonElement[]>([]);

  let outgoing = $state<Item[]>([]);
  let incoming = $state<Item[]>([]);
  let spaces = $state<Space[]>([]);
  let loading = $state(false);
  let error = $state('');
  let seq = 0;

  const ORIGIN_TEXT: Record<string, string> = {
    wikilink: 'вики-ссылка',
    frontmatter: 'frontmatter',
    explicit: 'явная',
    system: 'системная',
  };

  $effect(() => {
    const u = user;
    const k = kind;
    const i = id;
    untrack(() => load(u, k, i));
  });

  function items(edges: EdgeView[], nodes: GraphNode[], pick: (e: EdgeView) => string | null): Item[] {
    const by = new Map(nodes.map((n) => [n.ref, n]));
    return edges.map((edge) => {
      const ref = pick(edge);
      return { edge, ref, other: ref ? (by.get(ref) ?? null) : null };
    });
  }

  async function load(u: string, k: 'node' | 'entry', i: string) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const [out, back, sp] = await Promise.all([
        k === 'node' ? nodeLinks(u, i) : entryLinks(u, i),
        k === 'node' ? nodeBacklinks(u, i) : entryBacklinks(u, i),
        listSpaces(u),
      ]);
      if (my !== seq) return;
      outgoing = items(out.items, out.nodes, (e) => e.dst);
      incoming = items(back.items, back.nodes, (e) => e.src);
      spaces = sp.spaces;
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** Ссылка пространства узла связи; вне доступных — `null`. */
  function spaceOf(n: GraphNode): string | null {
    const s = spaces.find((x) => x.id === n.space_id);
    return s ? spaceRef(s) : null;
  }

  /** Адрес заметки для ссылки узла графа; у записи — `null`: её журнал известен только по
   * `GET /entries/{id}` (вид записи в узле графа сервер не отдаёт). */
  function hrefOf(n: GraphNode): string | null {
    const space = spaceOf(n);
    if (!space || !n.ref.startsWith('n:')) return null;
    return href({ name: 'node', user, space, node: n.ref.slice(2), anchor: null });
  }

  /** Переход к заметке или к записи в журнале её вида (список и граф). */
  async function open(n: GraphNode) {
    const space = spaceOf(n);
    if (!space) {
      error = 'Пространство цели недоступно.';
      return;
    }
    const [k, target] = [n.ref.slice(0, 1), n.ref.slice(2)];
    if (k === 'n') {
      go({ name: 'node', user, space, node: target, anchor: null });
      return;
    }
    // журнал — по виду записи: у термина словаря тоже есть статус, а задача, идея и вопрос в
    // Changelog не открываются [решение исполнителя, задача 11 M7.2]
    try {
      const e = await getEntry(user, target);
      go({ name: 'journal', user, space, journal: JOURNAL_OF_KIND[e.kind] ?? 'changelog', entry: target });
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    }
  }

  const select = (j: number) => (view = VIEWS[j][0]);
</script>

{#snippet list(title: string, rows: Item[], empty: string)}
  <h4>{title} <span class="muted">{rows.length}</span></h4>
  {#if rows.length === 0}<p class="muted">{empty}</p>{/if}
  <ul class="links-list">
    {#each rows as it (it.edge.id)}
      <li>
        <span class="type" title="Тип связи">{it.edge.type}</span>
        {#if it.other}
          {@const other = it.other}
          <button type="button" class="link" onclick={() => open(other)} disabled={other.deleted}>
            {other.path ?? other.title}
          </button>
          <CertaintyBadge value={other.certainty} />
          {#if other.deleted}<span class="muted small">удалено</span>{/if}
        {:else if it.ref === null}
          <span class="unresolved" title="Цель ссылки не найдена">{it.edge.dst_raw ?? '?'}</span>
          <span class="muted small">не найдено</span>
        {:else}
          <span class="muted">недоступно</span>
        {/if}
        <span class="muted small">{ORIGIN_TEXT[it.edge.origin] ?? it.edge.origin}</span>
      </li>
    {/each}
  </ul>
{/snippet}

<div class="links" aria-busy={loading}>
  <div class="views" role="tablist" aria-label="Вид связей">
    {#each VIEWS as [v, label], i (v)}
      <button
        type="button"
        role="tab"
        id={`${uid}-tab-${v}`}
        bind:this={viewTabs[i]}
        aria-selected={view === v}
        aria-controls={`${uid}-${v}`}
        tabindex={view === v ? 0 : -1}
        onclick={() => (view = v)}
        onkeydown={(e) => onTabKey(e, i, VIEWS.length, viewTabs, select)}>{label}</button
      >
    {/each}
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <!-- список — и вид печати: при «Графе» скрыт только на экране, на бумаге — он, а не граф -->
  <div role="tabpanel" id={`${uid}-list`} aria-labelledby={`${uid}-tab-list`} class:off={view !== 'list'}>
    {#if loading && !outgoing.length && !incoming.length}<p class="muted">Загрузка…</p>{/if}
    <section aria-label="Исходящие связи">
      {@render list('Исходящие', outgoing, 'Исходящих связей нет.')}
    </section>
    <section aria-label="Обратные ссылки">
      {@render list('Обратные ссылки', incoming, 'Сюда никто не ссылается.')}
    </section>
  </div>
  <div role="tabpanel" id={`${uid}-graph`} aria-labelledby={`${uid}-tab-graph`} class="graph-view" hidden={view !== 'graph'}>
    {#if view === 'graph'}
      {#await import('./GraphView.svelte')}
        <p class="muted">Загрузка графа…</p>
      {:then { default: GraphView }}
        <GraphView {user} {kind} {id} {hrefOf} onopen={open} {onexpired} />
      {:catch e}
        <!-- чанк графа не пришёл: сервер обновился (у чанков новые имена) или нет сети (§5.1 п. 9) -->
        <LazyFail error={e} label="Граф не загрузился" />
      {/await}
    {/if}
  </div>
</div>

<style>
  .views {
    display: flex;
    gap: 4px;
    margin: 4px 0 0;
  }
  .views button {
    padding: 4px 14px;
    background: transparent;
    color: var(--muted);
  }
  .views button[aria-selected='true'] {
    background: var(--selected);
    border-color: var(--accent);
    color: var(--text);
  }
  @media (pointer: coarse) {
    .views {
      gap: var(--touch-gap);
    }
  }
  h4 {
    margin: 12px 0 4px;
    font-size: 14px;
  }
  .links-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .links-list li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .type {
    font: 12px/18px ui-monospace, Consolas, monospace;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--chip);
    color: var(--muted);
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
  }
  .unresolved {
    color: var(--danger);
    text-decoration: line-through dotted;
  }
  .small {
    font-size: 12px;
  }
  /* печать — список при любом виде на экране; переключатель и граф — нет */
  @media screen {
    .off {
      display: none;
    }
  }
  @media print {
    .views,
    .graph-view {
      display: none !important;
    }
  }
</style>
