<script lang="ts">
  // Меню аккаунта шапки телефона (M7.2, спецификация §5.1 п. 3): логин, «Внутренние работы» (M7.3
  // §6.9 п. 1 — с точкой, пока исполнитель на паузе; точка видна и на самой кнопке меню) и
  // «Выйти». Раскрывашка (`aria-expanded`, панель в DOM всегда — `hidden`, поэтому `aria-controls`
  // ссылается на существующий узел); закрывается Esc (где бы ни был фокус; фокус из меню — на
  // кнопку меню), щелчком мимо, уходом фокуса из меню и сменой `closeKey` (переход по адресу).
  import { untrack } from 'svelte';
  import { href } from '../lib/router';

  let {
    login,
    onlogout,
    closeKey,
    paused = false,
  }: { login: string; onlogout: () => void; closeKey?: unknown; paused?: boolean } = $props();

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

  // Esc — на документе, пока меню открыто: Safari и Firefox на macOS не дают кнопке фокус по
  // щелчку, и фокус может быть вне меню. Esc отмечается обработанным — Esc оболочки (выдвижная
  // панель) его уже не берёт; фокус был в меню или потерян — на кнопку меню.
  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !open || e.defaultPrevented) return;
    e.preventDefault();
    open = false;
    const a = document.activeElement;
    if (!a || a === document.body || root?.contains(a)) button?.focus();
  }
</script>

<svelte:document {onpointerdown} {onkeydown} />

<div class="account" bind:this={root} {onfocusout}>
  <button
    type="button"
    class="account-btn"
    bind:this={button}
    aria-label="Меню аккаунта"
    aria-describedby={paused ? 'account-paused' : undefined}
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
    {#if paused}<span class="dot btn-dot" aria-hidden="true"></span>{/if}
  </button>
  {#if paused}<span id="account-paused" hidden>исполнитель внутренних работ на паузе</span>{/if}
  <div id="account-menu" class="account-menu panel" hidden={!open}>
    <span class="muted">{login}</span>
    <a class="item" href={href({ name: 'works' })} aria-describedby={paused ? 'account-paused' : undefined}
      >Внутренние работы{#if paused}<span class="dot" aria-hidden="true" data-testid="works-dot"></span>{/if}</a
    >
    <button type="button" onclick={onlogout}>Выйти</button>
  </div>
</div>

<style>
  .account {
    position: relative;
    flex: none;
  }
  .account-btn {
    position: relative;
    display: grid;
    place-items: center;
    padding: 6px;
    border: 0;
    background: transparent;
  }
  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-left: 6px;
    border-radius: 50%;
    background: var(--danger);
  }
  .btn-dot {
    position: absolute;
    top: 6px;
    right: 4px;
    margin: 0;
  }
  .item {
    display: flex;
    align-items: center;
    text-decoration: none;
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
    .account-btn,
    .item {
      min-width: 44px;
      min-height: 44px;
    }
  }
</style>
