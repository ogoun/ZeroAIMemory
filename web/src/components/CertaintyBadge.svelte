<script lang="ts">
  // Значок эпистемического статуса (§7.5): axiom — ромб, theorem — галочка, hypothesis — «?».
  import type { Certainty } from '../lib/api';
  import { CERTAINTY_TEXT } from '../lib/journals';

  let { value }: { value: Certainty | null | undefined } = $props();
</script>

{#if value}
  <span class="cert {value}" title={CERTAINTY_TEXT[value]} data-certainty={value}>
    <svg viewBox="0 0 12 12" aria-hidden="true">
      {#if value === 'axiom'}
        <path d="M6 1l5 5-5 5-5-5z" fill="currentColor" />
      {:else if value === 'theorem'}
        <path d="M2 6.5l2.6 2.5L10 3.2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
      {:else}
        <path d="M4.2 4.4a1.9 1.9 0 1 1 2.6 1.8c-.6.3-.8.6-.8 1.2v.4 M6 9.6v.2" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
      {/if}
    </svg>{value}
  </span>
{/if}

<style>
  .cert {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px 0 4px;
    border-radius: 8px;
    font-size: 12px;
    line-height: 18px;
    white-space: nowrap;
    border: 1px solid currentColor;
  }
  .cert svg {
    width: 11px;
    height: 11px;
  }
  .axiom {
    color: var(--callout-important);
  }
  .theorem {
    color: var(--callout-tip);
  }
  .hypothesis {
    color: var(--callout-warning);
  }
</style>
