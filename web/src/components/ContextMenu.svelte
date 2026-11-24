<script lang="ts" module>
  export interface MenuItem {
    label: string;
    danger?: boolean;
    action: () => void;
  }
</script>

<script lang="ts">
  import { tick } from 'svelte';

  // Контекстное меню в точке (x, y) окна; закрывается по Esc, клику мимо и выбору пункта. Закрытие
  // возвращает фокус туда, где он был до меню (кнопка «⋯» строки — образец menu button WAI-ARIA
  // APG): иначе фокус терялся, а окно, открытое пунктом меню, при закрытии не знало, куда его
  // вернуть (M7.3, §6.14 п. 1).
  let {
    x,
    y,
    items,
    onclose,
  }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  /** Меню — в конце `body`, поверх страницы: внутри липкой боковой панели (своя плоскость
   * наложения) пункты, заходящие на страницу, оказывались под её содержимым (формулы KaTeX на
   * планшете, M7.3) и не нажимались. */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  /** Элемент в фокусе до открытия меню. */
  const back = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null;

  /** Закрыть меню; фокус — на элемент до меню. */
  function close() {
    onclose();
    if (back?.isConnected) back.focus();
  }

  let el = $state<HTMLDivElement | null>(null);
  let pos = $state({ left: 0, top: 0 });
  let placed = $state(false);

  // Меню не выходит за край окна; фокус — на первый пункт, когда меню уже видимо (пункт с
  // `visibility: hidden` фокус не принимает — тогда не работали бы ни стрелки, ни Esc).
  $effect(() => {
    void x;
    void y;
    tick().then(async () => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      pos = {
        left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
        top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
      };
      placed = true;
      await tick();
      el?.querySelector<HTMLButtonElement>('button')?.focus();
    });
  });

  function onkeydown(e: KeyboardEvent) {
    if (!el) return;
    const buttons = [...el.querySelectorAll<HTMLButtonElement>('button')];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      close();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      buttons[(i + 1) % buttons.length]?.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      buttons[(i - 1 + buttons.length) % buttons.length]?.focus();
    }
  }

  function pick(it: MenuItem) {
    close();
    it.action();
  }
</script>

<div class="menu-layer" use:portal>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="menu-backdrop"
    onclick={close}
    oncontextmenu={(e) => {
      e.preventDefault();
      close();
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
  /* сенсорный ввод (M7.2, §5.1 п. 7): пункты по 44 px */
  @media (pointer: coarse) {
    .menu button {
      min-height: 44px;
      padding: 10px 14px;
    }
  }
</style>
