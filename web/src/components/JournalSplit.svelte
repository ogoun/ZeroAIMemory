<script lang="ts">
  // Журнал «список — карточка» (M7.2, спецификация §5.1 п. 5). Раскладка — по ширине своей
  // области (container query и `ResizeObserver`), а не окна:
  // - `split` (область ≥ SPLIT_MIN): список слева, карточка справа (44 %, не уже 380 px),
  //   карточка `position: sticky` со своей прокруткой; без открытой карточки список — во всю ширину;
  // - `stack` (уже): открытая карточка или форма — вместо списка, над ней «← <журнал>» (ссылка
  //   журнала без записи, `onclose` — тоже). Список скрыт, но не размонтирован: фильтры, вкладки и
  //   загруженные страницы целы; позиция прокрутки запоминается и восстанавливается;
  // - `overlay` (`mode='overlay'`, область ≥ SPLIT_MIN): карточка-панель поверх доски — делает и
  //   проверяет задача 2 M7.2; уже SPLIT_MIN — `stack`.
  // Фокус переходит только при открытии записи или формы (и при смене записи — `entry`): в `stack`
  // — на первый заголовок карточки, при возврате «←» — на нажатую строку списка, иначе на строку
  // записи (`[data-entry="<entry>"]` в списке), иначе на сам список. Смена раскладки при открытой
  // карточке (поворот, изменение окна) фокус не трогает, а прокрутку — только если фокус не в
  // карточке. Атрибут `data-layout` — текущая раскладка (для e2e).
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
    /** id открытой записи: смена — новая карточка; строка списка помечена `data-entry` с ним. */
    entry?: string | null;
    list: Snippet;
    detail: Snippet;
  }

  let { open, back, mode = 'auto', onclose, entry = null, list, detail }: Props = $props();

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
  /** Предыдущие `open` и `entry` — что открыли заново. */
  let wasOpen = false;
  let lastEntry: string | null = null;
  /** Карточку открыли (или сменили запись), а фокус ещё не переведён. */
  let fresh = false;
  /** Запись, показанная вместо списка последней (строка для фокуса при возврате). */
  let shownEntry: string | null = null;
  /** Что вернуть при возврате к списку: прокрутка (`null` — список ушёл при смене раскладки),
   * нажатая строка, запись, открытая из списка. */
  let saved: { top: number | null; focus: HTMLElement | null; entry: string | null } | null = null;
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

  const inList = (x: Element | null): x is HTMLElement => x instanceof HTMLElement && !!listEl?.contains(x);
  /** Другая запись. Уход записи из адреса (`null`) — закрытие, а не новая карточка: адрес
   * меняется раньше, чем журнал закрывает карточку (`open`), и миг «открыта без записи» не должен
   * переводить фокус. */
  const isNewRecord = (e: string | null) => e !== null && e !== lastEntry;
  const focusInDetail = () => !!detailEl?.contains(document.activeElement);

  // До скрытия списка — его прокрутка и элемент, к которому вернуть фокус. Открытие записи —
  // запоминается всё; смена раскладки при открытой карточке — только запись.
  $effect.pre(() => {
    const r = replaced && width > 0;
    const o = open;
    const e = entry;
    untrack(() => {
      if (!r || shown) return;
      const opening = fresh || (o && (!wasOpen || isNewRecord(e)));
      const a = document.activeElement;
      saved = opening
        ? { top: scroller().scrollTop, focus: inList(a) ? a : inList(activated) ? activated : null, entry: e }
        : { top: null, focus: null, entry: e };
    });
  });

  $effect(() => {
    const o = open;
    const e = entry;
    const measured = width > 0;
    const r = replaced && measured;
    untrack(() => {
      if (o && (!wasOpen || isNewRecord(e))) fresh = true;
      if (!o) fresh = false;
      wasOpen = o;
      lastEntry = e;
      if (r !== shown) {
        shown = r;
        if (r) {
          void showCard(fresh);
          fresh = false;
        } else {
          void showList(!o);
        }
      } else if (r && fresh) {
        // другая запись в `stack` (переход из карточки): её верх и заголовок
        void showCard(true);
        fresh = false;
      } else if (o && measured) {
        // карточка рядом со списком: переводить фокус не нужно, позже сужение его не отнимет
        fresh = false;
      }
    });
  });

  /** Карточка вместо списка: при открытии — её верх в видимой области под шапкой и фокус на
   * заголовке; при смене раскладки — фокус на месте, прокрутка — если фокус не в карточке. */
  async function showCard(opening: boolean) {
    shownEntry = entry;
    await tick();
    const d = detailEl;
    if (!d) return;
    if (!opening && focusInDetail()) return;
    const top = d.getBoundingClientRect().top;
    const margin = parseFloat(getComputedStyle(d).scrollMarginTop) || 0;
    if (top < margin || top > window.innerHeight - 120) d.scrollIntoView({ block: 'start' });
    if (!opening) return;
    const h = d.querySelector<HTMLElement>('h1, h2, h3');
    if (h) {
      if (!h.hasAttribute('tabindex')) h.tabIndex = -1;
      h.focus({ preventScroll: true });
    }
  }

  /** Список снова на месте. Закрыли карточку — прежняя прокрутка и фокус на строку: нажатую,
   * иначе строку записи, иначе сам список. Раскладка сменилась при открытой карточке — фокус на
   * месте, прокрутка восстанавливается, только если фокус не в карточке. */
  async function showList(closing: boolean) {
    const s = saved;
    saved = null;
    await tick();
    if (!s) return;
    const sc = scroller();
    if (!closing) {
      if (s.top !== null && !focusInDetail()) sc.scrollTop = s.top;
      return;
    }
    const same = shownEntry === s.entry;
    if (same && s.focus?.isConnected) {
      if (s.top !== null) sc.scrollTop = s.top;
      s.focus.focus({ preventScroll: true });
      return;
    }
    const row = shownEntry ? listEl?.querySelector<HTMLElement>(`[data-entry="${CSS.escape(shownEntry)}"]`) : null;
    const target = row
      ? (row.matches('a[href], button') ? row : row.querySelector<HTMLElement>('a[href], button')) ?? row
      : null;
    if (target) {
      target.scrollIntoView({ block: 'center' });
      if (!target.matches('a[href], button, [tabindex]')) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      return;
    }
    if (s.top !== null) sc.scrollTop = s.top;
    if (listEl) {
      if (!listEl.hasAttribute('tabindex')) listEl.tabIndex = -1;
      listEl.focus({ preventScroll: true });
    }
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
