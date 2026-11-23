<script lang="ts">
  // Releases решения (§7.4): список релизов и сравнение выбранного с предыдущим или с любым
  // другим — записи changelog интервала по видам, дифф текста релиза, коммиты. Правка и корзина
  // (M7.2, спецификация §5.9): у выбранного релиза — «Изменить» (`EntryEdit`: заголовок, текст,
  // теги, область; версия и коммиты — как есть) и «Удалить» (в корзину, с подтверждением);
  // «Корзина» — удалённые релизы с «Восстановить» (`JournalTrash`). Форма и корзина — ленивые чанки.
  import { untrack } from 'svelte';
  import DiffView from './DiffView.svelte';
  import Lazy from './Lazy.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import {
    ApiError,
    compareRelease,
    errorText,
    listEntries,
    type Entry,
    type ReleaseCompare,
  } from '../lib/api';
  import { trashAsk } from '../lib/entryTrash';
  import { CHANGELOG_KINDS, dataStr, fmtTime } from '../lib/journals';
  import { go } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    /** Выбранный релиз (id); `null` — последний. */
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const PAGE = 50;

  let releases = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  /** `''` — с предыдущим, иначе id релиза. */
  let withId = $state('');
  let cmp = $state<ReleaseCompare | null>(null);
  let cmpError = $state('');
  let cmpLoading = $state(false);
  let seq = 0;
  let cseq = 0;
  /** «Корзина»: вместо списка и сравнения — удалённые релизы. */
  let trash = $state(false);
  /** Форма правки выбранного релиза открыта. */
  let editing = $state(false);
  let actionError = $state('');
  let notice = $state('');
  let ask = $state<Ask | null>(null);

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      trash = false;
      editing = false;
      notice = '';
      load(u, s, true);
    });
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, { kind: 'release', cursor: reset ? null : next, limit: PAGE });
      if (my !== seq) return;
      releases = reset ? r.items : [...releases, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** Выбранный релиз: из адреса, иначе последний. */
  let selected = $derived(entry ?? releases[0]?.id ?? null);

  $effect(() => {
    const id = selected;
    const w = withId;
    const u = user;
    untrack(() => compare(u, id, w));
  });

  async function compare(u: string, id: string | null, w: string) {
    const my = ++cseq;
    cmp = null;
    cmpError = '';
    editing = false;
    actionError = '';
    if (!id) return;
    cmpLoading = true;
    try {
      const r = await compareRelease(u, id, w && w !== id ? w : undefined);
      if (my === cseq) cmp = r;
    } catch (e) {
      if (my === cseq) fail(e, (m) => (cmpError = m));
    } finally {
      if (my === cseq) cmpLoading = false;
    }
  }

  function pick(id: string) {
    withId = '';
    notice = '';
    go({ name: 'journal', user, space, journal: 'releases', entry: id }, { replace: true });
  }

  /** Релиз сохранён или перечитан (409): список и сравнение — с новой ревизией. */
  async function refresh(done: string) {
    editing = false;
    await load(user, space, true);
    await compare(user, selected, withId);
    notice = done;
  }

  /** 409 правки или удаления: сравнение перечитано, ошибка — под заголовком. */
  async function stale(message: string) {
    await compare(user, selected, withId);
    actionError = message;
  }

  function askDelete(e: Entry) {
    actionError = '';
    notice = '';
    ask = trashAsk(user, e, {
      deleted: async (msg) => {
        editing = false;
        // выбранный ушёл в корзину: адрес без записи — последний релиз
        go({ name: 'journal', user, space, journal: 'releases', entry: null }, { replace: true });
        await load(user, space, true);
        notice = msg;
      },
      stale,
      expired: onexpired,
    });
  }

  function toggleTrash() {
    trash = !trash;
    editing = false;
    notice = '';
    // назад к списку: восстановленные из корзины — в нём
    if (!trash) load(user, space, true);
  }

  /** Релиз восстановлен из корзины: список перечитается при возврате к нему (`toggleTrash`). */
  const onrestored = () => {};

  /** 409 формы правки: релиз перечитан — заголовок и текст сравнения с новой ревизией, форма цела. */
  function reread(e: Entry) {
    if (cmp?.to.id === e.id) cmp = { ...cmp, to: e };
    releases = releases.map((r) => (r.id === e.id ? e : r));
  }

  const version = (e: Entry) => dataStr(e.data, 'version') || e.title;
  let groups = $derived.by(() => {
    const c = cmp?.changelog ?? {};
    const known = CHANGELOG_KINDS.filter((k) => c[k]?.length);
    const rest = Object.keys(c).filter((k) => !CHANGELOG_KINDS.includes(k) && c[k]?.length);
    return [...known, ...rest].map((k) => ({ kind: k, items: c[k] }));
  });
</script>

<section class="journal releases">
  <div class="row bar">
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  {#if notice}<p class="muted" role="status">{notice}</p>{/if}
  {#if trash}
    <Lazy what="Корзина" load={() => import('./JournalTrash.svelte')} props={{ user, space, kind: 'release', onrestored, onexpired }} />
  {:else}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="layout">
      <ul class="list" aria-label="Релизы">
        {#each releases as r (r.id)}
          <li>
            <button
              type="button"
              class="rel"
              aria-current={r.id === selected ? 'true' : undefined}
              onclick={() => pick(r.id)}
            >
              <span class="ver">{version(r)}</span>
              <span class="muted small">{dataStr(r.data, 'date') || fmtTime(r.created_at)}</span>
            </button>
          </li>
        {/each}
        {#if !loading && releases.length === 0 && !error}
          <li class="muted">Релизов пока нет.</li>
        {/if}
        {#if loading}<li class="muted">Загрузка…</li>{/if}
        {#if next && !loading}
          <li><button type="button" onclick={() => load(user, space, false)}>Ещё</button></li>
        {/if}
      </ul>

      <div class="compare">
        {#if cmpError}<p class="error" role="alert">{cmpError}</p>{/if}
        {#if cmpLoading && !cmp}<p class="muted">Загрузка…</p>{/if}
        {#if cmp}
          <header>
            <h2>Релиз {version(cmp.to)}</h2>
            <!-- заголовок записи, если он не «Релиз <версия>» (правка заголовка видна) -->
            {#if cmp.to.title !== `Релиз ${version(cmp.to)}`}<p class="rel-title">{cmp.to.title}</p>{/if}
            <div class="muted small">
              {dataStr(cmp.to.data, 'date')}
              {#if dataStr(cmp.to.data, 'git_tag')}· тег <code>{dataStr(cmp.to.data, 'git_tag')}</code>{/if}
            </div>
            <label class="row with">
              Сравнить с
              <select bind:value={withId} aria-label="Сравнить с">
                <option value="">предыдущим релизом</option>
                {#each releases.filter((r) => r.id !== cmp?.to.id) as r (r.id)}
                  <option value={r.id}>{version(r)}</option>
                {/each}
              </select>
            </label>
            <p class="muted small">
              Интервал: {cmp.from ? version(cmp.from) : 'начало журнала'} → {version(cmp.to)}
            </p>
            {#if !editing}
              {@const to = cmp.to}
              <div class="row actions">
                <button type="button" class="small-btn" onclick={() => ((editing = true), (actionError = ''), (notice = ''))}>Изменить</button>
                <button type="button" class="small-btn danger" onclick={() => askDelete(to)}>Удалить</button>
              </div>
            {/if}
            {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
          </header>
          {#if editing}
            <Lazy
              what="Форма"
              load={() => import('./EntryEdit.svelte')}
              props={{
                user,
                entry: cmp.to,
                onsaved: (e) => refresh(`Релиз «${e.title}» сохранён.`),
                oncancel: () => (editing = false),
                onreread: reread,
                onexpired,
              }}
              oncancel={() => (editing = false)}
            />
          {/if}
          {#if cmp.to.body}<p class="body">{cmp.to.body}</p>{/if}

          <h3>Изменения</h3>
          {#if groups.length === 0}<p class="muted">Записей changelog в интервале нет.</p>{/if}
          {#each groups as g (g.kind)}
            <div class="group" data-group={g.kind}>
              <h4><span class="kind">{g.kind}</span> <span class="muted">{g.items.length}</span></h4>
              <ul>
                {#each g.items as e (e.id)}
                  <li>{e.title}{#if e.area} <span class="chip">{e.area}</span>{/if}</li>
                {/each}
              </ul>
            </div>
          {/each}
          {#if cmp.truncated}<p class="warn">Показаны не все записи интервала: их слишком много.</p>{/if}

          <h3>Текст релиза</h3>
          {#if cmp.notes_diff.trim()}
            <DiffView diff={cmp.notes_diff} />
          {:else}
            <p class="muted">Текст релиза не изменился.</p>
          {/if}

          <h3>Коммиты</h3>
          {#if cmp.commits.length === 0}<p class="muted">Коммиты не указаны.</p>{/if}
          <ul class="commits">
            {#each cmp.commits as c, i (i)}
              <li><code>{c.sha.slice(0, 10)}</code> {c.subject ?? ''}</li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>
  {/if}
</section>

<Prompt bind:ask />

<style>
  .layout {
    display: grid;
    grid-template-columns: 200px minmax(0, 1fr);
    gap: 20px;
  }
  @media (max-width: 719px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    align-content: start;
  }
  .rel {
    width: 100%;
    display: flex;
    justify-content: space-between;
    gap: 8px;
    border: 0;
    background: transparent;
    text-align: left;
    padding: 6px 8px;
  }
  .rel:hover {
    background: var(--hover);
  }
  .rel[aria-current='true'] {
    background: var(--selected);
  }
  .ver {
    font-weight: 600;
  }
  .with {
    margin-top: 8px;
    gap: 6px;
  }
  h3 {
    margin: 18px 0 6px;
    font-size: 15px;
  }
  h4 {
    margin: 8px 0 2px;
    font-size: 14px;
  }
  .group ul,
  .commits {
    margin: 0;
    padding-left: 20px;
  }
  .kind {
    font: 12px/18px ui-monospace, Consolas, monospace;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--chip);
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 12px;
  }
  .bar {
    margin-bottom: 8px;
  }
  .rel-title {
    margin: 0 0 4px;
    font-weight: 600;
    overflow-wrap: anywhere;
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
</style>
