<script lang="ts">
  // Блок администратора «Внутренних работ» (M7.4, спецификация §7.7 п. 2; §7.6 п. 2): сводка `GET
  // /admin/works` по всем пользователям — действующие режимы видов, пауза, очередь, упавшие работы
  // за 7 суток, последняя ночь, токены модели суток и месяца, последний законченный прогон ночной
  // проверки. Имя пользователя ведёт на его вид страницы (`#/works?user=<id>`; свой — `#/works`),
  // показанный сейчас — `aria-current`. База пользователя не открылась — строка с причиной, прочие
  // на месте. Таблица — `.table-wrap`, на телефоне — карточки (§5.1). Часть чанка «Внутренних
  // работ»: страница показывает блок только администратору.
  import { onMount } from 'svelte';
  import { ApiError, errorText } from '../lib/api';
  import { href } from '../lib/router';
  import { viewport } from '../lib/viewport.svelte';
  import { adminWorks, type AdminWorksRow } from '../lib/worksApi';
  import { PAUSE_TITLE, adminCheckText, modesSummary, num, queueText } from '../lib/worksText';

  let {
    current,
    self,
    onexpired,
  }: {
    /** Пользователь, чья страница показана. */
    current: string;
    /** Свой пользователь администратора (его строка ведёт на `#/works`). */
    self: string;
    onexpired: () => void;
  } = $props();

  let rows = $state<AdminWorksRow[] | null>(null);
  let error = $state('');

  onMount(() => {
    void (async () => {
      try {
        rows = (await adminWorks()).users;
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) return onexpired();
        error = errorText(e);
      }
    })();
  });

  let phone = $derived(viewport.current === 'phone');
  const link = (r: AdminWorksRow) => href({ name: 'works', user: r.user === self ? null : r.user });
  const tokens = (r: AdminWorksRow) => (r.tokens ? `${num(r.tokens.day)} / ${num(r.tokens.month)}` : '—');
  const regression = (r: AdminWorksRow) => r.check?.verdict === 'regression';
</script>

<section class="panel block" aria-labelledby="works-admin-h" data-testid="works-admin">
  <h2 id="works-admin-h">Все пользователи</h2>
  <p class="muted small">
    Сводка администратора: режимы видов, очередь, упавшие за 7 суток работы, последняя ночь, токены модели (сутки / месяц,
    UTC) и последний прогон ночной проверки. Имя — страница пользователя.
  </p>
  {#if error}<p class="error" role="alert">Не удалось получить сводку: {error}</p>{/if}
  {#if !rows && !error}
    <p class="muted">Загрузка…</p>
  {:else if rows && !rows.length}
    <p class="muted">Пользователей нет.</p>
  {:else if rows && phone}
    <ul class="cards" aria-label="Сводка по пользователям">
      {#each rows as r (r.user)}
        <li class="card" data-user={r.user}>
          <div class="card-head">
            <a href={link(r)} aria-current={r.user === current ? 'page' : undefined}><b>{r.login}</b></a>
            {#if r.paused}<span class="tag pause">пауза: {PAUSE_TITLE[r.paused.class] ?? r.paused.class}</span>{/if}
          </div>
          {#if r.error}
            <div class="small error">База не прочитана: {r.error}</div>
          {:else}
            <div class="small">{modesSummary(r.modes)}</div>
            <div class="small">Очередь: {queueText(r.queued, r.running)} · упало за 7 суток: {num(r.failed_7d ?? 0)}</div>
            <div class="small">Последняя ночь: {r.last_night ?? 'не было'} · токены: {tokens(r)}</div>
            <div class="small" class:bad={regression(r)}>Проверка: {adminCheckText(r.check)}</div>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if rows}
    <div class="table-wrap">
      <table aria-label="Сводка по пользователям">
        <thead>
          <tr>
            <th>Пользователь</th>
            <th>Режимы видов</th>
            <th>Очередь</th>
            <th>Упало за 7 суток</th>
            <th>Последняя ночь</th>
            <th>Токены, сутки / месяц</th>
            <th>Проверка</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.user)}
            <tr data-user={r.user} class:current={r.user === current}>
              <td>
                <a href={link(r)} aria-current={r.user === current ? 'page' : undefined}>{r.login}</a>
                {#if r.paused}<div><span class="tag pause">пауза: {PAUSE_TITLE[r.paused.class] ?? r.paused.class}</span></div>{/if}
              </td>
              {#if r.error}
                <td colspan="6" class="error">База не прочитана: {r.error}</td>
              {:else}
                <td>{modesSummary(r.modes)}</td>
                <td>{queueText(r.queued, r.running)}</td>
                <td class="num">{num(r.failed_7d ?? 0)}</td>
                <td class="nowrap">{r.last_night ?? 'не было'}</td>
                <td class="num">{tokens(r)}</td>
                <td class:bad={regression(r)}>{adminCheckText(r.check)}</td>
              {/if}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .block {
    margin-top: 16px;
  }
  .small {
    font-size: 13px;
  }
  .num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .nowrap {
    white-space: nowrap;
  }
  td {
    vertical-align: top;
  }
  tr.current {
    background: var(--selected);
  }
  .bad {
    color: var(--danger);
    font-weight: 600;
  }
  .tag {
    display: inline-block;
    margin-top: 2px;
    padding: 0 6px;
    border-radius: 4px;
    font-size: 12px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .cards {
    list-style: none;
    margin: 8px 0 0;
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
    align-items: center;
    justify-content: space-between;
    gap: 4px 8px;
  }
  /* ссылка — цель касания не меньше 44 px по высоте (§5.1) */
  .card-head a {
    display: inline-flex;
    align-items: center;
    min-height: 44px;
  }
</style>
