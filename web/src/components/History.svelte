<script lang="ts">
  // История ревизий заметки: список (новые сверху), дифф двух выбранных ревизий,
  // откат к выбранной (новая ревизия с её текстом, с подтверждением).
  import { untrack } from 'svelte';
  import DiffView from './DiffView.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import { ApiError, getDiff, listRevisions, revertNode, type NodeInfo, type RevisionInfo } from '../lib/api';

  interface Props {
    user: string;
    node: string;
    /** Текущая ревизия заметки: смена — перечитать список. */
    rev: number;
    onreverted: (n: NodeInfo) => void;
    onexpired: () => void;
  }

  let { user, node, rev, onreverted, onexpired }: Props = $props();

  let revs = $state<RevisionInfo[]>([]);
  let error = $state('');
  let loading = $state(false);
  /** Выбор для диффа: A — старая сторона, B — новая (порядок выравнивается при запросе). */
  let pickA = $state<number | null>(null);
  let pickB = $state<number | null>(null);
  let diff = $state<string | null>(null);
  let diffError = $state('');
  let ask = $state<Ask | null>(null);

  let seq = 0;
  let diffSeq = 0;

  const AUTHOR: Record<string, string> = { human: 'человек', agent: 'агент', system: 'система' };

  $effect(() => {
    const u = user;
    const n = node;
    void rev;
    untrack(() => load(u, n));
  });

  async function load(u: string, n: string) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const list = await listRevisions(u, n);
      if (my !== seq) return;
      revs = list;
      // по умолчанию — последняя правка: предыдущая против текущей
      pickB = list[0]?.rev ?? null;
      pickA = list[1]?.rev ?? null;
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = e instanceof Error ? e.message : 'Ошибка';
    } finally {
      if (my === seq) loading = false;
    }
  }

  $effect(() => {
    const a = pickA;
    const b = pickB;
    untrack(() => loadDiff(a, b));
  });

  async function loadDiff(a: number | null, b: number | null) {
    const my = ++diffSeq;
    diffError = '';
    if (a === null || b === null || a === b) {
      diff = null;
      return;
    }
    try {
      const d = await getDiff(user, node, Math.min(a, b), Math.max(a, b));
      if (my === diffSeq) diff = d;
    } catch (e) {
      if (my !== diffSeq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      diff = null;
      diffError = e instanceof Error ? e.message : 'Ошибка';
    }
  }

  function revert(r: RevisionInfo) {
    ask = {
      title: `Откатить к ревизии ${r.rev}?`,
      message: `Будет создана новая ревизия с текстом ревизии ${r.rev}. Прежние ревизии останутся в истории.`,
      confirm: 'Откатить',
      danger: true,
      run: async () => {
        try {
          onreverted(await revertNode(user, node, r.rev));
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onexpired();
          throw e;
        }
      },
    };
  }

  const fmtTime = (ts: string) => {
    const d = new Date(ts);
    return Number.isNaN(d.getTime()) ? ts : d.toLocaleString('ru-RU', { dateStyle: 'medium', timeStyle: 'medium' });
  };
  const short = (s: string) => (s.length > 10 ? `${s.slice(0, 8)}…` : s);
  let lo = $derived(pickA !== null && pickB !== null ? Math.min(pickA, pickB) : null);
  let hi = $derived(pickA !== null && pickB !== null ? Math.max(pickA, pickB) : null);
</script>

<section class="history">
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading && !revs.length}<p class="muted">Загрузка…</p>{/if}
  {#if revs.length}
    <div class="table-wrap">
      <table aria-label="Ревизии">
        <thead>
          <tr>
            <th title="Сторона «было» диффа">А</th>
            <th title="Сторона «стало» диффа">Б</th>
            <th>Ревизия</th>
            <th>Автор</th>
            <th>Машина</th>
            <th>Сессия</th>
            <th>Время</th>
            <th>Сообщение</th>
            <th><span class="sr">Действия</span></th>
          </tr>
        </thead>
        <tbody>
          {#each revs as r (r.rev)}
            <tr class:current={r.rev === revs[0].rev}>
              <td><input type="radio" name="rev-a" value={r.rev} bind:group={pickA} aria-label={`А: ревизия ${r.rev}`} /></td>
              <td><input type="radio" name="rev-b" value={r.rev} bind:group={pickB} aria-label={`Б: ревизия ${r.rev}`} /></td>
              <td class="num">{r.rev}{#if r.rev === revs[0].rev}<span class="muted"> · текущая</span>{/if}</td>
              <td>{AUTHOR[r.author_kind] ?? r.author_kind}</td>
              <td>{r.machine ?? '—'}</td>
              <td title={r.session_id ?? undefined}><code>{r.session_id ? short(r.session_id) : '—'}</code></td>
              <td class="nowrap">{fmtTime(r.ts)}</td>
              <td class="msg">{r.message ?? ''}</td>
              <td>
                {#if r.rev !== revs[0].rev}
                  <button type="button" class="small" onclick={() => revert(r)}>Откатить к этой</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <h2 class="diff-title">
      {#if lo !== null && hi !== null && lo !== hi}Изменения: ревизия {lo} → {hi}{:else}Выберите две разные ревизии (А и Б){/if}
    </h2>
    {#if diffError}<p class="error" role="alert">{diffError}</p>{/if}
    {#if diff !== null}<DiffView {diff} />{/if}
  {:else if !loading && !error}
    <p class="muted">Ревизий нет.</p>
  {/if}
</section>

<Prompt bind:ask />

<style>
  .history {
    padding: 8px 0 24px;
  }
  .table-wrap {
    overflow-x: auto;
  }
  table {
    font-size: 14px;
  }
  th {
    font-weight: 600;
    color: var(--muted);
    white-space: nowrap;
  }
  tr.current {
    background: var(--selected);
  }
  .num,
  .nowrap {
    white-space: nowrap;
  }
  .msg {
    min-width: 120px;
  }
  button.small {
    padding: 2px 8px;
    font-size: 13px;
    white-space: nowrap;
  }
  .diff-title {
    margin-top: 20px;
    font-size: 15px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
