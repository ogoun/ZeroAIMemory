<script lang="ts">
  // «Администрирование → Аудит» (#/admin/audit; M7.2, спецификация M7 §5.6 п. 4; ленивый чанк):
  // лента `GET /admin/audit` по 100 событий, новые первыми, «Показать ещё» — по курсору `before`;
  // время — местное (`localTime`), действие, пользователь (имя по списку пользователей; без
  // пользователя — кто действовал: `admin`, `system`, `anonymous`…), цель, адрес, детали JSON —
  // свёрнуты. Фильтр по действию и пользователю — по загруженному: серверных фильтров нет, пока
  // аудит мал (решение координатора 25). На телефоне — карточками строк.
  import { onMount } from 'svelte';
  import { ApiError, errorText, listAudit, type AuditRow, type User } from '../lib/api';
  import { localTime } from '../lib/time';
  import { viewport } from '../lib/viewport.svelte';

  let { users, onexpired }: { users: User[]; onexpired: () => void } = $props();

  /** Значение фильтра «без пользователя». */
  const NO_USER = '-';

  let rows = $state<AuditRow[]>([]);
  let next = $state<number | null>(null);
  let loaded = $state(false);
  let loading = $state(false);
  let error = $state('');
  let action = $state('');
  let user = $state('');

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
      onexpired();
      return true;
    }
    return false;
  }

  async function load(more: boolean) {
    loading = true;
    error = '';
    try {
      const r = await listAudit(more && next !== null ? next : undefined);
      // курсор — id: повтор страницы (двойной щелчок) не дублирует строки
      const seen = new Set(more ? rows.map((x) => x.id) : []);
      rows = [...(more ? rows : []), ...r.items.filter((x) => !seen.has(x.id))];
      next = r.next;
      loaded = true;
    } catch (e) {
      if (expired(e)) return;
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void load(false);
  });

  const userName = (id: string | null) => (id ? (users.find((u) => u.id === id)?.name ?? id) : '');
  const time = (ts: string) =>
    localTime(ts, { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const hasDetails = (d: unknown) => d !== null && d !== undefined && !(typeof d === 'object' && Object.keys(d as object).length === 0);
  const json = (d: unknown) => JSON.stringify(d, null, 2);

  /** Действия загруженных событий — для фильтра. */
  let actions = $derived([...new Set(rows.map((r) => r.action))].sort());
  /** Пользователи загруженных событий (по имени); «без пользователя» — последним. */
  let people = $derived.by(() => {
    const ids = [...new Set(rows.flatMap((r) => (r.user_id ? [r.user_id] : [])))];
    return ids.map((id) => ({ id, name: userName(id) })).sort((a, b) => a.name.localeCompare(b.name));
  });
  let anonymous = $derived(rows.some((r) => !r.user_id));
  let shown = $derived(
    rows.filter(
      (r) =>
        (!action || r.action === action) &&
        (!user || (user === NO_USER ? !r.user_id : r.user_id === user)),
    ),
  );
  let phone = $derived(viewport.current === 'phone');
</script>

<section class="panel audit-page">
  <h2>Аудит</h2>
  <div class="row filters">
    <label>
      Действие
      <select bind:value={action}>
        <option value="">все</option>
        {#each actions as a (a)}<option value={a}>{a}</option>{/each}
      </select>
    </label>
    <label>
      Пользователь
      <select bind:value={user}>
        <option value="">все</option>
        {#each people as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        {#if anonymous}<option value={NO_USER}>без пользователя</option>{/if}
      </select>
    </label>
    <span class="muted small" role="status">
      {#if loaded}показано {shown.length} из {rows.length} загруженных{/if}
    </span>
  </div>
  <p class="muted small">Фильтры — по загруженным событиям: старые события подгружаются «Показать ещё».</p>
  {#if error}<p class="error" role="alert">Не удалось получить журнал аудита: {error}</p>{/if}

  {#if !loaded && !error}
    <p class="muted">Загрузка…</p>
  {:else if loaded && !rows.length}
    <p class="muted">Событий нет.</p>
  {:else if phone}
    <ul class="cards" aria-label="События аудита">
      {#each shown as r (r.id)}
        <li class="card" data-audit={r.action}>
          <div class="card-head"><b>{r.action}</b> <span class="muted small">{time(r.ts)}</span></div>
          <div class="small">
            {#if r.user_id}{userName(r.user_id)}{:else}<span class="muted">{r.actor}</span>{/if}
            {#if r.target} · <span class="target">{r.target}</span>{/if}
            {#if r.ip} · <span class="muted">{r.ip}</span>{/if}
          </div>
          {#if hasDetails(r.details)}
            <details><summary>Детали</summary><pre>{json(r.details)}</pre></details>
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <div class="table-wrap">
      <table>
        <thead><tr><th>Время</th><th>Действие</th><th>Пользователь</th><th>Цель</th><th>Адрес</th><th>Детали</th></tr></thead>
        <tbody>
          {#each shown as r (r.id)}
            <tr data-audit={r.action}>
              <td class="nowrap">{time(r.ts)}</td>
              <td><code>{r.action}</code></td>
              <td>{#if r.user_id}{userName(r.user_id)}{:else}<span class="muted">{r.actor}</span>{/if}</td>
              <td class="target">{r.target ?? ''}</td>
              <td class="nowrap">{r.ip ?? ''}</td>
              <td class="details">
                {#if hasDetails(r.details)}
                  <details><summary>JSON</summary><pre>{json(r.details)}</pre></details>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
  {#if next !== null}
    <div class="row more">
      <button type="button" disabled={loading} onclick={() => load(true)}>Показать ещё</button>
    </div>
  {/if}
</section>

<style>
  .small {
    font-size: 13px;
  }
  .filters {
    gap: 12px;
  }
  /* список шире области (длинное действие или имя пользователя) сжимается, а не раздвигает
     страницу на телефоне */
  .filters label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    min-width: 0;
  }
  .filters select {
    flex: 1 1 auto;
    min-width: 0;
    max-width: 100%;
  }
  .nowrap {
    white-space: nowrap;
  }
  .target {
    overflow-wrap: anywhere;
  }
  .details {
    min-width: 80px;
  }
  pre {
    margin: 4px 0 0;
    max-width: 420px;
    overflow-x: auto;
    font-size: 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .more {
    margin-top: 12px;
  }
  .cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  .card {
    display: grid;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow-wrap: anywhere;
  }
  .card-head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 4px 8px;
    overflow-wrap: anywhere;
  }
  .card pre {
    max-width: 100%;
  }
</style>
