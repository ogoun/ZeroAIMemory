<script lang="ts">
  // Журнал «список — карточка» (M7.2, спецификация §5.1 п. 5). Раскладка — по ширине своей
  // области (container query и `ResizeObserver`), а не окна:
  // - `split` (область ≥ SPLIT_MIN): список слева, карточка справа (44 %, не уже 380 px),
  //   карточка `position: sticky` со своей прокруткой; без открытой карточки список — во всю ширину;
  // - `stack` (уже): открытая карточка или форма — вместо списка, над ней «← <журнал>» (ссылка
  //   журнала без записи, `onclose` — тоже). Список скрыт, но не размонтирован: фильтры, вкладки и
  //   загруженные страницы целы; позиция прокрутки запоминается и восстанавливается, фокус — на
  //   первый заголовок карточки при открытии и на нажатую строку списка при возврате;
  // - `overlay` (`mode='overlay'`, область ≥ SPLIT_MIN): карточка-панель поверх доски — делает и
  //   проверяет задача 2 M7.2; уже SPLIT_MIN — `stack`.
  // Атрибут `data-layout` — текущая раскладка (для e2e).
  import { tick, untrack, type Snippet } from 'svelte';
  import { SPLIT_MIN } from '../lib/layout';
  import { observeWidth } from '../lib/viewport.svelte';

  interface Props {
    /** Карточка (запись из адреса или форма) открыта. */
    open: boolean;
    /** Возврат к списку в `stack`: адрес журнала без записи и его название. */
    back: { href: string; label: string };
    mode?: 'auto' | 'overlay';
    /** Карточку закрыли («←» в `stack`). */
    onclose?: () => void;
    list: Snippet;
    detail: Snippet;
  }

  let { open, back, mode = 'auto', onclose, list, detail }: Props = $props();

  /** Ширина области; 0 — ещё не измерена (первое значение приходит до отрисовки). */
  let width = $state(0);
  let layout = $derived<'split' | 'stack' | 'overlay'>(
    width < SPLIT_MIN ? 'stack' : mode === 'overlay' ? 'overlay' : 'split',
  );
  /** Карточка вместо списка. */
  let replaced = $derived(open && layout === 'stack');

  let root = $state<HTMLElement | null>(null);
  let listEl = $state<HTMLElement | null>(null);
  let detailEl = $state<HTMLElement | null>(null);

  /** Показана ли карточка вместо списка (после замера ширины): переходы — по смене. */
  let shown = false;
  /** Что вернуть при возврате к списку. */
  let saved: { top: number; focus: HTMLElement | null } | null = null;
  /** Последний нажатый элемент списка: Safari не даёт кнопке фокус по нажатию. */
  let activated: HTMLElement | null = null;

  /** Прокручиваемый предок области (обычно — документ). */
  function scroller(): HTMLElement {
    for (let el = root?.parentElement ?? null; el; el = el.parentElement) {
      const o = getComputedStyle(el).overflowY;
      if ((o === 'auto' || o === 'scroll') && el.scrollHeight > el.clientHeight) return el;
    }
    return (document.scrollingElement ?? document.documentElement) as HTMLElement;
  }

  function remember(e: MouseEvent) {
    const t = e.target instanceof Element ? e.target.closest<HTMLElement>('button, a, [tabindex]') : null;
    if (t) activated = t;
  }

  // До скрытия списка — его прокрутка и элемент, к которому вернуть фокус.
  $effect.pre(() => {
    const r = replaced && width > 0;
    untrack(() => {
      if (!r || shown) return;
      const a = document.activeElement;
      const inList = (x: Element | null): x is HTMLElement => x instanceof HTMLElement && !!listEl?.contains(x);
      saved = { top: scroller().scrollTop, focus: inList(a) ? a : inList(activated) ? activated : null };
    });
  });

  $effect(() => {
    const r = replaced && width > 0;
    untrack(() => {
      if (r === shown) return;
      shown = r;
      void (r ? showCard() : showList());
    });
  });

  /** Карточка вместо списка: её верх — в видимой области под шапкой, фокус — на заголовок. */
  async function showCard() {
    await tick();
    const d = detailEl;
    if (!d) return;
    const top = d.getBoundingClientRect().top;
    const margin = parseFloat(getComputedStyle(d).scrollMarginTop) || 0;
    if (top < margin || top > window.innerHeight - 120) d.scrollIntoView({ block: 'start' });
    const h = d.querySelector<HTMLElement>('h1, h2, h3');
    if (h) {
      if (!h.hasAttribute('tabindex')) h.tabIndex = -1;
      h.focus({ preventScroll: true });
    }
  }

  /** Список снова на месте: прежняя прокрутка; закрыли карточку — фокус на её строку. */
  async function showList() {
    const s = saved;
    saved = null;
    await tick();
    if (!s) return;
    scroller().scrollTop = s.top;
    if (!open && s.focus?.isConnected) s.focus.focus({ preventScroll: true });
  }
</script>

<div class="jsplit" bind:this={root} data-layout={layout} use:observeWidth={(w) => (width = w)}>
  <div class="jsplit-grid" class:open class:overlay={mode === 'overlay'}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="jsplit-list" bind:this={listEl} hidden={replaced} onclickcapture={remember}>
      {@render list()}
    </div>
    {#if open}
      <div class="jsplit-detail" bind:this={detailEl}>
        {#if layout === 'stack'}
          <a class="jsplit-back" href={back.href} onclick={() => onclose?.()}>← {back.label}</a>
        {/if}
        {@render detail()}
      </div>
    {/if}
  </div>
</div>

<style>
  .jsplit {
    container: jsplit / inline-size;
  }
  .jsplit-list[hidden] {
    display: none;
  }
  .jsplit-detail {
    min-width: 0;
    scroll-margin-top: calc(var(--header-h) + 8px);
  }
  .jsplit-back {
    display: inline-flex;
    align-items: center;
    min-height: 32px;
    margin-bottom: 4px;
    text-decoration: none;
  }
  @media (pointer: coarse) {
    .jsplit-back {
      min-height: 44px;
      min-width: 44px;
    }
  }
  /* ширина — SPLIT_MIN (layout.ts) */
  @container jsplit (min-width: 840px) {
    .jsplit-grid.open:not(.overlay) {
      display: grid;
      grid-template-columns: minmax(0, 1fr) max(380px, 44%);
      gap: 16px;
      align-items: start;
    }
    .jsplit-grid.open:not(.overlay) > .jsplit-detail {
      position: sticky;
      top: calc(var(--header-h) + 16px);
      max-height: calc(var(--app-h) - var(--header-h) - 32px);
      overflow: auto;
      overscroll-behavior: contain;
    }
  }
</style>
