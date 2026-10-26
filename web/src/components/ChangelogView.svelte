<script lang="ts">
  // Changelog решения (§7.3): лента записей (новые — первыми) с курсором, фильтры по виду
  // (`data_kind`) и области (`area`) — на сервере, курсор их учитывает.
  import { untrack } from 'svelte';
  import { ApiError, errorText, getEntry, listEntries, type Entry } from '../lib/api';
  import { AUTHOR_TEXT, CHANGELOG_KINDS, dataStr, fmtTime } from '../lib/journals';

  interface Props {
    user: string;
    space: string;
    /** Открытая запись (из поиска или связей). */
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const PAGE = 50;

  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let kind = $state('');
  let area = $state('');
  /** Область, по которой загружена лента. */
  let appliedArea = $state('');
  let loading = $state(false);
  let error = $state('');
  let focused = $state<Entry | null>(null);
  let seq = 0;

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      kind = '';
      area = '';
      appliedArea = '';
      load(u, s, true);
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => focus(u, id));
  });

  async function focus(u: string, id: string | null) {
    focused = null;
    if (!id) return;
    try {
      const e = await getEntry(u, id);
      if (e.kind === 'changelog') focused = e;
    } catch (e) {
      fail(e);
    }
  }

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = errorText(e);
  }

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, {
        kind: 'changelog',
        area: appliedArea || undefined,
        dataKind: kind || undefined,
        cursor: reset ? null : next,
        limit: PAGE,
      });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) items = [];
      fail(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  function applyArea(e: SubmitEvent) {
    e.preventDefault();
    appliedArea = area.trim();
    load(user, space, true);
  }


  const refsOf = (e: Entry): [string, string[]][] => {
    const r = e.data.refs;
    if (!r || typeof r !== 'object') return [];
    return Object.entries(r as Record<string, unknown>)
      .filter((x): x is [string, string[]] => Array.isArray(x[1]) && x[1].length > 0)
      .map(([k, v]) => [k, v.filter((s) => typeof s === 'string')]);
  };
  const REF_TITLE: Record<string, string> = {
    commits: 'Коммиты',
    files: 'Файлы',
    notes: 'Заметки',
    issues: 'Задачи',
  };
</script>

{#snippet card(e: Entry, hl: boolean)}
  <article class="entry" class:focus={hl} data-entry={e.id}>
    <header>
      <span class="kind k-{dataStr(e.data, 'kind') || 'other'}">{dataStr(e.data, 'kind') || 'other'}</span>
      <h3>{e.title}</h3>
    </header>
    <div class="meta muted">
      {fmtTime(e.created_at)} · {AUTHOR_TEXT[e.author.kind] ?? e.author.kind}
      {#if e.area}· область <span class="chip">{e.area}</span>{/if}
      {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
    </div>
    {#if e.body}<p class="body">{e.body}</p>{/if}
    {#each refsOf(e) as [k, v] (k)}
      <div class="refs"><span class="muted">{REF_TITLE[k] ?? k}:</span>
        {#each v as x, i (i)}<code>{x}</code>{/each}
      </div>
    {/each}
  </article>
{/snippet}

<section class="journal">
  <form class="filters row" onsubmit={applyArea}>
    <label class="row">
      Вид
      <select bind:value={kind} aria-label="Вид записи" onchange={() => load(user, space, true)}>
        <option value="">все</option>
        {#each CHANGELOG_KINDS as k (k)}<option value={k}>{k}</option>{/each}
      </select>
    </label>
    <label class="row">
      Область
      <input bind:value={area} placeholder="например, web" aria-label="Область" />
    </label>
    <button type="submit">Применить</button>
  </form>

  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if focused}
    <div class="focused">
      <p class="muted small">Открытая запись</p>
      {@render card(focused, true)}
    </div>
  {/if}

  <div class="feed" aria-label="Лента changelog" role="feed" aria-busy={loading}>
    {#each items as e (e.id)}
      {@render card(e, false)}
    {/each}
  </div>
  {#if !loading && items.length === 0 && !error}
    <p class="muted">Записей нет{kind || appliedArea ? ' по выбранным фильтрам' : ''}.</p>
  {/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if next && !loading}
    <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
  {/if}
</section>

<style>
  .filters {
    margin-bottom: 16px;
  }
  .filters label {
    gap: 6px;
  }
  .feed {
    display: grid;
    gap: 10px;
  }
  .entry {
    padding: 10px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .entry.focus {
    border-color: var(--accent);
  }
  .entry header {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .entry h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .kind {
    flex: none;
    font: 12px/18px ui-monospace, Consolas, monospace;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--chip);
  }
  .k-feat {
    color: var(--callout-tip);
  }
  .k-fix {
    color: var(--callout-caution);
  }
  .k-docs,
  .k-research {
    color: var(--callout-note);
  }
  .meta {
    font-size: 12px;
    margin-top: 2px;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .body {
    margin: 6px 0 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .refs {
    margin-top: 4px;
    font-size: 12px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .focused {
    margin-bottom: 16px;
  }
  .small {
    font-size: 12px;
    margin: 0 0 4px;
  }
</style>
