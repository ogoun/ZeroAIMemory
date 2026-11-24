<script lang="ts">
  // «Задачи» (M4.5 §3): доска по статусам (открыта, в работе, заблокирована, выполнена, снята),
  // у карточки — следующий шаг, блокер, захват («в работе у <агент>, до <время>»), у выполненных —
  // длительность. Человек меняет статус и поля (PATCH с If-Match; переоткрытие из done/dropped —
  // с причиной) и снимает захват агента. Захватывает задачу агент: захват привязан к сессии
  // агента (`X-Zam-Session`), у WebUI сессии агента нет. План работ (M6.5 §9): у задачи из идеи —
  // пометка «из идеи w:<id> «…»» со ссылкой на идею (поле `idea`, в карточке — все её идеи),
  // кнопка «Из идей» — только такие задачи (`from_idea=1` на сервере).
  //
  // Раскладка (M7.2, спецификация §5.2 п. 1) — по ширине своей области: от BOARD_MIN — колонки
  // статусов (`data-board="columns"`), карточка — панель поверх доски (`JournalSplit` `overlay`);
  // уже — вкладки статусов с числами (`StatusTabs`, `data-board="tabs"`), карточка — вместо доски
  // («← Задачи»). Каждый статус — свой запрос (`status=<s>`, по 50, «Показать ещё» у колонки или
  // вкладки, решение 19); на вкладках грузится только выбранная. Числа колонок и вкладок —
  // `entries/counts` сервера, с фильтром «Из идей». Вкладка по умолчанию — «В работе», при пустой —
  // «Открыт»; задача, открытая из адреса, выбирает вкладку своего статуса (решение 18).
  //
  // Подзадачи `part_of` (M7.2, спецификация §5.5, решения 21–23): доска, её колонки и числа — верхний
  // уровень (`top_level=1`); у общей — «8/13» (`done/(total − dropped)`, `taskProgress.ts`) и полоса
  // прогресса, «Подзадачи ▸» раскрывает шаги под плиткой (`parent=<id>` в порядке шагов, вложенная
  // общая — со своим счётчиком и раскрытием). Переключатель «Подзадачи на доске» (выключен) —
  // все задачи по статусам с пометкой «часть k:… «…»», как до волны; «Из идей» — тоже без дерева
  // (пункты плана — задачи из идей, и шаги тоже), переключатель тогда нажат. Карточка общей — раздел
  // «Подзадачи 8/13» (`subtasks.items`), подзадачи — «Часть: k:<id> «…» (8/13)»; «давно без
  // обновлений» (`stale` сервера) — плашка на плитке и в карточке (не `staleOf`: та — о ссылках на
  // файлы).
  //
  // Правка и корзина (M7.2, спецификация §5.9): в карточке — «Изменить» (`EntryEdit`: заголовок,
  // текст, приоритет, кто ведёт, оценка, ссылки, теги, область; несохранённое в форме — вопрос перед
  // закрытием панели) и «Удалить» (в корзину, с подтверждением); «Корзина» доски — вместо доски
  // удалённые задачи с «Восстановить» (`JournalTrash`). Форма и корзина — ленивые чанки. 409 формы
  // переносит и черновик шага (`reread`).
  //
  // Карточка и формы (M7.3, спецификация §6.14, задача владельца k:01M85AS1C0EZVV95FQZ2ZEQCET): под
  // шапкой карточки (заголовок, статус, «Часть», захват, блокер) — ряд «Смена статуса», «Изменить» и
  // «Удалить», поле «Следующий шаг»; текст, «Из идей», подзадачи и файлы — ниже (правило Г). Смена
  // статуса без обязательных полей («→ в работе», «→ открыт») — сразу, итог строкой; «→ выполнен» и
  // «→ снят» — окно-подтверждение (конечные статусы, обратно — только «Переоткрыть»); переоткрытие
  // (причина) и «→ заблокирован» (что мешает) — окно формы (`FormDialog`); «Новая задача» — окно,
  // после «Добавить» — карточка созданной. «Изменить» — правка в карточке на месте (вид карточки
  // сменяется формой). Шаг и правка — под проверкой ухода (стек `pushGuard`): уход по адресу
  // спрашивает один раз; окна спрашивают о несохранённом сами. 409 окна статуса — набранное
  // остаётся, нетронутое — из свежей записи (`rebase`).
  import { onMount, tick, untrack } from 'svelte';
  import FileRefs from './FileRefs.svelte';
  import JournalSplit from './JournalSplit.svelte';
  import EditPanel from './EditPanel.svelte';
  import FormDialog from './FormDialog.svelte';
  import Notice from './Notice.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import StatusTabs from './StatusTabs.svelte';
  import TrashPanel from './TrashPanel.svelte';
  import {
    ApiError,
    createEntry,
    entryCounts,
    errorText,
    getEntry,
    isStaleEntry,
    listEntries,
    patchEntry,
    releaseTask,
    type Entry,
    type EntryStatus,
  } from '../lib/api';
  import { guardLeave } from '../lib/leaveEdit';
  import { trashAsk } from '../lib/entryTrash';
  import { focusSoon, revealTop } from '../lib/focus';
  import { rebase, rebaseText } from '../lib/formDraft';
  import { loadRefs, staleOf, type RefsByTarget } from '../lib/fileRefs';
  import {
    AUTHOR_TEXT,
    JOURNAL_TITLE,
    STATUS_TEXT,
    TASK_COLUMNS,
    boardTab,
    capitalized,
    entryRef,
    entryStatusText,
    fmtTime,
  } from '../lib/journals';
  import { BOARD_MIN } from '../lib/layout';
  import { TASK_FORMS } from '../lib/plural';
  import { go, href } from '../lib/router';
  import { progressText, progressValue, staleText, type ProgressOf } from '../lib/taskProgress';
  import { observeWidth } from '../lib/viewport.svelte';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  /** Задач статуса на страницу (решение 19). */
  const PAGE = 50;
  /** Наибольшая страница ленты сервера: перечитывание после правки сохраняет загруженное до неё. */
  const PAGE_MAX = 200;

  /** Колонка (вкладка) статуса: загруженные задачи, курсор, состояние запроса. */
  interface Column {
    items: Entry[];
    next: string | null;
    loading: boolean;
    loaded: boolean;
    error: string;
  }
  const emptyColumn = (): Column => ({ items: [], next: null, loading: false, loaded: false, error: '' });
  const emptyColumns = () => Object.fromEntries(TASK_COLUMNS.map((s) => [s, emptyColumn()])) as Record<EntryStatus, Column>;

  let cols = $state(emptyColumns());
  /** Номер последнего запроса колонки: ответ устаревшего запроса не применяется. */
  const cseq: Partial<Record<EntryStatus, number>> = {};
  /** Число задач по статусам (`entries/counts`, с «Из идей»); `null` — ещё не пришло. */
  let counts = $state<Partial<Record<EntryStatus, number>> | null>(null);
  let countsFailed = $state(false);
  let kseq = 0;
  let refs = $state<RefsByTarget>(new Map());
  /** Только задачи из идей (пункты плана). */
  let fromIdea = $state(false);
  /** «Подзадачи на доске» (по умолчанию выключен, решение 23): все задачи по статусам с пометкой
   * «часть k:… «…»», как до M7.2; выключен — верхний уровень (`top_level=1`), шаги — под общей. */
  let withSubtasks = $state(false);
  /** Доска без дерева — все задачи по статусам, у шага «часть …»: переключатель или «Из идей»
   * (пункты плана — задачи из идей: шаг из идеи под общей не из идеи иначе пропал бы с доски). */
  let flat = $derived(withSubtasks || fromIdea);
  /** Подзадачи общих задач по id общей (`parent=<id>`, порядок шагов): строки, курсор, запрос. */
  let subs = $state<Record<string, Column>>({});
  /** Номер последнего запроса подзадач общей: ответ устаревшего запроса не применяется. */
  const sseq: Record<string, number> = {};
  /** Раскрытые списки подзадач по пути плитки («<общая>», «<общая>/<вложенная>»): одна задача в
   * двух местах доски раскрывается отдельно, id элементов не повторяются. */
  let unfolded = $state<Record<string, boolean>>({});
  const uid = $props.id();

  /** Ширина области доски (CSS px); 0 — ещё не измерена. */
  let width = $state(0);
  let board = $derived<'columns' | 'tabs' | null>(width === 0 ? null : width >= BOARD_MIN ? 'columns' : 'tabs');
  /** Выбранная вкладка; `null` — ещё не выбрана (ждём числа и запись из адреса). */
  let tab = $state<EntryStatus | null>(null);
  /** Задача, по статусу которой уже выбрана вкладка: правка статуса вкладку не меняет. */
  let tabFor = '';
  /** Статусы на экране: колонки — все, вкладки — выбранная. */
  let visible = $derived<EntryStatus[]>(board === 'columns' ? TASK_COLUMNS : board === 'tabs' && tab ? [tab] : []);
  let total = $derived(counts ? TASK_COLUMNS.reduce((n, s) => n + (counts?.[s] ?? 0), 0) : null);
  /** Ошибка загрузки видимых статусов — одна на доску. */
  let boardError = $derived(visible.map((s) => cols[s].error).find(Boolean) ?? '');

  let cur = $state<Entry | null>(null);
  let curError = $state('');
  let eseq = 0;
  /** Карточка: просмотр или правка на месте (`EditPanel` вместо вида карточки). */
  type Mode = 'none' | 'edit';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');
  /** Окно «Новая задача». */
  let createOpen = $state(false);
  /** Окно смены статуса: переоткрытие (причина), «заблокирован» (что мешает) или подтверждение
   * конечного статуса. */
  type StatusDialog = { to: EntryStatus; kind: 'reopen' | 'blocked' | 'final' };
  let statusDlg = $state<StatusDialog | null>(null);
  /** Поля окна статуса при открытии (для `rebase` при 409). */
  let statusBase = { reason: '', blocked: '' };
  /** Поля окна статуса, изменённые и здесь, и в другом месте (409): помечены. */
  let statusClash = $state<string[]>([]);
  /** Ошибка открытого окна (сервер, проверка, 409). */
  let dlgError = $state('');
  /** «Корзина»: вместо доски — удалённые задачи. */
  let trash = $state(false);
  /** Итог действия, закрывшего карточку (удаление), — над доской. */
  let boardNotice = $state('');
  let ask = $state<Ask | null>(null);
  /** Форма правки (`EditPanel`): её несохранённое — в `unsaved()` и `leaveEdit()`. */
  let editor = $state<{ dirty: () => boolean; leave: () => boolean; release: () => void } | null>(null);

  let nStep = $state('');
  let nTitle = $state('');
  let nBody = $state('');
  let sReason = $state('');
  let sBlocked = $state('');
  let stepDraft = $state('');

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  // Доска — заново только при смене пользователя или пространства: свойства приходят из объекта
  // маршрута, и эффект перезапускается на каждый переход (открытие задачи) — вкладка, загруженные
  // страницы и прокрутка при этом целы (§5.1 п. 5). Неудавшиеся запросы повторяются при следующем
  // переходе.
  let listOf = '';
  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      if (listOf !== `${u}|${s}`) {
        listOf = `${u}|${s}`;
        fromIdea = false;
        withSubtasks = false;
        trash = false;
        boardNotice = '';
        tab = null;
        tabFor = '';
        resetBoard();
        reloadRefs();
        return;
      }
      if (countsFailed) loadCounts();
      for (const st of visible) if (cols[st].error) loadColumn(st, true);
      for (const id of unfoldedIds()) if (subs[id]?.error) loadSubs(id, true);
    });
  });

  // Видимые статусы загружены: колонки — все, вкладки — выбранная (прочие — при выборе).
  $effect(() => {
    const vis = visible;
    const c = cols;
    untrack(() => {
      for (const s of vis) if (!c[s].loaded && !c[s].loading && !c[s].error) loadColumn(s, true);
    });
  });

  // Видимые раскрытия загружены, как колонки: вложенная, раскрытая под свёрнутой внешней, после
  // правки из карточки шагов не имеет (`reloadBoard` перечитывает только видимые) — её шаги грузятся,
  // когда внешнюю раскроют снова.
  $effect(() => {
    const missing = [...unfoldedIds()].filter((id) => !subs[id]);
    untrack(() => {
      for (const id of missing) loadSubs(id, true);
    });
  });

  // Вкладка по умолчанию — когда пришли числа (и запись из адреса, если она есть).
  $effect(() => {
    if (board !== 'tabs' || tab !== null) return;
    const c = counts;
    const failed = countsFailed;
    const e = cur;
    const ce = curError;
    if (c === null && !failed) return;
    if (entry && !e && !ce) return;
    untrack(() => {
      if (e) tabFor = e.id;
      tab = boardTab(c, e ? tabStatusOf(e) : null);
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  /** Ссылки на файлы задач («возможно устарело»): при входе в журнал и после правки. */
  function reloadRefs() {
    const u = user;
    const s = space;
    loadRefs(u, s).then((r) => {
      if (u === user && s === space) refs = r;
    });
  }

  /** Всё заново: числа и колонки (видимые загрузит эффект выше), раскрытия подзадач — свёрнуты. */
  function resetBoard() {
    for (const s of TASK_COLUMNS) cseq[s] = (cseq[s] ?? 0) + 1;
    cols = emptyColumns();
    for (const id of Object.keys(sseq)) sseq[id]++;
    subs = {};
    unfolded = {};
    counts = null;
    countsFailed = false;
    loadCounts();
  }

  async function loadCounts() {
    const my = ++kseq;
    try {
      const c = await entryCounts(user, space, { kind: 'task', fromIdea, topLevel: !flat });
      if (my !== kseq) return;
      counts = c;
      countsFailed = false;
    } catch (e) {
      // без чисел доска работает; 401 — сессия кончилась
      if (my !== kseq) return;
      countsFailed = true;
      if (e instanceof ApiError && e.status === 401) onexpired();
    }
  }

  /** Страница задач статуса `s`: первая (`reset`) или следующая по курсору; `limit` — размер. */
  async function loadColumn(s: EntryStatus, reset: boolean, limit = PAGE) {
    const c = cols[s];
    const my = (cseq[s] = (cseq[s] ?? 0) + 1);
    c.loading = true;
    c.error = '';
    try {
      const r = await listEntries(user, space, {
        kind: 'task',
        status: [s],
        fromIdea,
        topLevel: !flat,
        cursor: reset ? null : c.next,
        limit,
      });
      if (my !== cseq[s]) return;
      c.items = reset ? r.items : [...c.items, ...r.items];
      c.next = r.next_cursor;
      c.loaded = true;
    } catch (e) {
      if (my !== cseq[s]) return;
      if (reset) {
        c.items = [];
        c.next = null;
        c.loaded = false;
      }
      fail(e, (m) => (c.error = m));
    } finally {
      if (my === cseq[s]) c.loading = false;
    }
  }

  /** Подзадачи общей `id` — все статусы в порядке шагов, страница PAGE_MAX (план целиком — одной
   * страницей, `docs/api-compat.md`); `reset` — с начала, иначе следующая по курсору. При ошибке
   * перечитывания загруженное остаётся. */
  async function loadSubs(id: string, reset: boolean) {
    if (!subs[id]) subs[id] = emptyColumn();
    const c = subs[id];
    const my = (sseq[id] = (sseq[id] ?? 0) + 1);
    c.loading = true;
    c.error = '';
    try {
      const r = await listEntries(user, space, { kind: 'task', parent: id, cursor: reset ? null : c.next, limit: PAGE_MAX });
      if (my !== sseq[id]) return;
      c.items = reset ? r.items : [...c.items, ...r.items];
      c.next = r.next_cursor;
      c.loaded = true;
    } catch (e) {
      if (my !== sseq[id]) return;
      fail(e, (m) => (c.error = m));
    } finally {
      if (my === sseq[id]) c.loading = false;
    }
  }

  /** id общих задач с раскрытыми и видимыми подзадачами: путь раскрыт целиком (свёрнутая внешняя
   * прячет вложенные, но их раскрытие помнит). */
  function unfoldedIds(): Set<string> {
    const ids = new Set<string>();
    for (const p of Object.keys(unfolded)) {
      const parts = p.split('/');
      if (parts.every((_, i) => unfolded[parts.slice(0, i + 1).join('/')])) ids.add(parts[parts.length - 1]);
    }
    return ids;
  }

  /** «Подзадачи ▸»: раскрыть (впервые и после ошибки — запрос `parent=<id>`; вложенные раскрытия
   * без шагов загрузит эффект выше) или свернуть. */
  function toggleSubs(path: string, id: string) {
    if (unfolded[path]) {
      delete unfolded[path];
      return;
    }
    unfolded[path] = true;
    const c = subs[id];
    if (!c || c.error || (!c.loaded && !c.loading)) loadSubs(id, true);
  }

  /** После правки: числа и видимые статусы — заново (задача могла сменить колонку), столько же
   * задач, сколько было загружено «Показать ещё» (до PAGE_MAX); прочие вкладки — при следующем
   * выборе. Видимые раскрытия — заново (статус шага и счётчик вложенной общей), шаги прочих
   * забываются и грузятся при следующем показе (эффект видимых раскрытий). */
  async function reloadBoard() {
    const vis = visible;
    for (const s of TASK_COLUMNS) {
      if (vis.includes(s)) continue;
      cseq[s] = (cseq[s] ?? 0) + 1;
      cols[s] = emptyColumn();
    }
    const openIds = unfoldedIds();
    for (const id of Object.keys(subs)) {
      if (openIds.has(id)) continue;
      sseq[id] = (sseq[id] ?? 0) + 1;
      delete subs[id];
    }
    const size = (s: EntryStatus) => Math.min(PAGE_MAX, Math.max(PAGE, cols[s].items.length));
    await Promise.all([loadCounts(), ...vis.map((s) => loadColumn(s, true, size(s))), ...[...openIds].map((id) => loadSubs(id, true))]);
  }

  /** «Показать ещё» колонки, вкладки или подзадач (`c` — их список, `load` — следующая страница,
   * `rows` — строки от родителя кнопки). Кнопка на время загрузки остаётся (`aria-disabled`), фокус
   * на ней цел; страниц больше нет и кнопка ушла — фокус на первую догруженную строку. */
  async function more(c: Column, load: () => Promise<void>, rows: string, e: MouseEvent) {
    if (c.loading) return;
    const btn = e.currentTarget as HTMLElement;
    const box = btn.parentElement;
    const first = c.items.length;
    await load();
    await tick();
    const a = document.activeElement;
    if (btn.isConnected || (a && a !== document.body)) return;
    box?.querySelectorAll<HTMLElement>(rows)[first]?.querySelector<HTMLElement>('a[href], button')?.focus();
  }

  function selectTab(s: EntryStatus) {
    tab = s;
    if (cols[s].error) loadColumn(s, true);
  }

  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    curError = '';
    // другая запись (переход по адресу уже спросил о набранном): окна закрываются
    if (id !== (cur?.id ?? null)) {
      statusDlg = null;
      createOpen = false;
    }
    if (!id) {
      cur = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'task') {
        cur = null;
        curError = 'Это не задача.';
        return;
      }
      cur = e;
      stepDraft = str(e.data, 'next_step');
      // запись из адреса — вкладка её статуса; правка статуса из карточки вкладку не меняет
      const st = e.id !== tabFor ? tabStatusOf(e) : null;
      if (st && TASK_COLUMNS.includes(st)) {
        tabFor = e.id;
        tab = st;
      }
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  /** Своя плитка задачи `e` на текущей доске: «Из идей» — только у задач из идей (шаг не из идеи —
   * лишь в раскрытии общей, общая не из идеи — лишь по ссылке «часть …»); иначе с переключателем —
   * у всех, без него — у верхнего уровня. */
  const hasTile = (e: Entry): boolean => (fromIdea ? !!e.idea : withSubtasks || !e.parent);

  /** Вкладка открытой задачи `e` — её статус (решение 18). Своей плитки на доске нет (шаг в
   * раскрытии, общая по ссылке «часть …»): открытая с доски вкладку не меняет, из адреса — вкладка
   * родителя, а без него — своя. `null` — вкладку не менять. */
  function tabStatusOf(e: Entry): EntryStatus | null {
    if (hasTile(e)) return e.status;
    return tab === null ? (e.parent?.status ?? e.status) : null;
  }

  const str = (d: Record<string, unknown>, k: string) => (typeof d[k] === 'string' ? (d[k] as string) : '');

  /** Действующий захват: `{agent, until}` или `null` (срок истёк — захвата нет). */
  function claimOf(e: Entry): { agent: string; until: string } | null {
    const c = e.data.claimed_by;
    if (!c || typeof c !== 'object') return null;
    const o = c as Record<string, unknown>;
    const until = typeof o.until === 'string' ? o.until : '';
    if (!until || new Date(until).getTime() <= Date.now()) return null;
    return { agent: typeof o.agent === 'string' && o.agent ? o.agent : 'агент', until };
  }

  const claimText = (c: { agent: string; until: string }) => `в работе у ${c.agent}, до ${fmtTime(c.until)}`;

  const durationText = (m: number | null | undefined) => {
    if (m === null || m === undefined) return '';
    if (m < 60) return `${m} мин`;
    const h = Math.floor(m / 60);
    return m % 60 ? `${h} ч ${m % 60} мин` : `${h} ч`;
  };

  let tabs = $derived(TASK_COLUMNS.map((s) => ({ id: s, title: capitalized(STATUS_TEXT[s]), count: counts?.[s] })));

  function toggleFromIdea() {
    fromIdea = !fromIdea;
    resetBoard();
  }

  function toggleSubtasks() {
    withSubtasks = !withSubtasks;
    resetBoard();
  }

  /** Адрес задачи на доске (ссылки подзадач и «часть …»): открытие — новая запись истории. */
  const taskHref = (id: string) => href({ name: 'journal', user, space, journal: 'tasks', entry: id });

  /** Ссылка на идею задачи: идея в пространстве задачи — в нём же, иначе — Knowledge (задача
   * «начать» общей идеи бывает в решении той же базы). */
  const ideaHref = (t: Entry, id: string, ideaSpace: string) =>
    href({ name: 'journal', user, space: ideaSpace === t.space_id ? space : 'knowledge', journal: 'ideas', entry: id });

  /** Открыть задачу: новая запись истории — «назад» браузера возвращает к доске (§5.1 п. 5). */
  function select(id: string) {
    notice = '';
    boardNotice = '';
    go({ name: 'journal', user, space, journal: 'tasks', entry: id });
  }

  /** Карточка показана (панелью поверх доски или вместо неё). */
  let detailOpen = $derived(cur !== null || curError !== '');

  /** Закрыли карточку («← Задачи», фон, Esc): несохранённое панель уже спросила («Закрыть без
   * сохранения?») — форма правки и черновик шага отпускаются, переход не спрашивает снова. */
  function closeDetail() {
    editor?.release();
    mode = 'none';
    actionError = '';
    stepDraft = cur ? str(cur.data, 'next_step') : '';
  }

  /** Черновик «Следующего шага» отличается от шага задачи (поле на виду, правило А): обрезанный —
   * так, как уйдёт при сохранении. После сохранения и перечитывания карточки (`open` берёт
   * `next_step` сервера) — пусто; после неудачной отправки набранное остаётся несохранённым. */
  const stepDirty = (): boolean => {
    const c = cur;
    return !!c && c.status !== 'done' && c.status !== 'dropped' && stepDraft.trim() !== str(c.data, 'next_step');
  };

  /** Несохранённое в карточке (вопрос панели перед закрытием): правка на месте и шаг. Окна
   * спрашивают о своём сами (`FormDialog`). */
  function unsaved(): boolean {
    if (!cur) return false;
    // форма правки сравнивает черновик с перечитанной записью сама (`EntryEdit.dirty`)
    if (mode === 'edit' && editor?.dirty()) return true;
    return stepDirty();
  }

  // шаг на виду — в стеке проверок ухода (§6.14 п. 4): уход по адресу спрашивает; согласие —
  // черновик шага отпущен (новая карточка возьмёт свой шаг)
  onMount(() => guardLeave(stepDirty, () => (stepDraft = cur ? str(cur.data, 'next_step') : '')));

  async function act(fn: () => Promise<string | void>, done: string, focusNotice = false) {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      mode = 'none';
      await reloadBoard();
      reloadRefs();
      if (target && target !== entry) select(target);
      else await open(user, target);
      notice = done;
      // нажатая кнопка ушла вместе с прежним статусом — фокус на итог
      if (focusNotice) focusSoon(noticeEl);
    } catch (e) {
      if (isStaleEntry(e) && cur) {
        // 409 (§6.14 п. 5): карточка перечитана; набранный шаг остаётся (правило `rebase`: тронутое —
        // человека), нетронутый — из свежей записи
        const msg = errorText(e);
        const typed = stepDirty() ? stepDraft : null;
        await open(user, cur.id);
        if (typed !== null) stepDraft = typed;
        actionError = msg;
      } else {
        fail(e, (m) => (actionError = m));
      }
    } finally {
      busy = false;
    }
  }

  /** Статусы, в которые можно перейти из текущего (из done/dropped — только в open, с причиной). */
  const targets = (e: Entry): EntryStatus[] =>
    e.status === 'done' || e.status === 'dropped'
      ? ['open']
      : TASK_COLUMNS.filter((s) => s !== e.status);

  /** Кнопка ряда «Смена статуса» (решение 84): без обязательных полей — сразу; конечный статус —
   * подтверждение; переоткрытие и «заблокирован» — окно с полем. */
  function pressStatus(s: EntryStatus) {
    const c = cur;
    if (!c || busy) return;
    actionError = '';
    dlgError = '';
    const reopen = c.status === 'done' || c.status === 'dropped';
    if (reopen || s === 'blocked' || s === 'done' || s === 'dropped') {
      sReason = '';
      sBlocked = str(c.data, 'blocked_by');
      statusBase = { reason: '', blocked: sBlocked };
      statusClash = [];
      statusDlg = { to: s, kind: reopen ? 'reopen' : s === 'blocked' ? 'blocked' : 'final' };
      return;
    }
    act(
      async () => {
        await patchEntry(user, c.id, c.rev, { status: s });
      },
      `Статус: ${STATUS_TEXT[s]}.`,
      true,
    );
  }

  /** Заголовок, пояснение и кнопка окна статуса. */
  const statusTitle = (d: StatusDialog | null): string =>
    !d ? '' : d.kind === 'reopen' ? 'Переоткрыть задачу' : d.kind === 'blocked' ? 'Задача заблокирована' : d.to === 'done' ? 'Отметить задачу выполненной?' : 'Снять задачу?';
  const statusDescription = (d: StatusDialog | null): string | undefined =>
    !d
      ? undefined
      : d.kind === 'reopen'
        ? 'Задача вернётся в «открыт»; причина останется в истории записи.'
        : d.kind === 'blocked'
          ? 'Задача перейдёт в «заблокирован»; что мешает — видно на доске и в карточке.'
          : `Задача перейдёт в «${STATUS_TEXT[d.to]}» и уйдёт из работы; вернуть её можно только кнопкой «Переоткрыть» с причиной.`;
  const statusSubmit = (d: StatusDialog | null): string =>
    !d ? '' : d.kind === 'reopen' ? 'Переоткрыть' : d.kind === 'blocked' ? 'Заблокировать' : d.to === 'done' ? 'Отметить выполненной' : 'Снять задачу';

  /** Набранное в окне статуса против полей при открытии. */
  const statusDirty = (): boolean =>
    !!statusDlg && (sReason.trim() !== '' || (statusDlg.kind === 'blocked' && sBlocked.trim() !== statusBase.blocked.trim()));

  /** Окно статуса: запрос; ошибка и 409 — в окне (набранное остаётся, нетронутое — из свежей
   * записи), успех — окно закрыто, доска и карточка перечитаны, итог строкой. */
  async function submitStatus() {
    const c = cur;
    const d = statusDlg;
    if (!c || !d) return;
    const reason = sReason.trim();
    if (d.kind === 'reopen' && !reason) {
      dlgError = 'Переоткрытие требует причины.';
      return;
    }
    dlgError = '';
    try {
      await patchEntry(user, c.id, c.rev, {
        status: d.to,
        ...(d.kind === 'reopen' ? { reason } : {}),
        ...(d.kind === 'blocked' && sBlocked.trim() ? { blocked_by: sBlocked.trim() } : {}),
      });
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      if (isStaleEntry(e)) return staleStatus(c.id);
      dlgError = errorText(e);
      return;
    }
    statusDlg = null;
    busy = true;
    try {
      await reloadBoard();
      reloadRefs();
      await open(user, c.id);
      notice = `Статус: ${STATUS_TEXT[d.to]}.`;
    } finally {
      busy = false;
    }
  }

  /** 409 окна статуса: запись перечитана, черновик перенесён (`rebase`), окно открыто. */
  async function staleStatus(id: string) {
    let fresh: Entry;
    try {
      fresh = await getEntry(user, id);
    } catch (e) {
      dlgError = errorText(e);
      return;
    }
    const next = { reason: '', blocked: str(fresh.data, 'blocked_by') };
    const r = rebase({ reason: sReason, blocked: sBlocked }, statusBase, next);
    sReason = r.fields.reason;
    sBlocked = r.fields.blocked;
    statusBase = next;
    statusClash = r.clash.map(String);
    reread(fresh);
    dlgError = rebaseText(
      r.clash.map((k) => (k === 'blocked' ? 'что мешает' : 'причина')),
      fresh.rev,
    );
  }

  function saveStep(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    act(async () => {
      await patchEntry(user, c.id, c.rev, { next_step: stepDraft.trim() });
    }, 'Следующий шаг сохранён.');
  }

  function release() {
    const c = cur;
    if (!c) return;
    act(
      async () => {
        await releaseTask(user, c.id);
      },
      'Захват снят.',
      true,
    );
  }

  /** «Изменить»: вид карточки сменяется формой (правило В), карточка — с начала, фокус — на первом
   * поле формы (`EntryEdit`). */
  async function startEdit() {
    actionError = '';
    notice = '';
    mode = 'edit';
    await tick();
    revealTop(detailEl());
  }

  /** Правка сохранена (`EntryEdit`): доска и ссылки — заново, карточка перечитана, фокус — на её
   * заголовке (кнопки формы ушли). */
  async function edited(e: Entry) {
    mode = 'none';
    cur = e;
    await reloadBoard();
    reloadRefs();
    await open(user, e.id);
    notice = 'Задача сохранена.';
    focusSoon(() => detailEl()?.querySelector('h2'));
  }

  /** «Отмена» формы правки: фокус — на «Изменить». */
  function cancelEdit() {
    mode = 'none';
    focusSoon(() => detailEl()?.querySelector('[aria-label="Правка задачи"] button'));
  }

  /** 409 формы правки: запись перечитана. Черновик «Следующего шага» — по правилу `rebase` формы:
   * нетронутый (равен шагу карточки) — из перечитанной записи, иначе «Сохранить шаг» затёр бы шаг
   * агента новой ревизией, а закрытие панели спросило бы о несохранённом; набранный — остаётся. */
  function reread(e: Entry) {
    if (cur && stepDraft.trim() === str(cur.data, 'next_step')) stepDraft = str(e.data, 'next_step');
    cur = e;
  }

  /** Карточка задачи (в панели или вместо доски). */
  const detailEl = () => root?.querySelector<HTMLElement>('article[aria-label="Карточка задачи"]');
  /** Итог действия в карточке (`Notice`): фокус после смены статуса и закрытия окна. */
  const noticeEl = () => detailEl()?.querySelector<HTMLElement>('.card-notice [role="status"]');

  /** «Удалить»: в корзину; панель закрывается, итог — над доской. */
  function askDelete(c: Entry) {
    actionError = '';
    notice = '';
    ask = trashAsk(user, c, {
      deleted: async (msg) => {
        mode = 'none';
        cur = null;
        go({ name: 'journal', user, space, journal: 'tasks', entry: null }, { replace: true });
        await reloadBoard();
        reloadRefs();
        boardNotice = msg;
      },
      stale: async (msg) => {
        await open(user, c.id);
        actionError = msg;
      },
      expired: onexpired,
      // панель закрыта, плитки нет — фокус на итог над доской
      focus: () => root?.querySelector('.board-notice [role="status"]'),
    });
  }

  function toggleTrash() {
    trash = !trash;
    boardNotice = '';
    // назад к доске: восстановленные из корзины — на ней (числа и видимые статусы — заново)
    if (!trash) reloadBoard();
  }

  let root = $state<HTMLElement | null>(null);

  function startCreate() {
    nTitle = '';
    nBody = '';
    nStep = '';
    dlgError = '';
    createOpen = true;
  }

  const createDirty = () => !!(nTitle.trim() || nBody.trim() || nStep.trim());

  /** «Добавить»: окно закрывается, открывается карточка созданной (адрес с `entry`: заголовок на
   * виду и в фокусе; на телефоне запись слоя окна становится записью карточки — одна «Назад»
   * возвращает к доске), доска перечитывается, итог — строкой (§6.14 п. 6). */
  async function submitCreate() {
    const title = nTitle.trim();
    if (!title) {
      dlgError = 'Укажите заголовок.';
      return;
    }
    dlgError = '';
    let id: string;
    try {
      id = (
        await createEntry(user, space, {
          kind: 'task',
          title,
          body: nBody,
          data: nStep.trim() ? { next_step: nStep.trim() } : {},
        })
      ).id;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      dlgError = errorText(e);
      return;
    }
    createOpen = false;
    select(id);
    notice = 'Задача добавлена.';
    await reloadBoard();
    reloadRefs();
  }
</script>

<section class="journal tasks" data-board={board ?? undefined} use:observeWidth={(w) => (width = w)} bind:this={root}>
  <!-- широкая область — колонки и карточка-панель поверх доски, узкая — вкладки и карточка вместо
       доски (§5.2 п. 1) -->
  <JournalSplit
    open={detailOpen}
    mode="overlay"
    back={{ href: href({ name: 'journal', user, space, journal: 'tasks', entry: null }), label: JOURNAL_TITLE.tasks }}
    label="Задача"
    onclose={closeDetail}
    dirty={unsaved}
    {entry}
  >
    {#snippet list()}
      {@render boardView()}
    {/snippet}
    {#snippet detail()}
      {@render taskDetail()}
    {/snippet}
  </JournalSplit>
</section>

<Prompt bind:ask />

{#snippet boardView()}
  <!-- «Корзина» — одна кнопка в обоих видах: фокус на ней переживает переключение -->
  <div class="row bar">
    {#if !trash}
      <button type="button" class="primary" onclick={startCreate}>Новая задача</button>
      <button type="button" class="toggle" aria-pressed={fromIdea} onclick={toggleFromIdea} title="Только задачи из идей — пункты плана работ">Из идей</button>
      <!-- с «Из идей» шаги из идей и так на доске (пункты плана): переключатель нажат и неактивен
           (`aria-disabled` — остаётся в порядке Tab), причина — видимой строкой и его описанием -->
      <button
        type="button"
        class="toggle"
        aria-pressed={flat}
        aria-disabled={fromIdea}
        aria-describedby={fromIdea ? `${uid}-flat-why` : undefined}
        onclick={() => {
          if (!fromIdea) toggleSubtasks();
        }}
        title={fromIdea ? undefined : 'Шаги планов — в колонках по статусам, с пометкой «часть …»; выключено — под общей задачей'}
      >
        Подзадачи на доске
      </button>
      {#if fromIdea}<span class="muted small" id="{uid}-flat-why">с «Из идей» — все задачи из идей, и шаги тоже</span>{/if}
    {/if}
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  {#if trash}
    <TrashPanel {user} {space} kind="task" {onexpired} />
  {:else}
    {@render liveBoard()}
  {/if}
{/snippet}

{#snippet liveBoard()}
  <div class="board-notice"><Notice text={boardNotice} /></div>
  {#if boardError}<p class="error" role="alert">{boardError}</p>{/if}
  {#if total === 0 && !boardError}<p class="muted">{fromIdea ? 'Задач из идей нет.' : 'Задач нет.'}</p>{/if}

  <div class="board" role="group" aria-label="Доска задач">
    {#if board === 'columns'}
      <div class="columns">
        {#each TASK_COLUMNS as s (s)}
          <section class="col" aria-label="Задачи: {STATUS_TEXT[s]}" data-status={s}>
            <h2>{STATUS_TEXT[s]} <span class="muted small">{counts?.[s] ?? ''}</span></h2>
            {@render tasksOf(s)}
          </section>
        {/each}
      </div>
    {:else if board === 'tabs'}
      <StatusTabs label="Статусы задач" {tabs} selected={tab ?? ''} onselect={(id) => selectTab(id as EntryStatus)} noun={TASK_FORMS}>
        {#if tab}{@render tasksOf(tab)}{:else}<p class="muted small">Загрузка…</p>{/if}
      </StatusTabs>
    {/if}
  </div>
{/snippet}

{#snippet tasksOf(s: EntryStatus)}
  {@const c = cols[s]}
  {#if c.loading && !c.loaded}<p class="muted small">Загрузка…</p>{/if}
  {#if board === 'tabs' && c.loaded && c.items.length === 0 && total !== 0}<p class="muted small">Задач нет.</p>{/if}
  {#each c.items as t (t.id)}
    {@const claim = claimOf(t)}
    {@const staleRefs = staleOf(refs.get(entryRef('task', t.id)))}
    <article class="task" class:selected={t.id === entry} data-task={t.id} data-entry={t.id}>
      <button type="button" class="link title" onclick={() => select(t.id)}>{t.title}</button>
      {#if t.parent}
        <a class="small part-of" href={taskHref(t.parent.id)} onclick={() => (notice = '')} data-testid="task-part-of">
          часть {entryRef('task', t.parent.id)} «{t.parent.title}»
        </a>
      {/if}
      {#if t.idea}
        <a class="small from-idea" href={ideaHref(t, t.idea.id, t.idea.space_id)} data-testid="from-idea">
          из идеи {entryRef('idea', t.idea.id)} «{t.idea.title}»{#if (t.ideas?.length ?? 1) > 1}&nbsp;и ещё {(t.ideas?.length ?? 1) - 1}{/if}
        </a>
      {/if}
      {#if str(t.data, 'next_step')}<div class="small">Дальше: {str(t.data, 'next_step')}</div>{/if}
      {#if t.status === 'blocked' && str(t.data, 'blocked_by')}
        <div class="small blocked">Блокер: {str(t.data, 'blocked_by')}</div>
      {/if}
      {#if claim}<div class="claim small">{claimText(claim)}</div>{/if}
      {#if t.status === 'done' && t.duration_min !== undefined && t.duration_min !== null}
        <div class="muted small">длительность {durationText(t.duration_min)}</div>
      {/if}
      {#if t.stale && t.last_activity_at}{@render idle(t.last_activity_at)}{/if}
      {#if staleRefs.length}<div class="badge small">возможно устарело</div>{/if}
      {#if t.subtasks?.total}{@render subtasksOf(t, t.id)}{/if}
    </article>
  {/each}
  {#if c.next}
    <button type="button" class="more" aria-disabled={c.loading} aria-busy={c.loading} onclick={(e) => more(c, () => loadColumn(s, false), '[data-task]', e)}>
      Показать ещё
    </button>
  {/if}
{/snippet}

<!-- Счётчик, полоса прогресса и «Подзадачи ▸» общей задачи `t`; раскрытый список — под ними (путь
     `path` — раскрытие этого места доски). Строка подзадачи — статус, заголовок-ссылка, захват,
     «давно без обновлений»; вложенная общая — со своим счётчиком и раскрытием. -->
{#snippet subtasksOf(t: Entry, path: string)}
  {@const shown = !!unfolded[path]}
  {@const box = `${uid}-subs-${path.replaceAll('/', '-')}`}
  {@const c = subs[t.id]}
  <div class="subs-bar">
    {#if t.subtasks}{@render progress(t.subtasks)}{/if}
    <!-- имя — с заголовком общей: в списке кнопок экранного диктора раскрытия различимы -->
    <button
      type="button"
      class="link small fold"
      aria-label="Подзадачи: {t.title}"
      aria-expanded={shown}
      aria-controls={shown ? box : undefined}
      onclick={() => toggleSubs(path, t.id)}
    >
      Подзадачи <span aria-hidden="true">{shown ? '▾' : '▸'}</span>
    </button>
  </div>
  {#if shown}
    <div class="subtasks" id={box}>
      {#if c?.loading && !c.loaded}<p class="muted small">Загрузка…</p>{/if}
      {#if c?.error}<p class="error small" role="alert">{c.error}</p>{/if}
      {#if c?.loaded && c.items.length === 0}<p class="muted small">Подзадач нет.</p>{/if}
      {#if c?.items.length}
        <ol aria-label="Подзадачи «{t.title}»">
          {#each c.items as k (k.id)}
            {@const claim = claimOf(k)}
            <li class:selected={k.id === entry} data-subtask={k.id} data-entry={k.id}>
              <div class="sub-head">
                <span class="chip">{k.status ? STATUS_TEXT[k.status] : ''}</span>
                <a class="title" href={taskHref(k.id)} onclick={() => (notice = '')}>{k.title}</a>
              </div>
              {#if claim}<div class="claim small">{claimText(claim)}</div>{/if}
              {#if k.stale && k.last_activity_at}{@render idle(k.last_activity_at)}{/if}
              {#if k.subtasks?.total}{@render subtasksOf(k, `${path}/${k.id}`)}{/if}
            </li>
          {/each}
        </ol>
      {/if}
      {#if c?.next}
        <button
          type="button"
          class="more small"
          aria-disabled={c.loading}
          aria-busy={c.loading}
          onclick={(e) => more(c, () => loadSubs(t.id, false), ':scope > ol > li', e)}
        >
          Показать ещё
        </button>
      {/if}
    </div>
  {/if}
{/snippet}

<!-- «8/13» и полоса прогресса (`done/(total − dropped)`); подзадач без снятых нет — ничего. Число
     для чтения с экрана — в полосе («8 из 13»), видимое — не повторяется. -->
{#snippet progress(s: ProgressOf)}
  {@const v = progressValue(s)}
  {#if v.max > 0}
    <span class="progress-text" aria-hidden="true" data-testid="task-progress">{progressText(s)}</span>
    {@render progressBar(v)}
  {/if}
{/snippet}

{#snippet progressBar(v: { now: number; max: number })}
  <span
    class="progress"
    role="progressbar"
    aria-label="Выполнено подзадач"
    aria-valuemin={0}
    aria-valuemax={v.max}
    aria-valuenow={v.now}
    aria-valuetext="{v.now} из {v.max}"
  >
    <span class="progress-fill" style:width="{(100 * v.now) / v.max}%"></span>
  </span>
{/snippet}

<!-- «Давно без обновлений» (`stale` сервера): часы от `last_activity_at`, точное время — в подсказке. -->
{#snippet idle(last: string)}
  <div class="idle small" data-testid="task-idle" title="последнее обновление — {fmtTime(last)}">{staleText(last, Date.now())}</div>
{/snippet}

{#snippet taskDetail()}
  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const claim = claimOf(c)}
    <article class="card panel" aria-label="Карточка задачи">
      <!-- шапка: заголовок, статус и короткие строки состояния (часть, захват, блокер, выполнено) -->
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status">{c.status ? STATUS_TEXT[c.status] : ''}</span>
          <span class="muted small">{AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
        {#if c.stale && c.last_activity_at}{@render idle(c.last_activity_at)}{/if}
        {#if c.parent}
          {@const pp = progressText(c.parent.subtasks)}
          <p class="part-of" data-testid="task-parent">
            Часть: <a href={taskHref(c.parent.id)} onclick={() => (notice = '')}>{entryRef('task', c.parent.id)} «{c.parent.title}»</a>{pp ? ` (${pp})` : ''}
          </p>
        {/if}
        {#if claim}<p class="claim" data-testid="claim">{claimText(claim)}</p>{/if}
        {#if c.status === 'blocked' && str(c.data, 'blocked_by')}<p class="blocked">Блокер: {str(c.data, 'blocked_by')}</p>{/if}
        {#if c.status === 'done'}
          <p class="muted">
            Выполнено {fmtTime(str(c.data, 'done_at'))}{#if c.duration_min !== undefined && c.duration_min !== null}, длительность {durationText(c.duration_min)}{/if}
          </p>
        {/if}
      </header>

      {#if mode === 'edit'}
        <!-- правка на месте: форма вместо вида карточки (§6.14 п. 3) -->
        <EditPanel bind:this={editor} {user} entry={c} onsaved={edited} oncancel={cancelEdit} onreread={reread} {onexpired} />
      {:else}
        <!-- ряды действий и поле шага — сразу под шапкой, до длинных разделов (правило Г) -->
        <div class="row actions" role="group" aria-label="Смена статуса">
          {#each targets(c) as s (s)}
            <button type="button" onclick={() => pressStatus(s)} disabled={busy}>
              {c.status === 'done' || c.status === 'dropped' ? 'Переоткрыть' : `→ ${STATUS_TEXT[s]}`}
            </button>
          {/each}
          {#if claim}<button type="button" class="danger" onclick={release} disabled={busy}>Снять захват</button>{/if}
        </div>
        <div class="row actions" role="group" aria-label="Правка задачи">
          <button type="button" onclick={startEdit} disabled={busy}>Изменить</button>
          <button type="button" class="danger" onclick={() => askDelete(c)} disabled={busy}>Удалить</button>
        </div>
        <div class="card-notice"><Notice text={notice} /></div>
        {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

        {#if c.status !== 'done' && c.status !== 'dropped'}
          <!-- поле на виду (правило А): видимая подпись над полем, «Сохранить шаг» — рядом -->
          <form class="step" onsubmit={saveStep}>
            <label for="{uid}-step">Следующий шаг</label>
            <div class="row">
              <input id="{uid}-step" bind:value={stepDraft} placeholder="что сделать дальше" maxlength="2000" />
              <button type="submit" disabled={busy}>Сохранить шаг</button>
            </div>
          </form>
        {:else if str(c.data, 'next_step')}
          <p class="muted">Дальше было: {str(c.data, 'next_step')}</p>
        {/if}

        {#if c.body}<p class="body">{c.body}</p>{/if}
        {#if c.ideas?.length}
          <div class="ideas" data-testid="task-ideas">
            <span class="muted">{c.ideas.length > 1 ? 'Из идей:' : 'Из идеи:'}</span>
            {#each c.ideas as i (i.id)}
              <a href={ideaHref(c, i.id, i.space_id)}>{entryRef('idea', i.id)} «{i.title}»</a>
              {#if i.status}<span class="chip">{entryStatusText('idea', i.status)}</span>{/if}
            {/each}
          </div>
        {/if}
        {#if c.subtasks?.total}
          {@const s = c.subtasks}
          {@const pt = progressText(s)}
          {@const items = s.items ?? []}
          <!-- раздел «Подзадачи 8/13»: шаги в порядке связей (`subtasks.items`, до 200) -->
          <section class="subtasks-card" aria-labelledby="{uid}-subs-title" data-testid="task-subtasks">
            <h3 id="{uid}-subs-title">Подзадачи{pt ? ` ${pt}` : ''}</h3>
            {#if pt}{@render progressBar(progressValue(s))}{/if}
            {#if items.length}
              <ol>
                {#each items as k (k.id)}
                  <li>
                    <span class="chip">{k.status ? STATUS_TEXT[k.status] : ''}</span>
                    <a href={taskHref(k.id)} onclick={() => (notice = '')}>{k.title}</a>
                    {#if k.claimed}
                      <span class="claim small" data-testid="subtask-claim" title="до {fmtTime(k.claimed.until)}">в работе у {k.claimed.agent || 'агент'}</span>
                    {/if}
                  </li>
                {/each}
              </ol>
            {/if}
            {#if s.truncated}
              <p class="muted small">… и ещё {s.total - items.length}: полный список — «Подзадачи» под задачей на доске.</p>
            {/if}
          </section>
        {/if}
        <FileRefs refs={refs.get(entryRef('task', c.id)) ?? []} />
      {/if}
    </article>
  {/if}
{/snippet}

<!-- «Новая задача» — окно (§6.14, правило Б); после «Добавить» — карточка созданной (п. 6) -->
<FormDialog
  open={createOpen}
  title="Новая задача"
  submit="Добавить"
  error={dlgError}
  dirty={createDirty}
  onsubmit={submitCreate}
  onclose={() => ((createOpen = false), (dlgError = ''))}
>
  <label>Заголовок <input bind:value={nTitle} required maxlength="300" data-autofocus /></label>
  <label>Описание <textarea bind:value={nBody} rows="4"></textarea></label>
  <label>Следующий шаг <input bind:value={nStep} maxlength="2000" /></label>
</FormDialog>

<!-- смена статуса: переоткрытие и «заблокирован» — окно с полем, конечный статус — подтверждение
     (решение 84) -->
<FormDialog
  open={statusDlg !== null}
  title={statusTitle(statusDlg)}
  subject={cur?.title}
  description={statusDescription(statusDlg)}
  submit={statusSubmit(statusDlg)}
  confirm={statusDlg?.kind === 'final'}
  danger={statusDlg?.to === 'dropped'}
  error={dlgError}
  dirty={statusDirty}
  refocus={noticeEl}
  onsubmit={submitStatus}
  onclose={() => ((statusDlg = null), (dlgError = ''))}
>
  {#if statusDlg?.kind === 'reopen'}
    <label class:clash={statusClash.includes('reason')}
      >Причина переоткрытия {#if statusClash.includes('reason')}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
      <textarea bind:value={sReason} rows="3" required maxlength="2000" data-autofocus></textarea></label
    >
  {:else if statusDlg?.kind === 'blocked'}
    <label class:clash={statusClash.includes('blocked')}
      >Что мешает <span class="muted small">(необязательно)</span>
      {#if statusClash.includes('blocked')}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
      <input bind:value={sBlocked} maxlength="2000" data-autofocus /></label
    >
  {/if}
</FormDialog>

<style>
  .bar {
    margin-bottom: 8px;
  }
  /* нажатый переключатель — `button.toggle[aria-pressed='true']` (app.css) */
  .toggle[aria-disabled='true'] {
    /* как у прочих неактивных кнопок (`button:disabled`, app.css) */
    opacity: 0.5;
    cursor: default;
  }
  .from-idea,
  .part-of {
    overflow-wrap: anywhere;
  }
  /* подзадачи (§5.5): счётчик, полоса прогресса и раскрытие под плиткой */
  .subs-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .progress-text {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  .progress {
    display: block;
    flex: 1 1 48px;
    max-width: 120px;
    height: 6px;
    border-radius: 3px;
    background: var(--border);
    overflow: hidden;
  }
  .progress-fill {
    display: block;
    height: 100%;
    background: var(--callout-tip);
  }
  .subtasks-card .progress {
    margin: 4px 0 8px;
  }
  .subtasks {
    min-width: 0;
    padding-left: 8px;
    border-left: 2px solid var(--border);
  }
  .subtasks ol,
  .subtasks-card ol {
    display: grid;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .subtasks li {
    display: grid;
    gap: 2px;
    min-width: 0;
    padding: 2px 4px;
    border-radius: 4px;
  }
  .subtasks li.selected {
    background: var(--selected);
  }
  .sub-head,
  .subtasks-card li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 6px;
    overflow-wrap: anywhere;
  }
  .subtasks-card {
    margin-top: 12px;
  }
  .subtasks-card h3 {
    margin: 0;
    font-size: 15px;
  }
  .idle {
    justify-self: start;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  header .idle {
    display: inline-block;
    margin-top: 4px;
  }
  /* ссылки строк подзадач — цели касания 44 px, как кнопки (§5.1 п. 2) */
  @media (pointer: coarse) {
    .sub-head a,
    .subtasks-card li a {
      display: inline-flex;
      align-items: center;
      min-height: 44px;
    }
  }
  .ideas {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    overflow-wrap: anywhere;
  }
  .columns {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 10px;
    align-items: start;
  }
  .col {
    min-width: 0;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .col h2 {
    margin: 0 0 8px;
    font-size: 14px;
  }
  .task {
    display: grid;
    gap: 2px;
    margin-bottom: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .task.selected {
    background: var(--selected);
  }
  .more {
    margin-top: 2px;
  }
  .more[aria-disabled='true'] {
    opacity: 0.5;
    cursor: default;
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
  .claim {
    color: var(--callout-tip);
  }
  .blocked {
    color: var(--danger);
  }
  .badge {
    justify-self: start;
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
  .meta {
    gap: 6px;
  }
  .actions {
    margin-top: 12px;
  }
  .actions + .actions {
    margin-top: 8px;
  }
  header p {
    margin: 4px 0 0;
  }
  .card-notice :global(.notice:not(:empty)) {
    margin: 8px 0 0;
  }
  .step {
    margin-top: 12px;
  }
  .small {
    font-size: 12px;
  }
  /* в панели и на телефоне кнопка переносится под поле, а не сжимает его */
  .step label {
    display: block;
    margin-bottom: 4px;
    font-size: 14px;
  }
  .step input {
    flex: 1 1 200px;
    min-width: 0;
  }
</style>
