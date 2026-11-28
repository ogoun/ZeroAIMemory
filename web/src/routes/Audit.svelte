<script lang="ts">
  // «Администрирование → Аудит» (#/admin/audit; M7.2, спецификация M7 §5.6 п. 4; ленивый чанк):
  // лента `GET /admin/audit` по 100 событий, новые первыми, «Показать ещё» — по курсору `before`;
  // время — местное (`localTime`), действие, пользователь (имя по списку пользователей; без
  // пользователя — кто действовал: `admin`, `system`, `anonymous`…), цель, адрес, детали JSON —
  // свёрнуты. На телефоне — карточками строк.
  // M7.4 (§7.6 п. 1, §7.7 п. 3; решение 25 снято): фильтры — запросом к серверу, а не по
  // загруженному: действие (точно или префиксом `user.*` — поле с подсказками из загруженных
  // событий, применяется Enter или «Применить»), пользователь (список всех пользователей),
  // период — даты «с» и «по» включительно (граница «по» — начало следующего дня, сервер её не
  // включает). Список и даты применяются сразу; «Показать ещё» листает с теми же фильтрами.
  import { onMount } from 'svelte';
  import { ApiError, errorText, listAudit, type AuditFilter, type AuditRow, type User } from '../lib/api';
  import { localTime } from '../lib/time';
  import { viewport } from '../lib/viewport.svelte';

  let { users, onexpired }: { users: User[]; onexpired: () => void } = $props();

  let rows = $state<AuditRow[]>([]);
  let next = $state<number | null>(null);
  let loaded = $state(false);
  let loading = $state(false);
  let error = $state('');
  /** Поля фильтров: действие набирается, список и даты применяются сразу. */
  let action = $state('');
  let user = $state('');
  let from = $state('');
  let to = $state('');
  /** Фильтры, по которым загружена лента («Показать ещё» — с ними же). */
  let applied = $state<AuditFilter>({});
  let periodError = $state('');
  let seq = 0;

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
      onexpired();
      return true;
    }
    return false;
  }

  /** Местная дата `YYYY-MM-DD` → начало дня (RFC 3339, UTC); `days` — сдвиг (граница «по»). */
  function dayStart(d: string, days = 0): string | undefined {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(d);
    if (!m) return undefined;
    return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + days).toISOString();
  }

  async function load(more: boolean) {
    const my = ++seq;
    const f = applied;
    loading = true;
    error = '';
    try {
      const r = await listAudit(f, more && next !== null ? next : undefined);
      if (my !== seq) return;
      // курсор — id: повтор страницы (двойной щелчок) не дублирует строки
      const seen = new Set(more ? rows.map((x) => x.id) : []);
      rows = [...(more ? rows : []), ...r.items.filter((x) => !seen.has(x.id))];
      next = r.next;
      loaded = true;
    } catch (e) {
      if (my !== seq) return;
      if (expired(e)) return;
      error = errorText(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** Применить поля фильтров: лента — заново с первой страницы. */
  function apply() {
    periodError = '';
    if (from && to && from > to) {
      periodError = 'Начало периода позже его конца.';
      return;
    }
    applied = { action: action.trim(), user, since: dayStart(from), until: dayStart(to, 1) };
    void load(false);
  }

  function reset() {
    action = '';
    user = '';
    from = '';
    to = '';
    apply();
  }

  onMount(() => {
    void load(false);
  });

  const userName = (id: string | null) => (id ? (users.find((u) => u.id === id)?.name ?? id) : '');
  const time = (ts: string) =>
    localTime(ts, { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const hasDetails = (d: unknown) => d !== null && d !== undefined && !(typeof d === 'object' && Object.keys(d as object).length === 0);
  const json = (d: unknown) => JSON.stringify(d, null, 2);

  /** Подсказки поля «Действие»: действия загруженных событий и их префиксы (`user.*`) — о прочих
   * сервер знает сам, поле принимает любое. */
  let suggestions = $derived.by(() => {
    const acts = [...new Set(rows.map((r) => r.action))];
    const prefixes = [...new Set(acts.flatMap((a) => (a.includes('.') ? [`${a.split('.')[0]}.*`] : [])))];
    return [...prefixes, ...acts].sort();
  });
  let people = $derived([...users].sort((a, b) => a.name.localeCompare(b.name)));
  let filtered = $derived(!!(applied.action || applied.user || applied.since || applied.until));
  let phone = $derived(viewport.current === 'phone');
</script>

<section class="panel audit-page">
  <h2>Аудит</h2>
  <form
    class="row filters"
    aria-label="Фильтры аудита"
    onsubmit={(e) => {
      e.preventDefault();
      apply();
    }}
  >
    <label class="grow">
      Действие
      <input
        type="search"
        bind:value={action}
        list="audit-actions"
        placeholder="user.password.set или user.*"
        spellcheck="false"
        autocomplete="off"
        aria-describedby="audit-action-hint"
      />
    </label>
    <datalist id="audit-actions">
      {#each suggestions as a (a)}<option value={a}></option>{/each}
    </datalist>
    <label>
      Пользователь
      <select bind:value={user} onchange={apply}>
        <option value="">все</option>
        {#each people as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
      </select>
    </label>
    <label>
      С
      <input type="date" bind:value={from} max={to || undefined} onchange={apply} aria-describedby="audit-period-hint" />
    </label>
    <label>
      По
      <input type="date" bind:value={to} min={from || undefined} onchange={apply} aria-describedby="audit-period-hint" />
    </label>
    <div class="row buttons">
      <button type="submit" disabled={loading}>Применить</button>
      {#if filtered || action || user || from || to}
        <button type="button" disabled={loading} onclick={reset}>Сбросить</button>
      {/if}
    </div>
  </form>
  <p class="muted small" id="audit-action-hint">
    Фильтры применяет сервер: действие — точно или префиксом со звёздочкой (<code>user.*</code>), «Показать ещё» листает
    с теми же фильтрами.
  </p>
  <p class="muted small" id="audit-period-hint" hidden>Даты включительно, по местному времени.</p>
  {#if periodError}<p class="error" role="alert">{periodError}</p>{/if}
  <p class="muted small" role="status" data-testid="audit-count">
    {#if loaded}{filtered ? 'По фильтрам' : 'Загружено'}: {rows.length}{next !== null ? ' — есть ещё' : ''}{/if}
  </p>
  {#if error}<p class="error" role="alert">Не удалось получить журнал аудита: {error}</p>{/if}

  {#if !loaded && !error}
    <p class="muted">Загрузка…</p>
  {:else if loaded && !rows.length}
    <p class="muted">{filtered ? 'Событий по этим фильтрам нет.' : 'Событий нет.'}</p>
  {:else if phone}
    <ul class="cards" aria-label="События аудита">
      {#each rows as r (r.id)}
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
      <table aria-label="События аудита">
        <thead><tr><th>Время</th><th>Действие</th><th>Пользователь</th><th>Цель</th><th>Адрес</th><th>Детали</th></tr></thead>
        <tbody>
          {#each rows as r (r.id)}
            <tr data-audit={r.action} data-user={r.user_id ?? ''}>
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
    gap: 8px 12px;
    align-items: flex-end;
  }
  /* поле шире области (длинное действие или имя пользователя) сжимается, а не раздвигает страницу
     на телефоне */
  .filters label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    min-width: 0;
  }
  .filters label.grow {
    flex: 1 1 260px;
  }
  .filters :is(input, select) {
    flex: 1 1 auto;
    min-width: 0;
    max-width: 100%;
  }
  .buttons {
    gap: 8px;
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
