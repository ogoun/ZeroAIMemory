<script lang="ts">
  // Ссылки записи на файлы (M4.5 §6): путь и состояние; у `stale` — «возможно устарело».
  import type { RefState } from '../lib/fileRefs';

  let { refs, verifiedAt = '' }: { refs: RefState[]; verifiedAt?: string } = $props();
</script>

{#if refs.length || verifiedAt}
  <div class="refs" data-testid="file-refs">
    {#if refs.length}
      <h3>Файлы</h3>
      <ul>
        {#each refs as r (r.path)}
          <li class:stale={r.state === 'stale'}>
            <code>{r.path}</code>
            {#if r.state === 'stale'}<span class="badge" title="Файл изменился после записи знания">возможно устарело</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if verifiedAt}<p class="muted small">проверено {verifiedAt}</p>{/if}
  </div>
{/if}

<style>
  .refs h3 {
    margin: 10px 0 4px;
    font-size: 15px;
  }
  ul {
    margin: 0;
    padding-left: 18px;
  }
  .badge {
    margin-left: 6px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--warn-bg);
    color: var(--warn-text);
    font-size: 12px;
  }
  .small {
    font-size: 12px;
    margin: 4px 0 0;
  }
</style>
