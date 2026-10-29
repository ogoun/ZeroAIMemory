<script lang="ts">
  // «Задачи» (M4.5 §3): доска по статусам (открыта, в работе, заблокирована, выполнена, снята),
  // у карточки — следующий шаг, блокер, захват («в работе у <агент>, до <время>»), у выполненных —
  // длительность. Человек меняет статус и поля (PATCH с If-Match; переоткрытие из done/dropped —
  // с причиной) и снимает захват агента. Захватывает задачу агент: захват привязан к сессии
  // агента (`X-Zam-Session`), у WebUI сессии агента нет.
  import { untrack } from 'svelte';
  import FileRefs from './FileRefs.svelte';
  import {
    ApiError,
    createEntry,
    errorText,
    getEntry,
    isStaleEntry,
    listEntries,
    patchEntry,
    releaseTask,
    type Entry,
    type EntryStatus,
  } from '../lib/api';
  import { loadRefs, staleOf, type RefsByTarget } from '../lib/fileRefs';
  import { AUTHOR_TEXT, STATUS_TEXT, TASK_COLUMNS, entryRef, fmtTime } from '../lib/journals';
  import { go } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const PAGE = 200;

  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let refs = $state<RefsByTarget>(new Map());
  let seq = 0;

  let cur = $state<Entry | null>(null);
  let curError = $state('');
  let eseq = 0;
  type Mode = 'none' | 'create' | 'status';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');

  let nStep = $state('');
  let nTitle = $state('');
  let nBody = $state('');
  let toStatus = $state<EntryStatus>('open');
  let sReason = $state('');
  let sBlocked = $state('');
  let stepDraft = $state('');

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      load(u, s, true);
      loadRefs(u, s).then((r) => {
        if (u === user && s === space) refs = r;
      });
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, { kind: 'task', cursor: reset ? null : next, limit: PAGE });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) items = [];
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    curError = '';
    if (!id) {
      cur = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'task') {
        cur = null;
        curError = 'Это не задача.';
        return;
      }
      cur = e;
      stepDraft = str(e.data, 'next_step');
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  const str = (d: Record<string, unknown>, k: string) => (typeof d[k] === 'string' ? (d[k] as string) : '');

  /** Действующий захват: `{agent, until}` или `null` (срок истёк — захвата нет). */
  function claimOf(e: Entry): { agent: string; until: string } | null {
    const c = e.data.claimed_by;
    if (!c || typeof c !== 'object') return null;
    const o = c as Record<string, unknown>;
    const until = typeof o.until === 'string' ? o.until : '';
    if (!until || new Date(until).getTime() <= Date.now()) return null;
    return { agent: typeof o.agent === 'string' && o.agent ? o.agent : 'агент', until };
  }

  const claimText = (c: { agent: string; until: string }) => `в работе у ${c.agent}, до ${fmtTime(c.until)}`;

  const durationText = (m: number | null | undefined) => {
    if (m === null || m === undefined) return '';
    if (m < 60) return `${m} мин`;
    const h = Math.floor(m / 60);
    return m % 60 ? `${h} ч ${m % 60} мин` : `${h} ч`;
  };

  let columns = $derived(TASK_COLUMNS.map((s) => ({ status: s, tasks: items.filter((t) => t.status === s) })));

  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'tasks', entry: id }, { replace: true });
  }

  async function act(fn: () => Promise<string | void>, done: string) {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      mode = 'none';
      await load(user, space, true);
      if (target && target !== entry) select(target);
      else await open(user, target);
      notice = done;
    } catch (e) {
      if (isStaleEntry(e) && cur) {
        const msg = errorText(e);
        await open(user, cur.id);
        actionError = msg;
      } else {
        fail(e, (m) => (actionError = m));
      }
    } finally {
      busy = false;
    }
  }

  /** Статусы, в которые можно перейти из текущего (из done/dropped — только в open, с причиной). */
  const targets = (e: Entry): EntryStatus[] =>
    e.status === 'done' || e.status === 'dropped'
      ? ['open']
      : TASK_COLUMNS.filter((s) => s !== e.status);

  function startStatus(s: EntryStatus) {
    toStatus = s;
    sReason = '';
    sBlocked = cur ? str(cur.data, 'blocked_by') : '';
    actionError = '';
    mode = 'status';
  }

  function submitStatus(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    const reopen = c.status === 'done' || c.status === 'dropped';
    if (reopen && !sReason.trim()) {
      actionError = 'Переоткрытие требует причины.';
      return;
    }
    act(async () => {
      await patchEntry(user, c.id, c.rev, {
        status: toStatus,
        ...(reopen ? { reason: sReason.trim() } : {}),
        ...(toStatus === 'blocked' && sBlocked.trim() ? { blocked_by: sBlocked.trim() } : {}),
      });
    }, `Статус: ${STATUS_TEXT[toStatus]}.`);
  }

  function saveStep(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    act(async () => {
      await patchEntry(user, c.id, c.rev, { next_step: stepDraft.trim() });
    }, 'Следующий шаг сохранён.');
  }

  function release() {
    const c = cur;
    if (!c) return;
    act(async () => {
      await releaseTask(user, c.id);
    }, 'Захват снят.');
  }

  function startCreate() {
    nTitle = '';
    nBody = '';
    nStep = '';
    actionError = '';
    mode = 'create';
  }

  function submitCreate(e: SubmitEvent) {
    e.preventDefault();
    const title = nTitle.trim();
    if (!title) {
      actionError = 'Укажите заголовок.';
      return;
    }
    act(
      async () =>
        (
          await createEntry(user, space, {
            kind: 'task',
            title,
            body: nBody,
            data: nStep.trim() ? { next_step: nStep.trim() } : {},
          })
        ).id,
      'Задача добавлена.',
    );
  }
</script>

<section class="journal tasks">
  <div class="row bar">
    <button type="button" class="primary" onclick={startCreate}>Новая задача</button>
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if !loading && items.length === 0 && !error}<p class="muted">Задач нет.</p>{/if}

  <div class="board" role="group" aria-label="Доска задач">
    {#each columns as col (col.status)}
      <section class="col" aria-label="Задачи: {STATUS_TEXT[col.status]}" data-status={col.status}>
        <h2>{STATUS_TEXT[col.status]} <span class="muted small">{col.tasks.length}</span></h2>
        {#each col.tasks as t (t.id)}
          {@const claim = claimOf(t)}
          {@const stale = staleOf(refs.get(entryRef('task', t.id)))}
          <article class="task" class:selected={t.id === entry} data-task={t.id}>
            <button type="button" class="link title" onclick={() => select(t.id)}>{t.title}</button>
            {#if str(t.data, 'next_step')}<div class="small">Дальше: {str(t.data, 'next_step')}</div>{/if}
            {#if t.status === 'blocked' && str(t.data, 'blocked_by')}
              <div class="small blocked">Блокер: {str(t.data, 'blocked_by')}</div>
            {/if}
            {#if claim}<div class="claim small">{claimText(claim)}</div>{/if}
            {#if t.status === 'done' && t.duration_min !== undefined && t.duration_min !== null}
              <div class="muted small">длительность {durationText(t.duration_min)}</div>
            {/if}
            {#if stale.length}<div class="badge small">возможно устарело</div>{/if}
          </article>
        {/each}
      </section>
    {/each}
  </div>
  {#if next && !loading}
    <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
  {/if}

  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const claim = claimOf(c)}
    <article class="card panel" aria-label="Карточка задачи">
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status">{c.status ? STATUS_TEXT[c.status] : ''}</span>
          <span class="muted small">{AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>
      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if claim}
        <p class="claim" data-testid="claim">{claimText(claim)}</p>
      {/if}
      {#if c.status === 'blocked' && str(c.data, 'blocked_by')}<p class="blocked">Блокер: {str(c.data, 'blocked_by')}</p>{/if}
      {#if c.status === 'done'}
        <p class="muted">
          Выполнено {fmtTime(str(c.data, 'done_at'))}{#if c.duration_min !== undefined && c.duration_min !== null}, длительность {durationText(c.duration_min)}{/if}
        </p>
      {/if}
      <FileRefs refs={refs.get(entryRef('task', c.id)) ?? []} />

      {#if c.status !== 'done' && c.status !== 'dropped'}
        <form class="row" onsubmit={saveStep}>
          <input bind:value={stepDraft} placeholder="следующий шаг" aria-label="Следующий шаг" maxlength="2000" />
          <button type="submit" disabled={busy}>Сохранить шаг</button>
        </form>
      {:else if str(c.data, 'next_step')}
        <p class="muted">Дальше было: {str(c.data, 'next_step')}</p>
      {/if}

      <div class="row actions" role="group" aria-label="Смена статуса">
        {#each targets(c) as s (s)}
          <button type="button" onclick={() => startStatus(s)} disabled={busy}>
            {c.status === 'done' || c.status === 'dropped' ? 'Переоткрыть' : `→ ${STATUS_TEXT[s]}`}
          </button>
        {/each}
        {#if claim}<button type="button" class="danger" onclick={release} disabled={busy}>Снять захват</button>{/if}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}

      {#if mode === 'status'}
        <form class="form" onsubmit={submitStatus} aria-label="Смена статуса задачи">
          <h3>Статус: {STATUS_TEXT[toStatus]}</h3>
          {#if c.status === 'done' || c.status === 'dropped'}
            <label>Причина переоткрытия <textarea bind:value={sReason} rows="2" required></textarea></label>
          {/if}
          {#if toStatus === 'blocked'}
            <label>Что мешает <input bind:value={sBlocked} maxlength="2000" /></label>
          {/if}
          {@render formEnd('Применить')}
        </form>
      {:else if actionError}
        <p class="error" role="alert">{actionError}</p>
      {/if}
    </article>
  {/if}

  {#if mode === 'create'}
    <form class="form panel" onsubmit={submitCreate} aria-label="Новая задача">
      <h3>Новая задача</h3>
      <label>Заголовок <input bind:value={nTitle} required maxlength="300" /></label>
      <label>Описание <textarea bind:value={nBody} rows="3"></textarea></label>
      <label>Следующий шаг <input bind:value={nStep} maxlength="2000" /></label>
      {@render formEnd('Добавить')}
    </form>
  {/if}
</section>

{#snippet formEnd(label: string)}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>{label}</button>
    <button type="button" onclick={() => ((mode = 'none'), (actionError = ''))}>Отмена</button>
  </div>
{/snippet}

<style>
  .bar {
    margin-bottom: 8px;
  }
  .board {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 10px;
    align-items: start;
  }
  .col {
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .col h2 {
    margin: 0 0 8px;
    font-size: 14px;
  }
  .task {
    display: grid;
    gap: 2px;
    margin-bottom: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .task.selected {
    background: var(--selected);
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
  }
  .title {
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .claim {
    color: var(--callout-tip);
  }
  .blocked {
    color: var(--danger);
  }
  .badge {
    justify-self: start;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .card {
    margin-top: 16px;
  }
  .card h2 {
    margin: 0 0 4px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .meta {
    gap: 6px;
  }
  .actions {
    margin-top: 12px;
  }
  .form {
    display: grid;
    gap: 8px;
    margin-top: 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .form h3 {
    margin: 0;
    font-size: 15px;
  }
  .form label {
    display: grid;
    gap: 4px;
  }
  .form textarea {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 12px;
  }
  input[aria-label='Следующий шаг'] {
    flex: 1;
    min-width: 200px;
  }
</style>
