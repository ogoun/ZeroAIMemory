<script lang="ts">
  // Меню аккаунта шапки телефона (M7.2, спецификация §5.1 п. 3): логин и «Выйти». Раскрывашка
  // (`aria-expanded`, панель в DOM всегда — `hidden`, поэтому `aria-controls` ссылается на
  // существующий узел); закрывается Esc (фокус — на кнопку меню), щелчком мимо, уходом фокуса из
  // меню и сменой `closeKey` (переход по адресу).
  import { untrack } from 'svelte';

  let { login, onlogout, closeKey }: { login: string; onlogout: () => void; closeKey?: unknown } = $props();

  let open = $state(false);
  let root = $state<HTMLElement | null>(null);
  let button = $state<HTMLButtonElement | null>(null);

  $effect(() => {
    void closeKey;
    untrack(() => (open = false));
  });

  function onpointerdown(e: PointerEvent) {
    if (open && e.target instanceof Node && !root?.contains(e.target)) open = false;
  }

  function onfocusout(e: FocusEvent) {
    if (e.relatedTarget instanceof Node && !root?.contains(e.relatedTarget)) open = false;
  }

  // Esc меню отмечается обработанным: Esc оболочки (выдвижная панель) его уже не берёт
  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !open) return;
    e.preventDefault();
    open = false;
    button?.focus();
  }
</script>

<svelte:document {onpointerdown} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="account" bind:this={root} {onfocusout} {onkeydown}>
  <button
    type="button"
    class="account-btn"
    bind:this={button}
    aria-label="Меню аккаунта"
    aria-expanded={open}
    aria-controls="account-menu"
    onclick={() => (open = !open)}
  >
    <svg viewBox="0 0 20 20" aria-hidden="true"
      ><circle cx="10" cy="7" r="3.4" fill="none" stroke="currentColor" stroke-width="1.6" /><path
        d="M3.5 17.5c.8-3.6 3.4-5.4 6.5-5.4s5.7 1.8 6.5 5.4"
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
        stroke-linecap="round"
      /></svg
    >
  </button>
  <div id="account-menu" class="account-menu panel" hidden={!open}>
    <span class="muted">{login}</span>
    <button type="button" onclick={onlogout}>Выйти</button>
  </div>
</div>

<style>
  .account {
    position: relative;
    flex: none;
  }
  .account-btn {
    display: grid;
    place-items: center;
    padding: 6px;
    border: 0;
    background: transparent;
  }
  .account-btn svg {
    display: block;
    width: 20px;
    height: 20px;
  }
  .account-menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 35;
    display: grid;
    gap: 8px;
    min-width: 200px;
    max-width: calc(100vw - 16px);
    padding: 12px;
    overflow-wrap: anywhere;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
  }
  .account-menu[hidden] {
    display: none;
  }
  @media (pointer: coarse) {
    .account-btn {
      min-width: 44px;
      min-height: 44px;
    }
  }
</style>
