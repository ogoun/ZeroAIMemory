<script lang="ts" module>
  export interface MenuItem {
    label: string;
    danger?: boolean;
    action: () => void;
  }
</script>

<script lang="ts">
  import { tick } from 'svelte';

  // Контекстное меню в точке (x, y) окна; закрывается по Esc, клику мимо и выбору пункта.
  let {
    x,
    y,
    items,
    onclose,
  }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  let el = $state<HTMLDivElement | null>(null);
  let pos = $state({ left: 0, top: 0 });
  let placed = $state(false);

  // Меню не выходит за край окна; фокус — на первый пункт.
  $effect(() => {
    void x;
    void y;
    tick().then(() => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      pos = {
        left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
        top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
      };
      placed = true;
      el.querySelector<HTMLButtonElement>('button')?.focus();
    });
  });

  function onkeydown(e: KeyboardEvent) {
    if (!el) return;
    const buttons = [...el.querySelectorAll<HTMLButtonElement>('button')];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      onclose();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      buttons[(i + 1) % buttons.length]?.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      buttons[(i - 1 + buttons.length) % buttons.length]?.focus();
    }
  }

  function pick(it: MenuItem) {
    onclose();
    it.action();
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  class="menu-backdrop"
  onclick={onclose}
  oncontextmenu={(e) => {
    e.preventDefault();
    onclose();
  }}
></div>
<div
  class="menu"
  role="menu"
  tabindex="-1"
  bind:this={el}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  style:visibility={placed ? 'visible' : 'hidden'}
  {onkeydown}
>
  {#each items as it (it.label)}
    <button type="button" role="menuitem" class:danger={it.danger} onclick={() => pick(it)}>
      {it.label}
    </button>
  {/each}
</div>

<style>
  .menu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
  }
  .menu {
    position: fixed;
    z-index: 41;
    min-width: 200px;
    padding: 4px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
    display: grid;
  }
  .menu button {
    border: 0;
    border-radius: 5px;
    background: transparent;
    text-align: left;
    padding: 7px 12px;
  }
  .menu button:hover,
  .menu button:focus-visible {
    background: var(--hover);
    outline: none;
  }
  .menu button.danger {
    color: var(--danger);
  }
</style>
