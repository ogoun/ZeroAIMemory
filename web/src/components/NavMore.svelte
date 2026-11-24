<script lang="ts">
  // «Ещё ▾» шапки (M7.3, ревью r1 задачи 8, I2): разделы, которые не помещаются в шапку, — меню у
  // правого края полосы разделов. Кнопка меню по WAI-ARIA APG «Menu Button»: `aria-haspopup="menu"`,
  // `aria-expanded`, `aria-controls` (меню в DOM всегда — `hidden`); щелчок, Enter и Пробел
  // открывают с фокусом на первом пункте, ↓ — тоже, ↑ — на последнем; в меню ↑/↓ — по кругу,
  // Home/End — к краям, Esc — закрыть, фокус на кнопку (Esc отмечается обработанным — выдвижная
  // панель и слои ниже его не берут), Tab, щелчок мимо и уход фокуса — закрыть; выбор пункта и
  // переход (`closeKey`) — закрыть. Меню — `position: fixed` под кнопкой: полоса разделов его не
  // обрезает. Пункты (`role="menuitem"`, `tabindex="-1"`) и признаки кнопки (точка, число) рисует
  // оболочка: кнопка показывает признаки скрытых в меню разделов.
  import { tick, untrack, type Snippet } from 'svelte';

  let {
    current = false,
    describedby,
    closeKey,
    badges,
    children,
  }: {
    /** Текущий раздел — в меню: кнопка выделена, как выбранный раздел. */
    current?: boolean;
    /** Описание признаков скрытых разделов (id через пробел) — для чтения с экрана. */
    describedby?: string;
    closeKey?: unknown;
    /** Признаки скрытых разделов у подписи кнопки (точка, число). */
    badges?: Snippet;
    children: Snippet;
  } = $props();

  let open = $state(false);
  let root = $state<HTMLElement | null>(null);
  let button = $state<HTMLButtonElement | null>(null);
  let menu = $state<HTMLElement | null>(null);
  /** Место меню в окне: под кнопкой, по её правому краю. */
  let pos = $state({ top: 0, right: 8 });

  $effect(() => {
    void closeKey;
    untrack(() => (open = false));
  });

  const items = (): HTMLElement[] => [...(menu?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];

  async function show(focus: 'first' | 'last') {
    const r = button?.getBoundingClientRect();
    if (r) pos = { top: Math.round(r.bottom + 6), right: Math.max(8, Math.round(window.innerWidth - r.right)) };
    open = true;
    await tick();
    const list = items();
    (focus === 'last' ? list.at(-1) : list[0])?.focus({ preventScroll: true });
  }

  function hide(refocus: boolean) {
    open = false;
    if (refocus) button?.focus({ preventScroll: true });
  }

  function onbuttonkeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    void show(e.key === 'ArrowUp' ? 'last' : 'first');
  }

  function onmenukeydown(e: KeyboardEvent) {
    const list = items();
    if (!list.length) return;
    const i = list.indexOf(document.activeElement as HTMLElement);
    let next: HTMLElement | undefined;
    switch (e.key) {
      case 'ArrowDown':
        next = list[(i + 1) % list.length];
        break;
      case 'ArrowUp':
        next = list[(i - 1 + list.length) % list.length];
        break;
      case 'Home':
        next = list[0];
        break;
      case 'End':
        next = list.at(-1);
        break;
      case 'Escape':
        e.preventDefault();
        hide(true);
        return;
      case 'Tab':
        // фокус уходит дальше по порядку, меню закрывается
        open = false;
        return;
      default:
        return;
    }
    e.preventDefault();
    next?.focus({ preventScroll: true });
  }

  /** Выбор пункта закрывает меню, даже если адрес не меняется (текущий раздел). */
  function onmenuclick(e: MouseEvent) {
    if (e.target instanceof Element && e.target.closest('[role="menuitem"]')) hide(true);
  }

  function onpointerdown(e: PointerEvent) {
    if (open && e.target instanceof Node && !root?.contains(e.target)) open = false;
  }

  function onfocusout(e: FocusEvent) {
    if (e.relatedTarget instanceof Node && !root?.contains(e.relatedTarget)) open = false;
  }

  // Esc — на документе, пока меню открыто (фокус может остаться на кнопке или вне меню)
  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !open || e.defaultPrevented) return;
    e.preventDefault();
    const a = document.activeElement;
    hide(!a || a === document.body || !!root?.contains(a));
  }
</script>

<svelte:document {onpointerdown} {onkeydown} />
<svelte:window onresize={() => (open = false)} />

<span class="nav-more" bind:this={root} {onfocusout}>
  <button
    type="button"
    class="nav-more-btn item"
    class:current
    bind:this={button}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls="nav-more-menu"
    aria-describedby={describedby}
    onclick={() => (open ? hide(false) : void show('first'))}
    onkeydown={onbuttonkeydown}
    >Ещё<span class="caret" aria-hidden="true">▾</span>{@render badges?.()}</button
  >
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    id="nav-more-menu"
    class="nav-more-menu panel"
    role="menu"
    aria-label="Ещё разделы"
    tabindex="-1"
    hidden={!open}
    bind:this={menu}
    style:top="{pos.top}px"
    style:right="{pos.right}px"
    onkeydown={onmenukeydown}
    onclick={onmenuclick}
  >
    {@render children()}
  </div>
</span>

<style>
  .nav-more {
    flex: none;
    display: inline-flex;
  }
  .caret {
    margin-left: 4px;
    font-size: 0.8em;
  }
  .nav-more-menu {
    position: fixed;
    z-index: 35;
    display: grid;
    gap: 2px;
    min-width: 200px;
    max-width: calc(100vw - 16px);
    max-height: calc(var(--app-h, 100vh) - var(--header-h, 52px) - 16px);
    overflow: auto;
    padding: 6px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
  }
  .nav-more-menu[hidden] {
    display: none;
  }
</style>
