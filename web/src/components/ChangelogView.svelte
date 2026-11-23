<script lang="ts">
  // Changelog решения (§7.3): лента записей (новые — первыми) с курсором, фильтры по виду
  // (`data_kind`) и области (`area`) — на сервере, курсор их учитывает. Правка и корзина (M7.2,
  // спецификация §5.9): у записи — «Изменить» (`EntryEdit`: заголовок, текст, вид, теги, область;
  // 409 — запись перечитана) и «Удалить» (в корзину, с подтверждением); «Корзина» — удалённые
  // записи с «Восстановить» (`JournalTrash`). Форма и корзина — ленивые чанки.
  import { untrack } from 'svelte';
  import Lazy from './Lazy.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import { ApiError, errorText, getEntry, listEntries, type Entry } from '../lib/api';
  import { trashAsk } from '../lib/entryTrash';
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
  /** «Корзина»: вместо ленты — удалённые записи. */
  let trash = $state(false);
  /** Запись с открытой формой правки. */
  let editing = $state('');
  /** Ошибка и итог действия над записью (правка, удаление) — у её карточки. */
  let cardError = $state<Record<string, string>>({});
  let notice = $state('');
  let ask = $state<Ask | null>(null);

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      kind = '';
      area = '';
      appliedArea = '';
      trash = false;
      editing = '';
      notice = '';
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

  /** Запись `e` в ленте и в открытой — новой ревизией (после правки или перечитывания). */
  function replace(e: Entry) {
    items = items.map((x) => (x.id === e.id ? e : x));
    if (focused?.id === e.id) focused = e;
  }

  function startEdit(e: Entry) {
    editing = e.id;
    cardError[e.id] = '';
    notice = '';
  }

  function saved(e: Entry) {
    editing = '';
    replace(e);
    notice = `Запись «${e.title}» сохранена.`;
  }

  /** 409 правки или удаления: запись — новой ревизией, ошибка — у её карточки. */
  async function stale(id: string, message: string) {
    try {
      replace(await getEntry(user, id));
    } catch (e) {
      fail(e);
    }
    cardError[id] = message;
  }

  function askDelete(e: Entry) {
    cardError[e.id] = '';
    notice = '';
    ask = trashAsk(user, e, {
      deleted: (msg) => {
        if (editing === e.id) editing = '';
        items = items.filter((x) => x.id !== e.id);
        if (focused?.id === e.id) focused = null;
        notice = msg;
      },
      stale: (msg) => stale(e.id, msg),
      expired: onexpired,
    });
  }

  function toggleTrash() {
    trash = !trash;
    editing = '';
    notice = '';
    // назад к ленте: восстановленные из корзины — в ней
    if (!trash) load(user, space, true);
  }

  /** Запись восстановлена из корзины: лента перечитается при возврате к ней (`toggleTrash`). */
  const onrestored = () => {};

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
  <!-- открытая сверху и та же в ленте — одна форма правки: у открытой -->
  {@const edit = editing === e.id && (hl || focused?.id !== e.id)}
  <article class="entry" class:focus={hl} data-entry={e.id}>
    <header>
      <span class="kind k-{dataStr(e.data, 'kind') || 'other'}">{dataStr(e.data, 'kind') || 'other'}</span>
      <h3>{e.title}</h3>
    </header>
    <div class="meta muted">
      {fmtTime(e.created_at)} · {AUTHOR_TEXT[e.author.kind] ?? e.author.kind} · ревизия {e.rev}
      {#if e.area}· область <span class="chip">{e.area}</span>{/if}
      {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
    </div>
    {#if e.body}<p class="body">{e.body}</p>{/if}
    {#each refsOf(e) as [k, v] (k)}
      <div class="refs"><span class="muted">{REF_TITLE[k] ?? k}:</span>
        {#each v as x, i (i)}<code>{x}</code>{/each}
      </div>
    {/each}
    {#if edit}
      <Lazy
        what="Форма"
        load={() => import('./EntryEdit.svelte')}
        props={{ user, entry: e, onsaved: saved, oncancel: () => (editing = ''), onreread: replace, onexpired }}
        oncancel={() => (editing = '')}
      />
    {:else}
      <div class="row actions">
        <button type="button" class="small-btn" aria-label="Изменить «{e.title}»" onclick={() => startEdit(e)}>Изменить</button>
        <button type="button" class="small-btn danger" aria-label="Удалить «{e.title}»" onclick={() => askDelete(e)}>Удалить</button>
      </div>
    {/if}
    {#if cardError[e.id]}<p class="error" role="alert">{cardError[e.id]}</p>{/if}
  </article>
{/snippet}

<section class="journal">
  <form class="filters row" onsubmit={applyArea}>
    {#if !trash}
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
    {/if}
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </form>

  {#if notice}<p class="muted" role="status">{notice}</p>{/if}

  {#if trash}
    <Lazy what="Корзина" load={() => import('./JournalTrash.svelte')} props={{ user, space, kind: 'changelog', onrestored, onexpired }} />
  {:else}
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
  {/if}
</section>

<Prompt bind:ask />

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
    min-width: 0;
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    overflow-wrap: anywhere;
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
    overflow-wrap: anywhere;
  }
  .refs code {
    min-width: 0;
  }
  .focused {
    margin-bottom: 16px;
  }
  .toggle[aria-pressed='true'] {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--selected);
  }
  .actions {
    margin-top: 6px;
  }
  .small-btn {
    padding: 2px 8px;
    font-size: 12px;
  }
  .small {
    font-size: 12px;
    margin: 0 0 4px;
  }
</style>
