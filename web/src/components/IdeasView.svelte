<script lang="ts">
  // «Идеи» (M5.7 §10; M6.5 §9): замыслы человека и агента. Вкладки по происхождению («Мои», «От
  // агента», «Все» — шаблон ARIA tabs, как у «Кода»), чипы статусов со счётчиками (`entries/counts`
  // с фильтрами ленты; по умолчанию «Активные»), «Группировать по статусу» — разделы в порядке
  // чипов, фильтры приоритета и тега — на сервере, курсор по 50; поиск по заголовку и тексту —
  // `POST /search kinds=[idea]`. Строка ленты — заголовок, приоритет, статус, дата и строка
  // решения разбора агента («В план → k:…», «Не хватает: …», «Отклонена: …», «Сделано: …»).
  // Карточка: «Разбор агента» (исход, разбор текстом с переносами, «Опирался на» — ссылки в журналы,
  // `file:`/`commit:` — текстом с «копировать», «Похожие», задача), «История разборов» (свёрнута),
  // у возвращённой — плашка «Агент просит дописать:» и «Изменить» (сохранение отправляет её на
  // разбор: «Отправлено на разбор»), у отклонённой — «Вернуть на разбор» и «Удалить» (в корзину,
  // с подтверждением). Кнопки M5.7: «Начать» (decide accept — задача и ссылка на неё; у идеи
  // Knowledge — выбор решения с предупреждением об общем), «Отложить» (дата UTC), «Отказаться»,
  // «Принять как свою» (у «от агента» «Начать» нет до неё), «Изменить» (PATCH с If-Match, 409 —
  // перечитать); формы — отдельный ленивый чанк `IdeaForm.svelte`, грузится при первом открытии.
  // Предлагает и разбирает идею агент (propose, review): у WebUI этих действий нет; у идеи со снятой
  // задачей открытый вопрос агента «вернуть» — пометка «Предложено вернуть» (§16, п. 6). Ленивый чанк.
  // Узкие экраны (M7.2, спецификация §5.2 п. 2): лента и карточка — `JournalSplit` (рядом или
  // карточка вместо ленты); «Группировать по статусу» в узкой области (< SPLIT_MIN) — аккордеон
  // `<details>` (открыт первый непустой раздел, число — в заголовке); чипы статусов — полоса;
  // фильтры (приоритет, тег, поиск) на телефоне — под кнопкой «Фильтры»; сбой загрузки чанка формы
  // — `LazyFail`. «Корзина» (M7.2, спецификация §5.9 п. 3) — вместо ленты удалённые идеи с
  // «Восстановить» (`JournalTrash`, ленивый чанк).
  //
  // Карточка и формы (M7.3, спецификация §6.14, решения 80–86): ряд решений — сразу под шапкой
  // карточки, до текста, разбора и истории (правило Г); «Новая идея» и решения с полями («Начать»
  // идеи Knowledge, «Отложить», «Отказаться», «Вернуть на разбор») — окна формы (`IdeaForm` в
  // `FormDialog`, правило Б), после «Записать» — карточка созданной; «Изменить» — правка в карточке
  // на месте (вид карточки сменяется формой, правило В). Несохранённое правки — `IdeaForm.dirty()`:
  // «← Идеи» спрашивает «Закрыть без сохранения?» (`JournalSplit`), уход по адресу — один вопрос
  // стека проверок ухода; окна спрашивают о своём сами. 409 правки и решения — идея перечитана,
  // набранное остаётся, нетронутое — из свежей идеи (`IdeaForm.rebase`), форма открыта (п. 5).
  import { onMount, tick, untrack, type Snippet } from 'svelte';
  import FormDialog from './FormDialog.svelte';
  import JournalSplit from './JournalSplit.svelte';
  import LazyFail from './LazyFail.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import TrashPanel from './TrashPanel.svelte';
  import {
    ApiError,
    adoptIdea,
    createEntry,
    decideIdea,
    deleteEntry,
    entryCounts,
    errorText,
    getEntry,
    isStaleEntry,
    listEntries,
    patchEntry,
    searchIdeas,
    spaceRef,
    type Entry,
    type EntryStatus,
    type IdeaDecision,
    type SearchItem,
    type Space,
  } from '../lib/api';
  import { copyText } from '../lib/clipboard';
  import {
    IDEA_CHIPS,
    IDEA_FORMS,
    IDEA_TABS as TABS,
    REVIEW_OUTCOME_TEXT,
    chipCount,
    decisionNotice,
    editNotice,
    groupByStatus,
    ideaButtons,
    olderReviews,
    originOf,
    priorityOf,
    refTarget,
    releasedOf,
    reviewCurrent,
    reviewLine,
    reviewNote,
    reviewOf,
    reviewerLabel,
    type DecisionOpts,
    type IdeaAction,
    type IdeaChip,
    type IdeaSubmit,
    type IdeaTab as Tab,
  } from '../lib/ideaButtons';
  import type { IdeaForm } from '../lib/ideaForms';
  import {
    AUTHOR_TEXT,
    IDEA_STATUSES,
    IDEA_STATUS_TEXT,
    JOURNAL_TITLE,
    PRIORITIES,
    PRIORITY_TEXT,
    dataStr as str,
    entryRef,
    fmtTime,
    type IdeaPriority,
  } from '../lib/journals';
  import { focusSoon } from '../lib/focus';
  import { rebaseText } from '../lib/formDraft';
  import { SPLIT_MIN } from '../lib/layout';
  import { guardLeave } from '../lib/leaveEdit';
  import { snippetParts } from '../lib/snippet';
  import { go, href } from '../lib/router';
  import { onTabKey } from '../lib/tabs';
  import { observeWidth, revealSelected, viewport } from '../lib/viewport.svelte';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    /** Пространства пользователя (`GET /spaces`): решения для «Начать» у идеи Knowledge. */
    spaces: Space[];
    /** Id пользователя сессии: его разборы подписаны «ваш агент» (M6.5 §7 «Общие решения»). */
    viewer: string;
    onexpired: () => void;
  }

  let { user, space, entry, spaces, viewer, onexpired }: Props = $props();

  const PAGE = 50;

  const uid = $props.id();

  const statusText = (s: EntryStatus | string | null | undefined) =>
    (IDEA_STATUS_TEXT as Record<string, string>)[s ?? ''] ?? s ?? '';
  const chipTitle = (c: IdeaChip) => (c === 'active' ? 'Активные' : c === 'all' ? 'Все' : IDEA_STATUS_TEXT[c]);
  const chipStatuses = (c: IdeaChip) => IDEA_CHIPS.find((x) => x.id === c)?.statuses ?? [];

  let tab = $state<Tab>('human');
  let tabEls = $state<HTMLButtonElement[]>([]);
  let chip = $state<IdeaChip>('active');
  /** Разделы по статусу в порядке чипов. */
  let grouped = $state(false);
  let priority = $state<'' | IdeaPriority>('');
  let tag = $state('');
  let query = $state('');
  /** Применённые тег и поиск (вкладка, статус и приоритет применяются сразу). */
  let applied = $state({ tag: '', query: '' });

  let items = $state<Entry[]>([]);
  let hits = $state<SearchItem[] | null>(null);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let seq = 0;
  /** Первая страница ленты не загрузилась: следующий переход повторит загрузку. */
  let failed = false;

  /** Ширина области журнала (CSS px): узкая (< SPLIT_MIN) — разделы аккордеоном. */
  let width = $state(0);
  let narrow = $derived(width > 0 && width < SPLIT_MIN);
  /** Телефон: фильтры — под кнопкой «Фильтры». */
  let phone = $derived(viewport.current === 'phone');
  let filtersOpen = $state(false);
  /** Заданных фильтров (приоритет, тег, поиск) — число на кнопке «Фильтры». */
  let activeFilters = $derived([priority, applied.tag, applied.query].filter(Boolean).length);
  /** Число идей по статусам (`entries/counts`) с фильтрами ленты; `null` — ещё не пришло. */
  let counts = $state<Partial<Record<EntryStatus, number>> | null>(null);
  let cseq = 0;

  let cur = $state<Entry | null>(null);
  /** Открытый вопрос-предложение идеи (`proposed`): текст агента и `data.task_space`. */
  let curQuestion = $state<Entry | null>(null);
  /** Ссылка на пространство задачи идеи (у идеи Knowledge задача может быть в решении). */
  let taskSpace = $state<string | null>(null);
  let curError = $state('');
  let eseq = 0;

  type Mode = 'none' | IdeaForm;
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');
  let ask = $state<Ask | null>(null);
  /** Ссылка разбора, которую только что скопировали («скопировано» у кнопки). */
  let copied = $state('');
  /** «Корзина» (M7.2, спецификация §5.9 п. 3): вместо ленты — удалённые идеи с «Восстановить». */
  let trash = $state(false);

  function toggleTrash() {
    trash = !trash;
    notice = '';
    // назад к ленте: восстановленные из корзины — в ней (и в счётчиках чипов)
    if (!trash) load(user, space, true);
  }

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  /** Пространство, для которого заданы фильтры: другое — фильтры и выдача сбрасываются. Эффект
   * перезапускается на каждый переход (свойства — из объекта маршрута): открытие идеи ленту не
   * перечитывает, а неудавшуюся загрузку — повторяет с прежними фильтрами. */
  let filtersFor = '';
  $effect(() => {
    const key = JSON.stringify([user, space]);
    if (key === filtersFor) {
      if (failed) untrack(() => load(user, space, true));
      return;
    }
    filtersFor = key;
    untrack(() => {
      seq++;
      items = [];
      hits = null;
      next = null;
      counts = null;
      tab = 'human';
      chip = 'active';
      priority = '';
      trash = false;
      reset();
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  /** Пространство идеи — Knowledge (задачу «Начать» можно поставить в своё решение). */
  let knowledge = $derived(space === 'knowledge' || spaces.find((s) => s.id === space)?.kind === 'knowledge');
  /** Решения той же базы, куда можно поставить задачу идеи Knowledge (свои, не чужие). */
  let targets = $derived(spaces.filter((s) => s.kind === 'solution' && s.role !== 'member'));

  /** Счётчики чипов — с фильтрами ленты (вкладка, приоритет, тег), без статуса. */
  async function loadCounts(u: string, s: string) {
    const my = ++cseq;
    try {
      const c = await entryCounts(u, s, {
        kind: 'idea',
        origin: tab === 'all' ? undefined : tab,
        priority: priority || undefined,
        tag: applied.tag || undefined,
      });
      if (my === cseq) counts = c;
    } catch (e) {
      // без счётчиков чипы работают; 401 — сессия кончилась
      if (my === cseq) counts = null;
      if (e instanceof ApiError && e.status === 401) onexpired();
    }
  }

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    if (reset) {
      failed = false;
      loadCounts(u, s);
    }
    const statuses = chipStatuses(chip);
    try {
      if (applied.query) {
        // «все» — явным перечнем: без статуса сервер берёт умолчание поиска (без declined, done)
        const r = await searchIdeas(u, s, applied.query, statuses.length ? statuses : IDEA_STATUSES);
        if (my !== seq) return;
        hits = r;
        items = [];
        next = null;
        return;
      }
      hits = null;
      const r = await listEntries(u, s, {
        kind: 'idea',
        status: statuses,
        origin: tab === 'all' ? undefined : tab,
        priority: priority || undefined,
        tag: applied.tag || undefined,
        cursor: reset ? null : next,
        limit: PAGE,
      });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) {
        items = [];
        hits = null;
        failed = true;
      }
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    curError = '';
    copied = '';
    if (!id) {
      cur = null;
      curQuestion = null;
      taskSpace = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'idea') {
        cur = null;
        curError = 'Это не идея.';
        return;
      }
      let q: Entry | null = null;
      let ts: string | null = null;
      if (e.question && !e.question.deleted && e.question.status === 'open') {
        q = await getEntry(u, e.question.id).catch(() => null);
      }
      if (e.task && !e.task.deleted) {
        // у идеи Knowledge задача может лежать в решении: адрес — по пространству задачи
        const t = knowledge ? await getEntry(u, e.task.id).catch(() => null) : null;
        const sp = t ? spaces.find((x) => x.id === t.space_id) : null;
        ts = sp ? spaceRef(sp) : space;
      }
      if (my !== eseq) return;
      cur = e;
      curQuestion = q;
      taskSpace = ts;
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  /** «Показать ещё»: на время загрузки кнопка остаётся (`aria-disabled`), фокус на ней цел;
   * страниц больше нет и кнопка ушла — фокус на первую догруженную запись. */
  async function more(e: MouseEvent) {
    if (loading) return;
    const btn = e.currentTarget as HTMLElement;
    const n = items.length;
    await load(user, space, false);
    await tick();
    const a = document.activeElement;
    const first = items[n]?.id;
    if (btn.isConnected || (a && a !== document.body) || !first) return;
    document.querySelector<HTMLElement>(`[data-idea="${CSS.escape(first)}"] button`)?.focus();
  }

  /** Открыть идею: новая запись истории — «назад» браузера возвращает к ленте (§5.1 п. 5). */
  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'ideas', entry: id });
  }

  /** Карточка показана (рядом с лентой или вместо неё). */
  let detailOpen = $derived(cur !== null || curError !== '');

  function selectTab(t: Tab) {
    if (tab === t) return;
    tab = t;
    load(user, space, true);
  }

  function pickChip(c: IdeaChip) {
    if (chip === c) return;
    chip = c;
    load(user, space, true);
  }

  /** Клавиатура tablist (`lib/tabs.ts`): ←/→ по кругу, Home, End; выбор следует за фокусом. */
  const ontabkey = (e: KeyboardEvent, i: number) => onTabKey(e, i, TABS.length, tabEls, (j) => selectTab(TABS[j].id));

  function apply(e?: SubmitEvent) {
    e?.preventDefault();
    applied = { tag: tag.trim(), query: query.trim() };
    load(user, space, true);
  }

  function reset() {
    tag = query = '';
    apply();
  }

  async function act(fn: () => Promise<string | void>, done: string | (() => string)) {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      mode = 'none';
      await load(user, space, true);
      if (target && target !== entry) select(target);
      else await open(user, target);
      notice = typeof done === 'string' ? done : done();
    } catch (e) {
      // 409: ревизия устарела или идею решили в другом месте — перечитать карточку
      if (e instanceof ApiError && e.status === 409 && cur) {
        const msg = errorText(e);
        await load(user, space, true);
        await open(user, cur.id);
        actionError = msg;
      } else {
        fail(e, (m) => (actionError = m));
      }
    } finally {
      busy = false;
    }
  }

  /** Решение по идее; `accept` — ещё и ссылка на созданную задачу. `form` — из окна формы. */
  function decide(c: Entry, decision: IdeaDecision, opts: DecisionOpts = {}, form = false) {
    let task = '';
    return (form ? formAct : act)(
      async () => {
        const r = await decideIdea(user, c.id, decision, opts);
        if (r.task) task = r.task.id;
      },
      () => decisionNotice(decision, entryRef('task', task), opts.horizon),
    );
  }

  /** Отправка формы (окна или правки в карточке): успех — форма закрыта, лента перечитана,
   * карточка — та же или созданной идеи (сразу: на телефоне запись слоя окна становится её
   * записью), итог строкой; после правки — фокус на заголовке карточки. Ошибка — в форме; 409 —
   * идея перечитана, набранное перенесено (`staleIdea`), форма открыта. */
  async function formAct(fn: () => Promise<string | void>, done: string | (() => string)) {
    if (busy) return;
    busy = true;
    actionError = '';
    let target: string | null;
    try {
      target = (await fn()) ?? cur?.id ?? null;
    } catch (e) {
      busy = false;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      if (e instanceof ApiError && e.status === 409 && cur) return staleIdea(cur.id, e);
      actionError = errorText(e);
      return;
    }
    const edited = mode === 'edit';
    mode = 'none';
    const other = !!target && target !== entry;
    if (other) select(target!);
    try {
      await load(user, space, true);
      if (!other) await open(user, target);
      notice = typeof done === 'string' ? done : done();
      if (edited) focusSoon(() => document.querySelector('article[aria-label="Карточка идеи"] h2'));
    } finally {
      busy = false;
    }
  }

  /** 409 формы: ревизия устарела или идею решили в другом месте — идея перечитана, правка
   * переносит набранное (`IdeaForm.rebase`), форма открыта; лента — заново. */
  async function staleIdea(id: string, e: unknown) {
    let fresh: Entry;
    try {
      fresh = await getEntry(user, id);
    } catch (err) {
      actionError = `${errorText(e)} ${errorText(err)}`;
      return;
    }
    const clash = ideaForm?.rebase(fresh) ?? [];
    cur = fresh;
    actionError = isStaleEntry(e) ? rebaseText(clash) : errorText(e);
    void load(user, space, true);
  }

  /** «Удалить» отклонённой идеи — в корзину, с подтверждением (M6.5 §6). */
  function askDelete(c: Entry) {
    ask = {
      title: 'Удалить идею?',
      message: `Идея «${c.title}» будет перемещена в корзину. Оставленная отклонённой, она служит образцом «уже обсуждали».`,
      confirm: 'Удалить',
      danger: true,
      run: async () => {
        try {
          await deleteEntry(user, c.id, c.rev);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) return onexpired();
          // 409: идею изменили или решили в другом месте — как act(): окно закрыть, ленту и
          // карточку перечитать, ошибку показать в карточке
          if (e instanceof ApiError && e.status === 409) {
            const msg = errorText(e);
            await load(user, space, true);
            await open(user, c.id);
            actionError = msg;
            return;
          }
          throw new Error(errorText(e));
        }
        // идея удалена: открытая форма закрывается с карточкой, переход не спрашивает
        mode = 'none';
        go({ name: 'journal', user, space, journal: 'ideas', entry: null }, { replace: true });
        await load(user, space, true);
        notice = `Идея «${c.title}» перемещена в корзину.`;
      },
    };
  }

  /** Решение для задачи по умолчанию: `data.task_space` вопроса-предложения или Knowledge. */
  function defaultTarget(): string {
    const ts = curQuestion ? str(curQuestion.data, 'task_space') : '';
    const sp = ts ? spaces.find((s) => s.id === ts) : null;
    return sp && sp.kind === 'solution' ? spaceRef(sp) : 'knowledge';
  }

  /** Номер открытия формы: повторное нажатие той же кнопки создаёт форму заново (поля сброшены). */
  let opened = $state(0);
  /** Загруженная открытая форма (`IdeaForm`): её несохранённое и перенос набранного при 409. */
  let ideaForm = $state<{ dirty: () => boolean; rebase: (fresh: Entry) => string[] } | null>(null);

  /** Несохранённое правки в карточке (вопрос панели и проверка ухода); окна спрашивают сами. */
  const unsaved = (): boolean => mode === 'edit' && !!ideaForm?.dirty();

  onMount(() => guardLeave(unsaved));

  /** Итог действия в карточке: фокус после закрытия окна (нажатая кнопка ушла вместе со статусом). */
  const cardNotice = () => document.querySelector<HTMLElement>('article[aria-label="Карточка идеи"] .notice');

  /** Окно формы открыто (создание, решения с полями). */
  let dialogMode = $derived(mode !== 'none' && mode !== 'edit');

  /** Открыть форму `m` (создание и решения с полями — окно, правка — в карточке). */
  function show(m: IdeaForm) {
    actionError = '';
    notice = '';
    opened++;
    mode = m;
  }

  /** Форму закрыли («Отмена», окно — «✕», Esc, фон): после правки фокус — на «Изменить». */
  function closeForm() {
    const edited = mode === 'edit';
    mode = 'none';
    actionError = '';
    if (edited) focusSoon(() => [...document.querySelectorAll<HTMLElement>('[aria-label="Решение по идее"] button')].find((b) => b.textContent?.trim() === 'Изменить'));
  }

  /** Кнопка карточки (`ideaButtons`): решение сразу, форма с полями или подтверждение удаления. */
  function press(a: IdeaAction, c: Entry) {
    const now = a === 'adopt' || (a === 'start' && !knowledge);
    if (a === 'adopt')
      act(async () => {
        await adoptIdea(user, c.id);
      }, 'Идея принята как своя.');
    else if (a === 'delete') askDelete(c);
    // «Начать» идеи решения — без формы; у идеи Knowledge — выбор пространства задачи
    else if (now) decide(c, 'accept');
    else show(a);
  }

  /** Заполненная форма (`IdeaForm.svelte`): создание, правка или решение. */
  function submitted(f: IdeaSubmit): Promise<void> | undefined {
    const c = cur;
    if ('create' in f) return formAct(async () => (await createEntry(user, space, { kind: 'idea', ...f.create })).id, 'Идея записана.');
    if (!c) return;
    if ('patch' in f) {
      // возвращённую, дописанную человеком, сервер возвращает на разбор (`returned → new`)
      let after: EntryStatus | null = c.status;
      return formAct(
        async () => {
          after = (await patchEntry(user, c.id, c.rev, f.patch)).status;
        },
        () => editNotice(c.status, after),
      );
    }
    return decide(c, f.decision, f.opts, true);
  }

  async function copyRef(ref: string, text: string) {
    copied = (await copyText(text)) ? ref : '';
  }

  const taskHref = (id: string, sp = taskSpace ?? space) => href({ name: 'journal', user, space: sp, journal: 'tasks', entry: id });
  const questionHref = (id: string) => href({ name: 'journal', user, space, journal: 'questions', entry: id });
  let filtered = $derived(!!(applied.tag || applied.query || chip !== 'active'));
  /** Число раздела: «показано из» счётчика сервера, если загружены не все. */
  const shownOf = (shown: number, total: number | undefined) =>
    total !== undefined && total !== shown ? `${shown} из ${total}` : String(shown);
</script>

<section class="journal ideas" use:observeWidth={(w) => (width = w)}>
  <!-- лента и карточка — рядом в широкой области, карточка вместо ленты в узкой (§5.2 п. 2) -->
  <JournalSplit
    open={detailOpen}
    back={{ href: href({ name: 'journal', user, space, journal: 'ideas', entry: null }), label: JOURNAL_TITLE.ideas }}
    onclose={closeForm}
    dirty={unsaved}
    {entry}
  >
    {#snippet list()}
      {@render ideasList()}
    {/snippet}
    {#snippet detail()}
      {@render ideaDetail()}
    {/snippet}
  </JournalSplit>
</section>

<Prompt bind:ask />

<!-- окна формы — «Новая идея» и решения с полями (§6.14, правило Б) -->
{#if dialogMode}{@render form(mode as IdeaForm)}{/if}

{#snippet ideasList()}
  <!-- «Корзина» — одна кнопка в обоих видах: фокус на ней переживает переключение -->
  <div class="row bar">
    {#if !trash}<button type="button" class="primary" onclick={() => show('create')}>Новая идея</button>{/if}
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  {#if trash}
    <TrashPanel {user} {space} kind="idea" {onexpired} />
  {:else}
    {@render liveIdeas()}
  {/if}
{/snippet}

{#snippet liveIdeas()}

  <div class="tabs" role="tablist" aria-label="Происхождение идей">
    {#each TABS as t, i (t.id)}
      <button
        type="button"
        role="tab"
        id={`${uid}-tab-${t.id}`}
        bind:this={tabEls[i]}
        aria-selected={tab === t.id}
        aria-controls={`${uid}-panel`}
        tabindex={tab === t.id ? 0 : -1}
        onclick={() => selectTab(t.id)}
        onkeydown={(e) => ontabkey(e, i)}>{t.title}</button
      >
    {/each}
  </div>

  <div role="tabpanel" id={`${uid}-panel`} aria-labelledby={`${uid}-tab-${tab}`}>
    <div class="chips">
      <!-- чипы — полоса: одна строка с прокруткой на телефоне и планшете (§5.1 п. 4) -->
      <div class="strip status-chips" role="group" aria-label="Статус идей" use:revealSelected={chip}>
        {#each IDEA_CHIPS as ch (ch.id)}
          <button type="button" class="schip" aria-pressed={chip === ch.id} data-chip={ch.id} onclick={() => pickChip(ch.id)}>
            {chipTitle(ch.id)}{#if counts && !applied.query}{' '}<span class="count">{chipCount(ch.id, counts)}</span>{/if}
          </button>
        {/each}
      </div>
      <div class="row">
        <label class="row small"><input type="checkbox" bind:checked={grouped} /> Группировать по статусу</label>
        {#if phone}
          <button
            type="button"
            class="filters-toggle"
            aria-expanded={filtersOpen}
            aria-controls={`${uid}-filters`}
            onclick={() => (filtersOpen = !filtersOpen)}
            >Фильтры{#if activeFilters}{' '}<span class="count">{activeFilters}</span>{/if}</button
          >
        {/if}
      </div>
    </div>
    <!-- на телефоне — под кнопкой «Фильтры» (§5.2 п. 2) -->
    <form id={`${uid}-filters`} class="filters row" hidden={phone && !filtersOpen} onsubmit={apply} aria-label="Фильтры идей">
      <label class="row">
        Приоритет
        <select bind:value={priority} onchange={() => load(user, space, true)} aria-label="Приоритет идей">
          <option value="">любой</option>
          {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
        </select>
      </label>
      <label class="row">
        Тег
        <input bind:value={tag} placeholder="например, ui" aria-label="Тег" disabled={!!query.trim()} title={query.trim() ? 'Поиск по тексту теги не фильтрует' : undefined} />
      </label>
      <label class="row grow">
        Поиск
        <input type="search" bind:value={query} placeholder="заголовок или текст" aria-label="Поиск по идеям" />
      </label>
      <button type="submit">Применить</button>
      {#if applied.tag || applied.query}<button type="button" onclick={reset}>Сбросить</button>{/if}
    </form>
    {#if applied.query}<p class="muted small">Поиск учитывает статус; вкладка, приоритет и тег к нему не применяются.</p>{/if}

    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if notice && !cur}<p class="muted" role="status">{notice}</p>{/if}

    {#if grouped}
      <!-- разделы в порядке чипов, у каждого — своя лента (в role=feed — только статьи); поиск и
           лента не бывают вместе; число в заголовке — «показано из» счётчика сервера; в узкой
           области — аккордеон, открыт первый (непустой) раздел -->
      <div class="groups">
        {#each groupByStatus(hits ?? []) as g, i (g.status)}
          {#snippet feed()}
            <div class="feed" role="feed" aria-label={`Идеи раздела «${statusText(g.status)}»`} aria-busy={loading}>
              {#each g.items as h (h.entry_id)}{@render hit(h)}{/each}
            </div>
          {/snippet}
          {@render group(g.status, i === 0, g.items.length, undefined, feed)}
        {/each}
        {#each groupByStatus(items) as g, i (g.status)}
          {#snippet feed()}
            <div class="feed" role="feed" aria-label={`Идеи раздела «${statusText(g.status)}»`} aria-busy={loading}>
              {#each g.items as it (it.id)}{@render row(it)}{/each}
            </div>
          {/snippet}
          {@render group(g.status, i === 0, g.items.length, counts?.[g.status as EntryStatus], feed)}
        {/each}
      </div>
    {:else}
      <div class="feed" role="feed" aria-label={hits ? 'Найденные идеи' : 'Лента идей'} aria-busy={loading}>
        {#each hits ?? [] as h (h.entry_id)}{@render hit(h)}{/each}
        {#each items as it (it.id)}{@render row(it)}{/each}
      </div>
    {/if}
    {#if !loading && !error && !(hits ?? items).length}
      <p class="muted">{hits ? 'Ничего не найдено.' : filtered ? 'Идей по выбранным фильтрам нет.' : 'Идей нет.'}</p>
    {/if}
    {#if next}
      <button type="button" class="more" aria-disabled={loading} aria-busy={loading} onclick={more}>Показать ещё</button>
    {/if}
    {#if loading}<p class="muted">Загрузка…</p>{/if}
  </div>
{/snippet}

{#snippet ideaDetail()}
  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const agent = originOf(c) === 'agent'}
    {@const rel = releasedOf(c)}
    {@const rv = reviewOf(c)}
    {@const older = olderReviews(c)}
    <article class="card panel" aria-label="Карточка идеи" data-idea-card={c.id}>
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status" data-status={c.status}>{statusText(c.status)}</span>
          <span class="chip prio">приоритет: {PRIORITY_TEXT[priorityOf(c)]}</span>
          {@render marks(agent, [])}
          <span class="muted small">{entryRef('idea', c.id)} · {AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
        {#if c.status === 'deferred' && str(c.data, 'horizon')}<p class="muted">Отложена до {str(c.data, 'horizon')} (UTC).</p>{/if}
      </header>
      {#if mode === 'edit'}
        <!-- правка на месте: форма вместо вида карточки (§6.14 п. 3) -->
        {@render form('edit')}
      {:else}
        <!-- ряд решений — сразу под шапкой, до текста, разбора и истории (правило Г) -->
        <div class="row actions" role="group" aria-label="Решение по идее">
          {#each ideaButtons(c.status, agent, !!rel) as b (b.act)}
            <button type="button" class={b.cls} onclick={() => press(b.act, c)} disabled={busy}>{b.title}</button>
          {/each}
        </div>
        <!-- итог действия: живая область есть всегда (фокус после закрытия окна — сюда) -->
        <p class="muted notice" role="status" tabindex="-1">{notice}</p>
        {#if actionError && !dialogMode}<p class="error" role="alert">{actionError}</p>{/if}
        {#if c.body}<p class="body">{c.body}</p>{/if}
        {#if c.tags.length}<div class="row">{@render marks(false, c.tags)}</div>{/if}
        {#if c.status === 'returned' && rv?.missing}
          <div class="warn ask" data-testid="review-missing">
            <p><strong>Агент просит дописать:</strong></p>
            <p class="body">{rv.missing}</p>
            <p class="small">Допишите идею кнопкой «Изменить»: после сохранения она вернётся на разбор.</p>
          </div>
        {/if}
        {#if curQuestion && (c.status === 'proposed' || rel)}
          <div class="proposal" data-testid={rel ? 'reopen-proposed' : undefined}>
            <p><strong>{rel ? 'Предложено вернуть' : 'Агент предлагает начать'}.</strong> <a href={questionHref(curQuestion.id)}>Вопрос {entryRef('question', curQuestion.id)}</a></p>
            {#if curQuestion.body}<p class="body">{curQuestion.body}</p>{/if}
          </div>
        {/if}
        {#if c.task && !c.task.deleted && !rel}
          <p>
            <a href={taskHref(c.task.id)} data-testid="task-link">Задача {entryRef('task', c.task.id)}</a>
            <span class="muted small">{c.task.title ?? ''}</span>
          </p>
        {/if}
        {#if rel}
          <p class="warn" data-testid="task-released">
            Задача {rel}{#if c.task && !c.task.deleted}: <a href={taskHref(c.task.id)}>{entryRef('task', c.task.id)}</a>{/if}. Идею можно вернуть на
            разбор или отказаться от неё.
          </p>
        {/if}

        {#if rv}
          <section class="review" aria-label="Разбор агента" data-testid="idea-review">
            <h3>Разбор агента</h3>
            <p class="row">
              <span class="chip outcome" data-outcome={rv.outcome}>{REVIEW_OUTCOME_TEXT[rv.outcome]}</span>
              {#if rv.outcome === 'plan' && rv.task}<span>→ {@render ref(rv.task)}</span>{/if}
              <span class="muted small" data-testid="review-by">{reviewerLabel(rv, c.reviewers, viewer)} · {fmtTime(rv.at)}</span>
            </p>
            {#if reviewNote(c)}<p class="muted small" data-testid="review-note">{reviewNote(c)}</p>{/if}
            <p class="body">{rv.summary}</p>
            {#if rv.missing && c.status !== 'returned'}<p class="body"><strong>Не хватало:</strong> {rv.missing}</p>{/if}
            {#if rv.basis.length}{@render refs('Опирался на', rv.basis, 'review-basis')}{/if}
            {#if rv.similar.length}{@render refs('Похожие', rv.similar, 'review-similar')}{/if}
            {#if c.status === 'declined' && reviewCurrent(c)}
              <p class="muted small">Согласны с отклонением — удалите идею или оставьте её: отклонённая служит образцом «уже обсуждали». Не согласны — верните на разбор с доводом.</p>
            {/if}
          </section>
        {/if}
        {#if older.length}
          <details class="history" data-testid="review-history">
            <summary>История разборов ({older.length})</summary>
            {#each older as h, i (i)}
              <div class="past">
                <p class="row">
                  <span class="chip outcome" data-outcome={h.outcome}>{REVIEW_OUTCOME_TEXT[h.outcome]}</span>
                  <span class="muted small">{reviewerLabel(h, c.reviewers, viewer)} · {fmtTime(h.at)}</span>
                </p>
                <p class="body">{h.summary}</p>
                {#if h.missing}<p class="body"><strong>Не хватало:</strong> {h.missing}</p>{/if}
              </div>
            {/each}
          </details>
        {/if}

      {/if}
    </article>
  {/if}
{/snippet}

{#snippet head(status: string, shown: number, total: number | undefined)}
  <h3>{statusText(status)} <span class="muted small">{shownOf(shown, total)}</span></h3>
{/snippet}

<!-- раздел «Группировать по статусу»: в узкой области — `<details>` (число — в заголовке) -->
{#snippet group(status: string, first: boolean, shown: number, total: number | undefined, feed: Snippet)}
  {#if narrow}
    <details class="group acc" data-group={status} open={first}>
      <summary>{statusText(status)} <span class="muted small">{shownOf(shown, total)}</span></summary>
      {@render feed()}
    </details>
  {:else}
    <section class="group" aria-label={`Раздел «${statusText(status)}»`} data-group={status}>
      {@render head(status, shown, total)}
      {@render feed()}
    </section>
  {/if}
{/snippet}

{#snippet hit(h: SearchItem)}
  <article class="idea" class:selected={h.entry_id === entry} data-idea={h.entry_id}>
    <button type="button" class="link title" onclick={() => select(h.entry_id ?? '')}>{h.title ?? ''}</button>
    {#if h.status}<span class="chip">{statusText(h.status)}</span>{/if}
    <p class="snippet small">
      {#each snippetParts(h.snippet ?? '') as p, j (j)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
    </p>
  </article>
{/snippet}

{#snippet row(it: Entry)}
  {@const task = str(it.data, 'task_id')}
  {@const q = str(it.data, 'question_id')}
  {@const hz = it.status === 'deferred' && str(it.data, 'horizon')}
  {@const line = reviewLine(it)}
  {@const planLine = line.kind === 'plan' && !!task && !knowledge}
  <article class="idea" class:selected={it.id === entry} data-idea={it.id}>
    <button type="button" class="link title" onclick={() => select(it.id)}>{it.title}</button>
    <span class="chip prio" data-priority={priorityOf(it)}>{PRIORITY_TEXT[priorityOf(it)]}</span>
    <span class="chip" data-status={it.status}>{statusText(it.status)}</span>
    <span class="muted small">{fmtTime(it.updated_at)}</span>
    {#if hz}<span class="muted small">не раньше {hz} UTC</span>{/if}
    {@render marks(originOf(it) === 'agent', [])}
    {#if planLine}<a class="small decision" href={taskHref(task, space)} data-testid="review-line">{line.text}</a>
    {:else if line.kind}<span class="small decision" data-testid="review-line">{line.text}</span>{/if}
    {#if planLine}<!-- ссылка на задачу — в строке решения -->
    {:else if task && !knowledge}<a class="small" href={taskHref(task, space)}>задача {entryRef('task', task)}</a>
    {:else if task}<button type="button" class="link small" onclick={() => select(it.id)} title="Задача может быть в решении: ссылка — в карточке">задача {entryRef('task', task)}</button>
    {:else if q && it.status === 'proposed'}<a class="small" href={questionHref(q)}>вопрос {entryRef('question', q)}</a>{/if}
    {@render marks(false, it.tags)}
  </article>
{/snippet}

{#snippet ref(r: string)}
  {@const t = refTarget(r)}
  {#if t && 'journal' in t}<a href={href({ name: 'journal', user, space, journal: t.journal, entry: t.id })}>{r}</a>
  {:else if t && 'node' in t}<a href={href({ name: 'node', user, space, node: t.node, anchor: null })}>{r}</a>
  {:else if t && 'copy' in t}<code>{r}</code>
    <button type="button" class="link small" onclick={() => copyRef(r, t.copy)}>{copied === r ? 'скопировано' : 'копировать'}</button>
  {:else}<span>{r}</span>{/if}
{/snippet}

{#snippet refs(label: string, list: string[], testid: string)}
  <div class="refs" data-testid={testid}>
    <span class="muted">{label}:</span>
    <ul class="row">
      {#each list as r (r)}<li>{@render ref(r)}</li>{/each}
    </ul>
  </div>
{/snippet}

{#snippet form(m: IdeaForm)}
  {#await import('./IdeaForm.svelte')}
    {#if m === 'edit'}<p class="muted">Загрузка…</p>{/if}
  {:then { default: Form }}
    {#key opened}
      <Form
        bind:this={ideaForm}
        {m}
        open={dialogMode}
        idea={m === 'create' ? null : cur}
        {targets}
        space0={defaultTarget()}
        {busy}
        error={actionError}
        refocus={cardNotice}
        onsubmit={submitted}
        onerror={(e) => (actionError = e)}
        oncancel={closeForm}
      />
    {/key}
  {:catch e}
    <!-- чанк формы не пришёл: сервер обновился (у чанков новые имена) или нет сети (§5.1 п. 9) -->
    {#if m === 'edit'}
      <LazyFail error={e} label="Форма не загрузилась" hint="обновите страницу">
        <button type="button" onclick={closeForm}>Отмена</button>
      </LazyFail>
    {:else}
      <!-- окно формы — с той же ошибкой (`role="alert"`) и «Обновить страницу» -->
      <FormDialog
        open={dialogMode}
        confirm
        title={IDEA_FORMS[m].title}
        submit="Обновить страницу"
        onsubmit={() => location.reload()}
        onclose={closeForm}
      >
        <p class="error" role="alert">Форма не загрузилась: {errorText(e)} — обновите страницу</p>
      </FormDialog>
    {/if}
  {/await}
{/snippet}

{#snippet marks(agent: boolean, tags: string[])}
  {#if agent}<span class="badge small">от агента</span>{/if}
  {#each tags as t (t)}<span class="chip">#{t}</span>{/each}
{/snippet}

<style>
  .more[aria-disabled='true'] {
    opacity: 0.5;
    cursor: default;
  }
  .bar {
    margin-bottom: 8px;
  }
  .tabs {
    display: flex;
    gap: 4px;
    margin: 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    border: none;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: none;
  }
  .tabs button[aria-selected='true'] {
    border-bottom-color: var(--accent);
    color: var(--accent);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 8px;
  }
  .status-chips {
    min-width: 0;
    max-width: 100%;
    gap: 6px;
  }
  /* при касании — зазор между чипами полосы (`--touch-gap`, app.css) */
  @media (pointer: coarse) {
    .status-chips {
      gap: var(--touch-gap);
    }
  }
  .filters[hidden] {
    display: none;
  }
  .acc {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0 8px;
  }
  .acc[open] {
    padding-bottom: 8px;
  }
  .acc summary {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 36px;
    font-weight: 600;
    font-size: 14px;
    cursor: pointer;
    list-style: none;
  }
  .acc summary::-webkit-details-marker {
    display: none;
  }
  .acc summary::before {
    content: '▸';
    color: var(--muted);
  }
  .acc[open] > summary::before {
    content: '▾';
  }
  @media (pointer: coarse) {
    .acc summary {
      min-height: 44px;
    }
  }
  .schip {
    padding: 2px 10px;
    border-radius: 12px;
  }
  .schip[aria-pressed='true'] {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--selected);
  }
  .count {
    margin-left: 2px;
    color: var(--muted);
    font-size: 12px;
  }
  .filters {
    margin-bottom: 10px;
  }
  .filters .grow {
    flex: 1 1 220px;
  }
  .filters .grow input {
    flex: 1;
    min-width: 0;
  }
  .feed,
  .group,
  .groups {
    display: grid;
    gap: 6px;
  }
  .group h3 {
    margin: 8px 0 0;
    font-size: 14px;
  }
  .idea {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .idea.selected {
    background: var(--selected);
  }
  .decision {
    flex-basis: 100%;
    overflow-wrap: anywhere;
  }
  .snippet {
    flex-basis: 100%;
    margin: 0;
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
  }
  .title {
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .badge {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .card {
    margin-top: 4px;
  }
  .card h2 {
    margin: 0 0 4px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .ask p,
  .review p,
  .past p {
    margin: 4px 0;
  }
  .proposal,
  .review {
    padding: 8px 10px;
    border-left: 3px solid var(--accent);
  }
  .review {
    margin-top: 12px;
  }
  .review h3 {
    margin: 0;
    font-size: 15px;
  }
  .refs ul {
    margin: 2px 0;
    padding: 0;
    list-style: none;
  }
  .refs li {
    overflow-wrap: anywhere;
  }
  .history {
    margin-top: 10px;
  }
  .past {
    padding: 4px 0 4px 10px;
    border-left: 2px solid var(--border);
    margin-top: 6px;
  }
  .meta {
    gap: 6px;
  }
  .actions {
    margin-top: 12px;
  }
  header p {
    margin: 4px 0 0;
  }
  .notice {
    margin: 8px 0 0;
  }
  .notice:empty {
    margin: 0;
  }
  .small {
    font-size: 12px;
  }
</style>
