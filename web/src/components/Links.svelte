<script lang="ts">
  // Вкладка «Связи» (§8.5): исходящие связи узла или записи (с неразрешёнными ссылками) и
  // обратные ссылки — списком; переход к заметке или записи журнала.
  import { untrack } from 'svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import {
    ApiError,
    entryBacklinks,
    entryGraph,
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
  import { go } from '../lib/router';

  interface Props {
    user: string;
    kind: 'node' | 'entry';
    id: string;
    onexpired: () => void;
  }

  let { user, kind, id, onexpired }: Props = $props();

  type Item = { edge: EdgeView; other: GraphNode | null; ref: string | null };

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
      const self = `${k === 'node' ? 'n' : 'e'}:${i}`;
      const [out, back, sp] = await Promise.all([
        k === 'node'
          ? nodeLinks(u, i)
          : entryGraph(u, i).then((g) => ({ items: g.edges.filter((e) => e.src === self), nodes: g.nodes })),
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

  async function openItem(it: Item) {
    const ref = it.ref;
    const n = it.other;
    if (!ref || !n) return;
    const s = spaces.find((x) => x.id === n.space_id);
    if (!s) {
      error = 'Пространство цели недоступно.';
      return;
    }
    const space = spaceRef(s);
    const [k, target] = [ref.slice(0, 1), ref.slice(2)];
    if (k === 'n') {
      go({ name: 'node', user, space, node: target, anchor: null });
      return;
    }
    if (n.status) {
      go({ name: 'journal', user, space, journal: 'insights', entry: target });
      return;
    }
    try {
      const e = await getEntry(user, target);
      go({
        name: 'journal',
        user,
        space,
        journal: e.kind === 'release' ? 'releases' : e.kind === 'insight' ? 'insights' : 'changelog',
        entry: target,
      });
    } catch (e) {
      error = errorText(e);
    }
  }
</script>

{#snippet list(title: string, rows: Item[], empty: string)}
  <h4>{title} <span class="muted">{rows.length}</span></h4>
  {#if rows.length === 0}<p class="muted">{empty}</p>{/if}
  <ul class="links-list">
    {#each rows as it (it.edge.id)}
      <li>
        <span class="type" title="Тип связи">{it.edge.type}</span>
        {#if it.other}
          <button type="button" class="link" onclick={() => openItem(it)} disabled={it.other.deleted}>
            {it.other.path ?? it.other.title}
          </button>
          <CertaintyBadge value={it.other.certainty} />
          {#if it.other.deleted}<span class="muted small">удалено</span>{/if}
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
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading && !outgoing.length && !incoming.length}<p class="muted">Загрузка…</p>{/if}
  <section aria-label="Исходящие связи">
    {@render list('Исходящие', outgoing, 'Исходящих связей нет.')}
  </section>
  <section aria-label="Обратные ссылки">
    {@render list('Обратные ссылки', incoming, 'Сюда никто не ссылается.')}
  </section>
</div>

<style>
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
</style>
