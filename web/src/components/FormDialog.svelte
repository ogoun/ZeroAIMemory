<script lang="ts" module>
  /** Последняя нажатая кнопка или ссылка (не в окне формы): при касании и в Safari нажатая кнопка
   * фокус не получает, а вернуть его при закрытии окна нужно на неё (как `activated` у
   * `JournalSplit`). Нажатие старше трёх секунд (чанк окна мог грузиться) — не то, что открыло окно. */
  let pressed: { el: HTMLElement; at: number } | null = null;
  if (typeof document !== 'undefined') {
    document.addEventListener(
      'click',
      (e) => {
        const t = e.target instanceof Element ? e.target.closest<HTMLElement>('button, a[href], [role="button"]') : null;
        if (t && !t.closest('[data-form-dialog]')) pressed = { el: t, at: Date.now() };
      },
      true,
    );
  }
  function lastPressed(): HTMLElement | null {
    const p = pressed;
    return p && Date.now() - p.at < 3000 && p.el.isConnected ? p.el : null;
  }
</script>

<script lang="ts">
  // Окно формы (M7.3, спецификация §6.14 п. 1, решения 80–82, 93): один каркас для форм по кнопке
  // (создание, короткие действия с полями, подтверждения) на bits-ui `Dialog` (чанк `dialog-content`).
  // - Шапка: «✕» (имя «Закрыть»), заголовок (`Dialog.Title`, h2), под ним — запись, которую правят
  //   (`subject`); тело — `description`, поля (`children`), итог (`notice`), ошибка (`role="alert"`,
  //   над кнопками); кнопки — основное действие, затем «Отмена», справа (`danger` — красная
  //   основная), дополнительные (`extra`) — слева от них. При `busy` и пока идёт отправка кнопки
  //   выключены. Enter в однострочном поле и Ctrl+Enter (⌘+Enter) в многострочном — отправка.
  // - Фокус при открытии — на поле `[data-autofocus]` (`data-select` — ещё и выделить текст), иначе
  //   на первом поле; у окна-подтверждения без полей (`confirm`) — на «Отмена», если действие
  //   `danger` (наименее разрушительное, как у `alertdialog` WAI-ARIA APG: Enter не удаляет сразу),
  //   иначе на основной кнопке. Tab — по кругу внутри окна (ловушка фокуса bits-ui). При закрытии
  //   фокус — туда, куда скажет владелец (`refocus`), иначе на элемент, открывший окно; если фокус
  //   уже ушёл сам (карточка созданной записи) — не трогать.
  // - Несохранённое (`dirty()`): Esc, щелчок по фону, «Отмена», «✕» и системная «Назад» на телефоне
  //   не закрывают окно, а показывают внутри него вопрос `LEAVE_EDIT` с «Продолжить правку» (фокус —
  //   на ней, текст вопроса — её `aria-describedby`: вопрос объявляется один раз, живой области нет)
  //   и «Закрыть без сохранения»; Esc при вопросе — продолжить правку. Пока окно открыто, оно стоит
  //   в стеке проверок ухода (`pushGuard`): переход по адресу спрашивает один раз, текстом формы.
  // - Ширина 520 px (`wide` — 720 px), высота — до 90 % окна: прокручивается тело, шапка и кнопки
  //   закреплены. Телефон (< 720 px) — окно формы во весь экран: основное действие — в закреплённой
  //   шапке («✕ · Заголовок · Добавить»), нижнего ряда кнопок нет (кроме `extra`), отступы — с
  //   безопасными областями; запись истории слоя (`pushLayer`) — «Назад» закрывает окно, а не
  //   меняет адрес. Окно-подтверждение на телефоне — прежний `.dialog` во всю ширину, без слоя.
  // - Ссылки `#/…` внутри окна (кандидаты-дубли) — переходом роутера: запись слоя заменяется
  //   адресом, «Назад» после него возвращает туда, где окно открыли.
  import { Dialog } from 'bits-ui';
  import { tick, untrack, type Snippet } from 'svelte';
  import { focusNow } from '../lib/focus';
  import { LEAVE_EDIT } from '../lib/guards';
  import { go, parse, pushGuard, pushLayer, type LayerHandle } from '../lib/router';
  import { MEDIA } from '../lib/layout';

  interface Props {
    open: boolean;
    title: string;
    /** Пояснение под заголовком. */
    description?: string;
    /** Запись, которую правит окно (на телефоне окно закрывает карточку — «что я правлю»). */
    subject?: string;
    /** Подпись основной кнопки. */
    submit: string;
    danger?: boolean;
    busy?: boolean;
    /** Ошибка (сервер, проверка формы) — внутри окна, над кнопками; окно остаётся открытым. */
    error?: string;
    /** Итог внутри окна (не ошибка). */
    notice?: string;
    wide?: boolean;
    /** Окно-подтверждение без полей: на телефоне не во весь экран. */
    confirm?: boolean;
    /** В форме есть несохранённое. */
    dirty?: () => boolean;
    /** Куда перевести фокус при закрытии после удавшейся отправки (окно закрыто владельцем во время
     * `onsubmit`); `null` — на элемент, открывший окно. */
    refocus?: () => Element | null | undefined;
    /** Отправка; исключение — текст ошибки в окне, окно остаётся открытым. */
    onsubmit: () => void | Promise<void>;
    /** Окно закрыли (кнопкой, Esc, фоном, «Назад», «Закрыть без сохранения»): владелец ставит
     * `open = false`. */
    onclose: () => void;
    children?: Snippet;
    extra?: Snippet;
  }

  let {
    open,
    title,
    description,
    subject,
    submit,
    danger = false,
    busy = false,
    error = '',
    notice = '',
    wide = false,
    confirm = false,
    dirty,
    refocus,
    onsubmit,
    onclose,
    children,
    extra,
  }: Props = $props();

  const uid = $props.id();

  let content = $state<HTMLElement | null>(null);
  let formEl = $state<HTMLFormElement | null>(null);
  let keepEl = $state<HTMLButtonElement | null>(null);
  let errorEl = $state<HTMLElement | null>(null);
  /** Вопрос о несохранённом на экране. */
  let asking = $state(false);
  /** Идёт отправка (`onsubmit` ещё не ответил). */
  let pending = $state(false);
  /** Исключение `onsubmit`. */
  let thrown = $state('');
  let shownError = $derived(error || thrown);
  let locked = $derived(busy || pending);

  /** Переход по адресу уже спросил и человек согласился уйти: окно не спрашивает снова. */
  let released = false;
  /** Элемент, открывший окно (фокус при закрытии). */
  let opener: HTMLElement | null = null;
  /** Запись истории слоя окна во весь экран (телефон). */
  let layer: LayerHandle | null = null;
  /** Окно во весь экран (телефон, форма с полями): при открытии — по ширине окна. */
  let full = false;
  /** Адрес при открытии: отправка, сменившая адрес (карточка созданной записи), фокус на кнопку,
   * открывшую окно, не возвращает — его переводит журнал на заголовок карточки. */
  let hashAtOpen = '';

  const unsaved = () => !released && !!dirty?.();

  // Элемент, открывший окно, — до перерисовки: окно ещё не смонтировано и фокус не перевело.
  let wasOpen = false;
  $effect.pre(() => {
    const o = open;
    untrack(() => {
      if (o && !wasOpen) {
        // нажатая кнопка (при касании и в Safari она фокус не получает), иначе элемент в фокусе
        const a = document.activeElement;
        opener = lastPressed() ?? (a instanceof HTMLElement && a !== document.body ? a : null);
      }
      wasOpen = o;
    });
  });

  // Открытие и закрытие: проверка ухода, слой истории, фокус при закрытии.
  $effect(() => {
    if (!open) return;
    untrack(opened);
    return () => untrack(closed);
  });

  let offGuard: (() => void) | null = null;

  function opened() {
    asking = false;
    pending = false;
    thrown = '';
    released = false;
    hashAtOpen = location.hash;
    if (opener && content?.contains(opener)) opener = null;
    // переход во время отправки — её итог (владелец открывает созданное), не уход с набранным
    offGuard = pushGuard({ dirty: () => open && !pending && unsaved(), release: () => (released = true) });
    full = !confirm && window.matchMedia(MEDIA.phone).matches;
    if (full) layer = pushLayer(fromBack);
  }

  function closed() {
    offGuard?.();
    offGuard = null;
    layer?.drop();
    layer = null;
    asking = false;
    // окно закрыл владелец во время отправки — удавшееся действие: фокус — куда он скажет
    // (`refocus`, как `Ask.refocus` у `Prompt`); закрыли иначе — на элемент, открывший окно
    const bySubmit = pending;
    // после перерисовки владельца: элемент, открывший окно, мог уйти вместе с записью
    const back = opener;
    opener = null;
    setTimeout(() => restoreFocus(back, bySubmit), 0);
  }

  function restoreFocus(back: HTMLElement | null, bySubmit: boolean) {
    const a = document.activeElement;
    // фокус ушёл сам (заголовок карточки созданной записи) — не трогать
    if (a && a !== document.body && a.isConnected) return;
    const r = bySubmit ? refocus?.() : null;
    if (r && focusNow(r)) return;
    // отправка открыла другую запись: фокус переведёт её карточка, а не кнопка под окном
    if (bySubmit && location.hash !== hashAtOpen) return;
    if (back?.isConnected) focusNow(back);
  }

  /** «Назад» на телефоне: запись слоя уже снята. Несохранённое — вопрос в окне, «Продолжить правку»
   * вернёт запись слоя. */
  function fromBack() {
    layer = null;
    if (!open) return;
    if (unsaved()) void ask();
    else onclose();
  }

  async function ask() {
    asking = true;
    await tick();
    keepEl?.focus();
  }

  /** «✕», «Отмена», Esc, фон: несохранённое — сначала вопрос в окне. */
  function requestClose() {
    if (pending) return;
    if (asking) {
      void keep();
      return;
    }
    if (unsaved()) void ask();
    else onclose();
  }

  /** «Продолжить правку»: вопрос скрыт, слой истории (если «Назад» его снял) — снова, фокус — на
   * первом поле с набранным (последнее поле, где был фокус, ушло вместе с вопросом). */
  async function keep() {
    asking = false;
    if (full && !layer && open) layer = pushLayer(fromBack);
    await tick();
    firstField()?.focus();
  }

  /** «Закрыть без сохранения». */
  function discard() {
    asking = false;
    released = true;
    onclose();
  }

  async function send(e?: Event) {
    e?.preventDefault();
    if (locked || asking) return;
    pending = true;
    thrown = '';
    try {
      await onsubmit();
    } catch (err) {
      thrown = err instanceof Error ? err.message : 'Ошибка';
    } finally {
      // закрытие окна владельцем (в той же отправке) — раньше: оно видит «идёт отправка»
      await tick();
      pending = false;
    }
  }

  // ошибка появилась — на виду (на телефоне тело прокручивается, ошибка — в его конце)
  $effect(() => {
    if (!shownError) return;
    void tick().then(() => errorEl?.scrollIntoView({ block: 'nearest' }));
  });

  const FIELD = 'input:not([type="hidden"]):not(:disabled), select:not(:disabled), textarea:not(:disabled)';
  const firstField = (): HTMLElement | null =>
    content?.querySelector<HTMLElement>('[data-autofocus]') ?? content?.querySelector<HTMLElement>(`.fd-body ${FIELD}`) ?? null;

  function onOpenAutoFocus(e: Event) {
    e.preventDefault();
    const c = content;
    if (!c) return;
    let t: HTMLElement | null = null;
    if (confirm) t = c.querySelector<HTMLElement>(danger ? '[data-fd="cancel"]' : '[data-fd="submit"]');
    else t = firstField() ?? c.querySelector<HTMLElement>('[data-fd="submit"]:not([hidden])');
    t?.focus();
    if (t instanceof HTMLInputElement && t.hasAttribute('data-select')) t.select();
  }

  /** Ctrl+Enter (⌘+Enter) в многострочном поле — отправка (Enter однострочного — сама форма). */
  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && e.target instanceof HTMLTextAreaElement) {
      e.preventDefault();
      formEl?.requestSubmit();
    }
  }

  /** Ссылка приложения внутри окна — переход роутера (запись слоя заменяется адресом). */
  function onclick(e: MouseEvent) {
    if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest<HTMLAnchorElement>('a[href^="#"]') : null;
    if (!a || a.target) return;
    e.preventDefault();
    go(parse(a.getAttribute('href') ?? ''));
  }
</script>

<Dialog.Root {open}>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content
      bind:ref={content}
      class="panel dialog fd{confirm ? ' fd-confirm' : ' fd-form'}{wide ? ' fd-wide' : ''}"
      data-form-dialog=""
      {onOpenAutoFocus}
      onCloseAutoFocus={(e) => e.preventDefault()}
      onEscapeKeydown={(e) => {
        e.preventDefault();
        requestClose();
      }}
      onInteractOutside={(e) => {
        e.preventDefault();
        requestClose();
      }}
    >
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
      <form class="fd-frame" bind:this={formEl} onsubmit={send} {onkeydown} {onclick} aria-busy={locked}>
        <header class="fd-head">
          <!-- «✕»: на телефоне — слева («✕ · Заголовок · Добавить», как листы iOS и Material), у окна
               по центру — справа; видна одна (вторая — `display: none`, вне порядка Tab) -->
          {#if !confirm}
            <button type="button" class="fd-x fd-x-start" aria-label="Закрыть" onclick={requestClose}>✕</button>
          {/if}
          <div class="fd-titles">
            <Dialog.Title level={2} class="fd-title">{title}</Dialog.Title>
            {#if subject}<p class="fd-subject muted">{subject}</p>{/if}
          </div>
          {#if !confirm}
            <!-- основное действие на телефоне — в шапке (нижнего ряда кнопок там нет); пока на экране
                 вопрос о несохранённом — выключено, как и нижнее -->
            <button type="submit" class="fd-head-submit {danger ? 'danger-fill' : 'primary'}" disabled={locked || asking}>{submit}</button>
            <button type="button" class="fd-x fd-x-end" aria-label="Закрыть" onclick={requestClose}>✕</button>
          {/if}
        </header>
        {#if asking}
          <div class="fd-ask warn" data-testid="fd-ask">
            <p id="{uid}-ask">{LEAVE_EDIT}</p>
            <div class="row">
              <button type="button" bind:this={keepEl} aria-describedby="{uid}-ask" onclick={keep}>Продолжить правку</button>
              <button type="button" class="danger" onclick={discard}>Закрыть без сохранения</button>
            </div>
          </div>
        {/if}
        <div class="fd-body">
          {#if description}<Dialog.Description class="fd-desc muted">{description}</Dialog.Description>{/if}
          {@render children?.()}
          {#if notice}<p class="fd-notice" role="status">{notice}</p>{/if}
          {#if shownError}<p class="error fd-error" role="alert" bind:this={errorEl}>{shownError}</p>{/if}
        </div>
        <footer class="fd-foot" class:fd-has-extra={!!extra}>
          {#if extra}<div class="fd-extra row">{@render extra()}</div>{/if}
          <div class="fd-main row">
            <button type="submit" class={danger ? 'danger-fill' : 'primary'} data-fd="submit" disabled={locked || asking}>{submit}</button>
            <button type="button" data-fd="cancel" onclick={requestClose} disabled={pending}>Отмена</button>
          </div>
        </footer>
      </form>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .fd-frame {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    min-height: 0;
  }
  .fd-head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
  }
  .fd-titles {
    flex: 1;
    min-width: 0;
  }
  :global(.fd-title) {
    margin: 0;
    font-size: 16px;
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  .fd-subject {
    margin: 2px 0 0;
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .fd-x {
    flex: none;
    padding: 2px 8px;
    border-color: transparent;
    background: transparent;
    line-height: 1.4;
  }
  .fd-head-submit,
  .fd-x-start {
    display: none;
    flex: none;
  }
  .fd-ask {
    margin: 12px 16px 0;
  }
  .fd-ask p {
    margin: 0 0 8px;
  }
  /* строки — по содержимому, сверху вниз: тело на телефоне растянуто на весь экран, а сетка по
     умолчанию (`align-content: normal`) делила бы свободное место между строками поровну */
  .fd-body {
    display: grid;
    align-content: start;
    gap: 10px;
    padding: 12px 16px;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .fd-body > :global(*) {
    min-width: 0;
  }
  /* абзацы тела — без своих полей: отступ между строками один (`gap`) */
  .fd-body > :global(p) {
    margin: 0;
  }
  :global(.fd-desc) {
    margin: 0;
  }
  .fd-notice {
    margin: 0;
  }
  .fd-error {
    margin: 0;
  }
  .fd-foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
  }
  .fd-extra {
    margin-right: auto;
  }
  /* поля формы окна: подпись (с пометкой «необязательно» в той же строке) над полем, поле во всю
     ширину; поле с припиской (дата и «UTC») — рядом с ней */
  .fd-body :global(label:not(.inline)) {
    display: block;
  }
  .fd-body :global(label:not(.inline) > :is(input:not([type='checkbox'], [type='radio']), select, textarea)) {
    display: block;
    width: 100%;
    margin-top: 4px;
  }
  .fd-body :global(label:not(.inline) > .row) {
    margin-top: 4px;
    flex-wrap: nowrap;
  }
  .fd-body :global(label:not(.inline) > .row > input) {
    flex: 1 1 auto;
    min-width: 0;
  }
  .fd-body :global(input:not([type='checkbox'], [type='radio'])),
  .fd-body :global(select),
  .fd-body :global(textarea) {
    max-width: 100%;
  }
  .fd-body :global(textarea) {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    resize: vertical;
  }
  .fd-body :global([aria-invalid='true']) {
    border-color: var(--danger);
  }
  @media (pointer: coarse) {
    .fd-x {
      min-width: 44px;
      min-height: 44px;
    }
  }
  /* телефон: окно формы во весь экран, основное действие — в шапке, нижнего ряда нет (кроме
     дополнительных кнопок); ширина — BP_TABLET (layout.ts) */
  @media (max-width: 719px) {
    :global(.dialog.fd-form) .fd-head-submit,
    :global(.dialog.fd-form) .fd-x-start {
      display: inline-flex;
      align-items: center;
      justify-content: center;
    }
    :global(.dialog.fd-form) .fd-x-end {
      display: none;
    }
    :global(.dialog.fd-form) .fd-head {
      align-items: center;
    }
    :global(.dialog.fd-form) .fd-main {
      display: none;
    }
    :global(.dialog.fd-form) .fd-foot:not(.fd-has-extra) {
      display: none;
    }
    :global(.dialog.fd-form) .fd-body {
      flex: 1 1 auto;
    }
  }
</style>
