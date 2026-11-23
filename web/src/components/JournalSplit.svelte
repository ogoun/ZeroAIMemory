<script lang="ts">
  // Журнал «список — карточка» (M7.2, спецификация §5.1 п. 5). Раскладка — по ширине своей
  // области (container query и `ResizeObserver`), а не окна:
  // - `split` (область ≥ SPLIT_MIN): список слева, карточка справа (44 %, не уже 380 px),
  //   карточка `position: sticky` со своей прокруткой; без открытой карточки список — во всю ширину;
  // - `stack` (уже): открытая карточка или форма — вместо списка, над ней «← <журнал>» (ссылка
  //   журнала без записи, `onclose` — тоже; несохранённое (`dirty()`) — сначала тот же вопрос
  //   «Закрыть без сохранения?», что у панели). Список скрыт, но не размонтирован: фильтры, вкладки
  //   и загруженные страницы целы; позиция прокрутки запоминается и восстанавливается;
  // - `overlay` (`mode='overlay'`, область ≥ SPLIT_MIN; доска задач, §5.2 п. 1): карточка — боковая
  //   панель 480 px поверх списка (окно `role="dialog"` у правого края под шапкой, своя прокрутка),
  //   список не сужается; фон, Esc и «Закрыть» закрывают (`onclose` и адрес журнала без записи —
  //   «назад» истории, если панель открыта со списка, иначе заменой адреса: закрытие не добавляет
  //   запись истории; повторное закрытие, пока идёт «назад», ничего не делает). Несохранённое молча
  //   не теряется: журнал отвечает `dirty()` (что не сохранено — знает он), и тогда сначала вопрос
  //   «Закрыть без сохранения?»; пока он на экране, закрывает только «Закрыть без сохранения».
  //   Esc — самому верхнему слою: выдвижная панель дерева, окна и меню берут его раньше. Фокус не
  //   уходит под фон: Tab — по кругу целей панели, фокус на списке под панелью или на элементе под
  //   фоном (после щелчка по шапке) возвращается в панель; уже SPLIT_MIN — `stack`.
  // Фокус переходит только при открытии записи или формы (и при смене записи — `entry`): в `stack`
  // и `overlay` — на первый заголовок карточки, при возврате «←» (закрытии панели) — на нажатую
  // строку списка, иначе на строку записи (`[data-entry="<entry>"]` в списке), иначе на сам список.
  // Смена раскладки при открытой карточке (поворот, изменение окна) фокус не трогает, а прокрутку —
  // только если фокус не в карточке. Атрибут `data-layout` — текущая раскладка (для e2e).
  import { onDestroy, tick, untrack, type Snippet } from 'svelte';
  import { SPLIT_MIN } from '../lib/layout';
  import { go, onCancel, parse, shownEntryId } from '../lib/router';
  import { observeWidth } from '../lib/viewport.svelte';

  interface Props {
    /** Карточка (запись из адреса или форма) открыта. */
    open: boolean;
    /** Возврат к списку в `stack`: адрес журнала без записи и его название. */
    back: { href: string; label: string };
    mode?: 'auto' | 'overlay';
    /** Имя панели `overlay` для чтения с экрана (по умолчанию — `back.label`). */
    label?: string;
    /** Карточку закрыли («←» в `stack`; фон или Esc панели `overlay`). */
    onclose?: () => void;
    /** В панели `overlay` есть несохранённое (сравнение — у журнала, с тем, что сохранено в записи):
     * закрытие сначала спрашивает. */
    dirty?: () => boolean;
    /** id открытой записи: смена — новая карточка; строка списка помечена `data-entry` с ним. */
    entry?: string | null;
    list: Snippet;
    detail: Snippet;
  }

  let { open, back, mode = 'auto', label, onclose, dirty, entry = null, list, detail }: Props = $props();

  const uid = $props.id();

  /** Ширина области; 0 — ещё не измерена (первое значение приходит до отрисовки). */
  let width = $state(0);
  let layout = $derived<'split' | 'stack' | 'overlay'>(
    width < SPLIT_MIN ? 'stack' : mode === 'overlay' ? 'overlay' : 'split',
  );
  /** Карточка вместо списка. */
  let replaced = $derived(open && layout === 'stack');
  /** Карточка — панель поверх списка. */
  let sheet = $derived(open && layout === 'overlay');

  let root = $state<HTMLElement | null>(null);
  let listEl = $state<HTMLElement | null>(null);
  let detailEl = $state<HTMLElement | null>(null);
  let scrimEl = $state<HTMLElement | null>(null);
  let confirmEl = $state<HTMLElement | null>(null);
  /** Вопрос «Закрыть без сохранения?» в панели. */
  let confirming = $state(false);

  /** Показана ли карточка вместо списка (после замера ширины): переходы — по смене. */
  let shown = false;
  /** Показана ли карточка панелью поверх списка (после замера ширины). */
  let paneled = false;
  /** Предыдущие `open` и `entry` — что открыли заново. */
  let wasOpen = false;
  let lastEntry: string | null = null;
  /** Карточку открыли (или сменили запись), а фокус ещё не переведён. */
  let fresh = false;
  /** Запись, показанная вместо списка или панелью последней (строка для фокуса при возврате). */
  let shownEntry: string | null = null;
  /** Что вернуть при возврате к списку: прокрутка (`null` — список ушёл при смене раскладки),
   * нажатая строка, запись, открытая из списка. */
  let saved: { top: number | null; focus: HTMLElement | null; entry: string | null } | null = null;
  /** Элемент списка, к которому вернуть фокус при закрытии панели. */
  let returnTo: HTMLElement | null = null;
  /** Адрес открытой панели и как её закрыть: «назад» истории (открыта со списка — переходом,
   * добавившим запись истории) или заменой адреса; `at` — запись истории, на которой открыта. */
  let opened: { hash: string; pops: boolean; at: string } | null = null;
  /** Последняя смена адреса-хэша (`hashchange`): откуда пришли к открытой записи. */
  let lastChange: { from: string; to: string } | null = null;
  /** Панель закрывается («назад» истории ещё не отработал): повторные закрытия не действуют. */
  let closing = false;
  /** Где был фокус, когда появился вопрос «Закрыть без сохранения?» («Продолжить правку» — туда). */
  let beforeAsk: HTMLElement | null = null;
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
  /** Элемент списка в фокусе, иначе последний нажатый в нём. */
  const listFocus = (): HTMLElement | null => {
    const a = document.activeElement;
    return inList(a) ? a : inList(activated) ? activated : null;
  };

  /** Строка записи `id` в списке — её ссылка или кнопка (иначе сама строка). */
  function rowTarget(id: string | null): HTMLElement | null {
    const row = id ? listEl?.querySelector<HTMLElement>(`[data-entry="${CSS.escape(id)}"]`) : null;
    if (!row) return null;
    return (row.matches('a[href], button') ? row : row.querySelector<HTMLElement>('a[href], button')) ?? row;
  }

  // До скрытия списка — его прокрутка и элемент, к которому вернуть фокус. Открытие записи —
  // запоминается всё; смена раскладки при открытой карточке — только запись.
  $effect.pre(() => {
    const r = replaced && width > 0;
    const o = open;
    const e = entry;
    untrack(() => {
      if (!r || shown) return;
      const opening = fresh || (o && (!wasOpen || isNewRecord(e)));
      saved = opening ? { top: scroller().scrollTop, focus: listFocus(), entry: e } : { top: null, focus: null, entry: e };
    });
  });

  $effect(() => {
    const o = open;
    const e = entry;
    const measured = width > 0;
    const r = replaced && measured;
    const p = sheet && measured;
    untrack(() => {
      if (o && (!wasOpen || isNewRecord(e))) fresh = true;
      // вопрос «Закрыть без сохранения?» — о показанной карточке: закрыли её или открыли другую
      if (!o || isNewRecord(e)) confirming = false;
      if (!o) fresh = false;
      wasOpen = o;
      lastEntry = e;
      if (p !== paneled) {
        paneled = p;
        if (p) {
          // панель поверх списка: фокус вернётся на нажатую строку
          returnTo = listFocus();
          openSheet();
          void showSheet(fresh);
          fresh = false;
        } else {
          confirming = false;
          closing = false;
          opened = null;
          if (!o) void closeSheet();
          // иначе область сузилась при открытой панели — карточка вместо списка (ниже)
          else returnTo = null;
        }
      } else if (p && fresh) {
        // другая запись в панели (ссылка из карточки): фокус на её заголовке, при закрытии — на
        // её строку, а не на строку первой записи (`closeSheet`)
        openSheet();
        void showSheet(true);
        fresh = false;
      }
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

  /** Фокус на первый заголовок карточки (без прокрутки). */
  function focusHeading(d: HTMLElement) {
    const h = d.querySelector<HTMLElement>('h1, h2, h3');
    if (h) {
      if (!h.hasAttribute('tabindex')) h.tabIndex = -1;
      h.focus({ preventScroll: true });
    }
  }

  /** Карточка вместо списка: при открытии — её верх в видимой области под шапкой и фокус на
   * заголовке; при смене раскладки — фокус на месте, прокрутка — если фокус не в карточке, а
   * фокус со скрытого списка (или потерянный) — на заголовок карточки. */
  async function showCard(opening: boolean) {
    shownEntry = entry;
    await tick();
    const d = detailEl;
    if (!d) return;
    if (!opening && focusInDetail()) return;
    const top = d.getBoundingClientRect().top;
    const margin = parseFloat(getComputedStyle(d).scrollMarginTop) || 0;
    if (top < margin || top > window.innerHeight - 120) d.scrollIntoView({ block: 'start' });
    const a = document.activeElement;
    const lost = !a || a === document.body || inList(a);
    if (!opening && !lost) return;
    focusHeading(d);
  }

  /** Новая запись в панели: как её закрыть (пришли к ней со списка переходом — «назад» истории) и
   * поля без правок. */
  function openSheet() {
    const h = location.hash;
    opened = {
      hash: h,
      pops: !!lastChange && lastChange.from === back.href && lastChange.to === h,
      at: shownEntryId(),
    };
    confirming = false;
    closing = false;
  }

  /** Панель поверх списка: при открытии (и смене записи) — фокус на заголовке карточки; при смене
   * раскладки — фокус на месте, если он уже в карточке. Прокрутка списка не меняется. */
  async function showSheet(opening: boolean) {
    shownEntry = entry;
    await tick();
    const d = detailEl;
    if (!d) return;
    if (!opening && focusInDetail()) return;
    focusHeading(d);
  }

  /** Панель закрыта: фокус — на строку показанной записи (нажатый в ней элемент, иначе её ссылку
   * или кнопку), без строки — на нажатый элемент списка («Новая задача»); если фокус уже ушёл сам
   * (щелчок по шапке) — не трогать. Прокрутка — только если строка вне окна. Запись бывает в списке
   * не одной строкой (доска задач: плитка шага и он же в раскрытии общей) — нажатый элемент строки
   * этой записи (ближайший `[data-entry]`) сильнее первой такой строки. */
  async function closeSheet() {
    const back0 = returnTo;
    returnTo = null;
    await tick();
    const a = document.activeElement;
    if (a && a !== document.body) return;
    const pressed = back0?.isConnected ? back0 : null;
    const own = !!shownEntry && pressed?.closest<HTMLElement>('[data-entry]')?.dataset.entry === shownEntry;
    const row = shownEntry ? listEl?.querySelector<HTMLElement>(`[data-entry="${CSS.escape(shownEntry)}"]`) : null;
    const t = own ? pressed : row ? (pressed && row.contains(pressed) ? pressed : rowTarget(shownEntry)) : pressed;
    if (!t) return;
    if (!t.matches('a[href], button, [tabindex]')) t.tabIndex = -1;
    t.focus({ preventScroll: true });
    const r = t.getBoundingClientRect();
    if (r.bottom <= 0 || r.top >= window.innerHeight) t.scrollIntoView({ block: 'nearest' });
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
    const target = rowTarget(shownEntry);
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

  /** Закрыть панель: `onclose` (форма без записи) и адрес журнала без записи — «назад» истории,
   * если к записи пришли со списка, иначе заменой адреса: закрытие записи истории не добавляет, и
   * «назад» браузера закрытую панель снова не открывает. «Назад» асинхронный: до смены адреса
   * панель ещё видна, и повторный Esc или щелчок не уводят назад второй раз (`closing`). */
  function close() {
    if (closing) return;
    closing = true;
    confirming = false;
    onclose?.();
    const h = location.hash;
    if (h === back.href) return;
    // «назад» — только с той записи истории, на которой панель открыта: после отката, который не
    // пришёл (роутер поставил адрес на другую запись), «назад» увёл бы мимо списка
    if (opened?.pops && opened.hash === h && opened.at === shownEntryId()) {
      history.back();
      // запасной путь: «назад» без смены адреса (соседняя запись с тем же адресом — история до
      // номеров роутера) не даёт `hashchange` — тогда заменой адреса, «идёт закрытие» не залипает
      window.addEventListener(
        'popstate',
        () => {
          if (location.hash !== h) return;
          closing = false;
          go(parse(back.href), { replace: true });
        },
        { once: true },
      );
    } else go(parse(back.href), { replace: true });
  }

  /** «← <журнал>» в `stack`: несохранённое — сначала тот же вопрос, что у панели; без него — обычный
   * переход ссылки (`onclose` и адрес журнала). */
  function backClick(e: MouseEvent) {
    if (dirty?.()) {
      e.preventDefault();
      void requestClose();
      return;
    }
    onclose?.();
  }

  /** «Закрыть без сохранения»: в `stack` — как ссылка «←» (новая запись истории), в панели — `close`. */
  function discard() {
    if (layout !== 'stack') return close();
    confirming = false;
    onclose?.();
    go(parse(back.href));
  }

  // переход отменила проверка ухода (несохранённая правка): адрес прежний — закрытие не идёт
  onDestroy(onCancel(() => (closing = false)));

  /** Закрыть по фону, Esc или «Закрыть»: несохранённое — сначала вопрос в панели; пока он на
   * экране, фон и «Закрыть» только возвращают к нему фокус (закрывает «Закрыть без сохранения»). */
  async function requestClose() {
    if (closing) return;
    if (!confirming && !dirty?.()) {
      close();
      return;
    }
    if (!confirming) {
      const a = document.activeElement;
      beforeAsk = a instanceof HTMLElement && detailEl?.contains(a) ? a : null;
      confirming = true;
    }
    await tick();
    confirmEl?.querySelector<HTMLElement>('button')?.focus();
  }

  /** «Продолжить правку»: вопрос скрыт, фокус — где был (поле с текстом), иначе на панели. */
  async function keepEditing() {
    confirming = false;
    const back0 = beforeAsk;
    beforeAsk = null;
    await tick();
    (back0?.isConnected ? back0 : detailEl)?.focus();
  }

  // Esc — самому верхнему слою. Этот обработчик — на `window` после обработчика оболочки (она
  // смонтирована раньше): открытая выдвижная панель дерева закрывается первой и отмечает Esc
  // обработанным; окна и меню (поиск Ctrl+K, контекстное меню, меню аккаунта) — тоже их. Иначе Esc
  // закрывает панель, где бы ни был фокус (Safari и Firefox на macOS не фокусируют кнопку по
  // щелчку); при вопросе «Закрыть без сохранения?» Esc — «Продолжить правку».
  function onkeydown(e: KeyboardEvent) {
    if (!paneled || e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
    const other = e.target instanceof Element ? e.target.closest('[role="dialog"], [role="menu"]') : null;
    if (other && other !== detailEl) return;
    e.preventDefault();
    if (confirming) void keepEditing();
    else void requestClose();
  }

  /** Элемент закрыт фоном или самой панелью. Список журнала под открытой панелью закрыт всегда (по
   * DOM: и строка, заехавшая под липкую шапку); прочее (неподвижное дерево) — по точке в середине
   * элемента внутри окна: выше фона лежат шапка, выдвижная панель, окна и меню. */
  function underSheet(t: HTMLElement): boolean {
    if (listEl?.contains(t)) return true;
    const r = t.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const x = Math.min(Math.max(r.left + r.width / 2, 0), window.innerWidth - 1);
    const y = Math.min(Math.max(r.top + r.height / 2, 0), window.innerHeight - 1);
    const hit = document.elementFromPoint(x, y);
    if (!hit) return false;
    if (hit === t || t.contains(hit) || hit.contains(t)) return false;
    return hit === scrimEl || !!detailEl?.contains(hit);
  }

  // Фокус не уходит под фон (WCAG 2.2 SC 2.4.11): после щелчка по шапке Tab мог бы пройти по
  // доске и дереву под фоном — такой фокус возвращается в панель, на её заголовок. Шапка,
  // выдвижная панель дерева, окна и меню выше фона — фокус в них остаётся. Проверка — после
  // прокрутки к элементу (кадр спустя).
  function onfocusin(e: FocusEvent) {
    const t = e.target;
    if (!paneled || !(t instanceof HTMLElement) || detailEl?.contains(t)) return;
    requestAnimationFrame(() => {
      if (paneled && document.activeElement === t && detailEl && underSheet(t)) focusHeading(detailEl);
    });
  }

  function onhashchange(e: HashChangeEvent) {
    lastChange = { from: new URL(e.oldURL).hash, to: new URL(e.newURL).hash };
    // адрес сменился — закрытие состоялось (или ушли со страницы): повторное снова разрешено
    closing = false;
  }

  const FOCUSABLE = 'a[href], button, input, select, textarea, summary, [tabindex]';

  /** Tab в панели — по кругу её целей: фокус не уходит на список под фоном. */
  function trap(e: KeyboardEvent) {
    if (!paneled || e.key !== 'Tab') return;
    const d = detailEl;
    if (!d) return;
    const f = [...d.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
      (x) => x.tabIndex >= 0 && !x.matches(':disabled') && x.getClientRects().length > 0,
    );
    if (!f.length) {
      e.preventDefault();
      return;
    }
    const a = document.activeElement;
    // позиция фокуса среди целей; фокус на заголовке или самой панели — «между» целями
    let i = a && d.contains(a) ? f.indexOf(a as HTMLElement) : -2;
    if (i === -1) {
      const next = f.findIndex((x) => !!(a!.compareDocumentPosition(x) & Node.DOCUMENT_POSITION_FOLLOWING));
      i = (next === -1 ? f.length : next) - 0.5;
    }
    if (e.shiftKey ? i === -2 || i <= 0 : i === -2 || i >= f.length - 1) {
      e.preventDefault();
      f[e.shiftKey ? f.length - 1 : 0].focus();
    }
  }
</script>

<svelte:window {onkeydown} {onhashchange} />
<svelte:document {onfocusin} />

<div class="jsplit" bind:this={root} data-layout={layout} use:observeWidth={(w) => (width = w)}>
  <div class="jsplit-grid" class:open class:sheet-mode={mode === 'overlay'}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="jsplit-list" bind:this={listEl} hidden={replaced} onclickcapture={remember}>
      {@render list()}
    </div>
    {#if open}
      {#if sheet}
        <!-- фон панели: щелчок закрывает её (с клавиатуры — Esc) -->
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="jsplit-scrim" aria-hidden="true" bind:this={scrimEl} onclick={requestClose}></div>
      {/if}
      <!-- панель — окно (`role="dialog"`, `tabindex="-1"`: щелчок внутри не теряет фокус); рядом
           со списком и вместо него — обычный блок -->
      <!-- svelte-ignore a11y_no_static_element_interactions, a11y_no_noninteractive_tabindex -->
      <div
        class="jsplit-detail"
        class:jsplit-sheet={sheet}
        bind:this={detailEl}
        role={sheet ? 'dialog' : undefined}
        aria-modal={sheet ? 'true' : undefined}
        aria-label={sheet ? (label ?? back.label) : undefined}
        tabindex={sheet ? -1 : undefined}
        onkeydown={trap}
      >
        {#if layout === 'stack'}
          <a class="jsplit-back" href={back.href} onclick={backClick}>← {back.label}</a>
        {:else if sheet}
          <div class="jsplit-bar"><button type="button" onclick={requestClose}>Закрыть</button></div>
        {/if}
        {#if confirming && (layout === 'stack' || sheet)}
          <div
            class="jsplit-confirm warn"
            role="alertdialog"
            aria-label="Несохранённый текст"
            aria-describedby={`${uid}-ask`}
            bind:this={confirmEl}
          >
            <p id={`${uid}-ask`}>В карточке есть несохранённый текст. Закрыть без сохранения?</p>
            <div class="row">
              <button type="button" onclick={keepEditing}>Продолжить правку</button>
              <button type="button" class="danger" onclick={discard}>Закрыть без сохранения</button>
            </div>
          </div>
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
  /* панель поверх списка (`overlay`): под шапкой у правого края, фон — до низа окна; ниже
     выдвижной панели дерева (24–25, Shell.svelte), меню (35–41) и окон (50+) */
  .jsplit-scrim {
    position: fixed;
    inset: var(--header-h) 0 0 0;
    z-index: 22;
    background: rgb(0 0 0 / 0.28);
  }
  .jsplit-sheet {
    position: fixed;
    top: var(--header-h);
    right: 0;
    bottom: 0;
    z-index: 23;
    width: min(480px, 100%);
    overflow: auto;
    overscroll-behavior: contain;
    padding: 12px max(16px, var(--safe-right)) calc(16px + var(--safe-bottom)) 16px;
    background: var(--bg);
    border-left: 1px solid var(--border);
    box-shadow: -8px 0 24px rgb(0 0 0 / 0.18);
  }
  .jsplit-sheet:focus {
    outline: none;
  }
  .jsplit-bar {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 4px;
  }
  .jsplit-confirm {
    margin-bottom: 8px;
  }
  .jsplit-confirm p {
    margin: 0 0 8px;
  }
  /* ширина — SPLIT_MIN (layout.ts) */
  @container jsplit (min-width: 840px) {
    .jsplit-grid.open:not(.sheet-mode) {
      display: grid;
      grid-template-columns: minmax(0, 1fr) max(380px, 44%);
      gap: 16px;
      align-items: start;
    }
    .jsplit-grid.open:not(.sheet-mode) > .jsplit-detail {
      position: sticky;
      top: calc(var(--header-h) + 16px);
      max-height: calc(var(--app-h) - var(--header-h) - 32px);
      overflow: auto;
      overscroll-behavior: contain;
    }
  }
</style>
