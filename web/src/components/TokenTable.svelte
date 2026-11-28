<script lang="ts">
  // Список токенов (M7.6 §9.1, решение координатора 143) — «Мои токены» и токены пользователя у
  // администратора: над таблицей — что значит «Отозвать»; действующие — в таблице с кнопкой
  // «Отозвать», отозванные не удаляются (история и аудит), а свёрнуты под «Отозванные (N)».
  // Строки — `tr[data-token=<название>]` (e2e находят строку и после отзыва).
  import type { TokenInfo } from '../lib/api';

  let {
    tokens,
    label,
    time,
    ip = false,
    onrevoke,
  }: {
    tokens: TokenInfo[];
    /** `aria-label` таблицы действующих токенов. */
    label: string;
    /** Время для показа (у страниц — свой формат). */
    time: (iso: string) => string;
    /** Показывать адрес последнего использования. */
    ip?: boolean;
    onrevoke: (t: TokenInfo) => void;
  } = $props();

  let active = $derived(tokens.filter((t) => !t.revoked_at));
  let revoked = $derived(tokens.filter((t) => t.revoked_at));

  const used = (t: TokenInfo) => (t.last_used_at ? time(t.last_used_at) : '—') + (ip && t.last_ip ? ` · ${t.last_ip}` : '');
</script>

<p class="muted small lead">Отозвать — токен перестаёт работать сразу и навсегда; строка остаётся в истории.</p>
<!-- таблица — в `.table-wrap` (§5.1 п. 6): на телефоне прокручивается внутри -->
<div class="table-wrap">
  <table aria-label={label}>
    <thead><tr><th>Название</th><th>Префикс</th><th>Доступ</th><th>Использован</th><th></th></tr></thead>
    <tbody>
      {#each active as t (t.id)}
        <tr data-token={t.name}>
          <td>{t.name}</td>
          <td><code>zam_{t.prefix}…</code></td>
          <td>{t.access}</td>
          <td class="muted">{used(t)}</td>
          <td><button type="button" class="danger" onclick={() => onrevoke(t)}>Отозвать</button></td>
        </tr>
      {:else}
        <tr><td colspan="5" class="muted">Действующих токенов нет.</td></tr>
      {/each}
    </tbody>
  </table>
</div>
{#if revoked.length}
  <details class="revoked">
    <summary>Отозванные ({revoked.length})</summary>
    <div class="table-wrap">
      <table aria-label="Отозванные токены">
        <thead><tr><th>Название</th><th>Префикс</th><th>Доступ</th><th>Использован</th><th>Отозван</th></tr></thead>
        <tbody>
          {#each revoked as t (t.id)}
            <tr data-token={t.name}>
              <td>{t.name}</td>
              <td><code>zam_{t.prefix}…</code></td>
              <td>{t.access}</td>
              <td class="muted">{used(t)}</td>
              <td class="muted">отозван {time(t.revoked_at ?? '')}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </details>
{/if}

<style>
  .lead {
    margin: 0 0 6px;
  }
  .small {
    font-size: 12px;
  }
  .revoked {
    margin-top: 8px;
  }
  .revoked summary {
    cursor: pointer;
    width: fit-content;
  }
  .revoked .table-wrap {
    margin-top: 6px;
  }
</style>
