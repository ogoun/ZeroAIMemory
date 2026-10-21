<script lang="ts">
  // Значок состояния индексации заметки (`index_state`) — встроенный SVG, без библиотек
  // иконок: `pending` — часы, `failed` — восклицательный знак, `excluded` — перечёркнутый
  // круг. `indexed` — без значка (обычное состояние не шумит). `decorative` — рядом есть
  // текст состояния: значок скрыт от экранных читалок, чтобы не читать его дважды.
  import { stateText } from '../lib/indexing';

  let { state, decorative = false }: { state: string; decorative?: boolean } = $props();
</script>

{#if state === 'pending' || state === 'failed' || state === 'excluded'}
  <span
    class="ibadge ibadge-{state}"
    role={decorative ? undefined : 'img'}
    aria-label={decorative ? undefined : stateText(state)}
    aria-hidden={decorative ? 'true' : undefined}
    title={decorative ? undefined : stateText(state)}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true">
      {#if state === 'pending'}
        <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.5" />
        <path d="M8 4.6V8l2.4 1.6" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      {:else if state === 'failed'}
        <circle cx="8" cy="8" r="7" fill="currentColor" />
        <path d="M8 4v5" stroke="var(--panel)" stroke-width="1.8" stroke-linecap="round" />
        <circle cx="8" cy="11.6" r="1.1" fill="var(--panel)" />
      {:else}
        <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.4" />
        <path d="M3.8 12.2l8.4-8.4" stroke="currentColor" stroke-width="1.4" />
      {/if}
    </svg>
  </span>
{/if}

<style>
  .ibadge {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    margin: 0 2px;
    vertical-align: -3px;
    color: var(--muted);
  }
  .ibadge svg {
    width: 14px;
    height: 14px;
  }
  .ibadge-failed {
    color: var(--danger);
  }
  .ibadge-excluded {
    opacity: 0.7;
  }
</style>
