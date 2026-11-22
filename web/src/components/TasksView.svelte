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
  import { tick, untrack } from 'svelte';
  import FileRefs from './FileRefs.svelte';
  import JournalSplit from './JournalSplit.svelte';
  import StatusTabs from './StatusTabs.svelte';
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
  type Mode = 'none' | 'create' | 'status';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');

  let nStep = $state('');
  let nTitle = $state('');
  let nBody = $state('');
  let toStatus = $state<EntryStatus>('open');
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
        tab = null;
        tabFor = '';
        resetBoard();
        reloadRefs();
        return;
      }
      if (countsFailed) loadCounts();
      for (const st of visible) if (cols[st].error) loadColumn(st, true);
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
      tab = boardTab(c, e?.status ?? null);
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

  /** Всё заново: числа и колонки (видимые загрузит эффект выше). */
  function resetBoard() {
    for (const s of TASK_COLUMNS) cseq[s] = (cseq[s] ?? 0) + 1;
    cols = emptyColumns();
    counts = null;
    countsFailed = false;
    loadCounts();
  }

  async function loadCounts() {
    const my = ++kseq;
    try {
      const c = await entryCounts(user, space, { kind: 'task', fromIdea });
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

  /** После правки: числа и видимые статусы — заново (задача могла сменить колонку), столько же
   * задач, сколько было загружено «Показать ещё» (до PAGE_MAX); прочие вкладки — при следующем
   * выборе. */
  async function reloadBoard() {
    const vis = visible;
    for (const s of TASK_COLUMNS) {
      if (vis.includes(s)) continue;
      cseq[s] = (cseq[s] ?? 0) + 1;
      cols[s] = emptyColumn();
    }
    const size = (s: EntryStatus) => Math.min(PAGE_MAX, Math.max(PAGE, cols[s].items.length));
    await Promise.all([loadCounts(), ...vis.map((s) => loadColumn(s, true, size(s)))]);
  }

  /** «Показать ещё» колонки или вкладки. Кнопка на время загрузки остаётся (`aria-disabled`),
   * фокус на ней цел; страниц больше нет и кнопка ушла — фокус на первую догруженную задачу. */
  async function more(s: EntryStatus, e: MouseEvent) {
    const c = cols[s];
    if (c.loading) return;
    const btn = e.currentTarget as HTMLElement;
    const box = btn.parentElement;
    const first = c.items.length;
    await loadColumn(s, false);
    await tick();
    const a = document.activeElement;
    if (btn.isConnected || (a && a !== document.body)) return;
    box?.querySelectorAll<HTMLElement>('[data-task]')[first]?.querySelector<HTMLElement>('button')?.focus();
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
      if (e.id !== tabFor && e.status && TASK_COLUMNS.includes(e.status)) {
        tabFor = e.id;
        tab = e.status;
      }
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
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

  /** Ссылка на идею задачи: идея в пространстве задачи — в нём же, иначе — Knowledge (задача
   * «начать» общей идеи бывает в решении той же базы). */
  const ideaHref = (t: Entry, id: string, ideaSpace: string) =>
    href({ name: 'journal', user, space: ideaSpace === t.space_id ? space : 'knowledge', journal: 'ideas', entry: id });

  /** Открыть задачу: новая запись истории — «назад» браузера возвращает к доске (§5.1 п. 5). */
  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'tasks', entry: id });
  }

  /** Карточка или форма показана (панелью поверх доски или вместо неё). */
  let detailOpen = $derived(cur !== null || curError !== '' || mode === 'create');

  /** Закрыли карточку («← Задачи», фон, Esc): форма новой задачи закрывается здесь. */
  function closeDetail() {
    mode = 'none';
    actionError = '';
  }

  /** Несохранённое в панели (вопрос перед закрытием): поля — так, как уйдут при отправке
   * (обрезанные), против сохранённого в задаче. После сохранения и перечитывания карточки (`open`
   * берёт `next_step` сервера) — пусто; после неудачной отправки набранное остаётся несохранённым. */
  function unsaved(): boolean {
    if (mode === 'create') return !!(nTitle.trim() || nBody.trim() || nStep.trim());
    const c = cur;
    if (!c) return false;
    if (mode === 'status') {
      if (sReason.trim()) return true;
      if (toStatus === 'blocked' && sBlocked.trim() !== str(c.data, 'blocked_by')) return true;
    }
    return c.status !== 'done' && c.status !== 'dropped' && stepDraft.trim() !== str(c.data, 'next_step');
  }

  async function act(fn: () => Promise<string | void>, done: string) {
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
    } catch (e) {
      if (isStaleEntry(e) && cur) {
        const msg = errorText(e);
        await open(user, cur.id);
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

  function startStatus(s: EntryStatus) {
    toStatus = s;
    sReason = '';
    sBlocked = cur ? str(cur.data, 'blocked_by') : '';
    actionError = '';
    mode = 'status';
  }

  function submitStatus(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    const reopen = c.status === 'done' || c.status === 'dropped';
    if (reopen && !sReason.trim()) {
      actionError = 'Переоткрытие требует причины.';
      return;
    }
    act(async () => {
      await patchEntry(user, c.id, c.rev, {
        status: toStatus,
        ...(reopen ? { reason: sReason.trim() } : {}),
        ...(toStatus === 'blocked' && sBlocked.trim() ? { blocked_by: sBlocked.trim() } : {}),
      });
    }, `Статус: ${STATUS_TEXT[toStatus]}.`);
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
    act(async () => {
      await releaseTask(user, c.id);
    }, 'Захват снят.');
  }

  function startCreate() {
    nTitle = '';
    nBody = '';
    nStep = '';
    actionError = '';
    mode = 'create';
  }

  function submitCreate(e: SubmitEvent) {
    e.preventDefault();
    const title = nTitle.trim();
    if (!title) {
      actionError = 'Укажите заголовок.';
      return;
    }
    act(
      async () =>
        (
          await createEntry(user, space, {
            kind: 'task',
            title,
            body: nBody,
            data: nStep.trim() ? { next_step: nStep.trim() } : {},
          })
        ).id,
      'Задача добавлена.',
    );
  }
</script>

<section class="journal tasks" data-board={board ?? undefined} use:observeWidth={(w) => (width = w)}>
  <!-- широкая область — колонки и карточка-панель поверх доски, узкая — вкладки и карточка вместо
       доски (§5.2 п. 1) -->
  <JournalSplit
    open={detailOpen}
    mode="overlay"
    back={{ href: href({ name: 'journal', user, space, journal: 'tasks', entry: null }), label: JOURNAL_TITLE.tasks }}
    label={mode === 'create' && !cur ? 'Новая задача' : 'Задача'}
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

{#snippet boardView()}
  <div class="row bar">
    <button type="button" class="primary" onclick={startCreate}>Новая задача</button>
    <button type="button" class="toggle" aria-pressed={fromIdea} onclick={toggleFromIdea} title="Только задачи из идей — пункты плана работ">Из идей</button>
  </div>

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
    {@const stale = staleOf(refs.get(entryRef('task', t.id)))}
    <article class="task" class:selected={t.id === entry} data-task={t.id} data-entry={t.id}>
      <button type="button" class="link title" onclick={() => select(t.id)}>{t.title}</button>
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
      {#if stale.length}<div class="badge small">возможно устарело</div>{/if}
    </article>
  {/each}
  {#if c.next}
    <button type="button" class="more" aria-disabled={c.loading} aria-busy={c.loading} onclick={(e) => more(s, e)}>Показать ещё</button>
  {/if}
{/snippet}

{#snippet taskDetail()}
  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const claim = claimOf(c)}
    <article class="card panel" aria-label="Карточка задачи">
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status">{c.status ? STATUS_TEXT[c.status] : ''}</span>
          <span class="muted small">{AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>
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
      {#if claim}
        <p class="claim" data-testid="claim">{claimText(claim)}</p>
      {/if}
      {#if c.status === 'blocked' && str(c.data, 'blocked_by')}<p class="blocked">Блокер: {str(c.data, 'blocked_by')}</p>{/if}
      {#if c.status === 'done'}
        <p class="muted">
          Выполнено {fmtTime(str(c.data, 'done_at'))}{#if c.duration_min !== undefined && c.duration_min !== null}, длительность {durationText(c.duration_min)}{/if}
        </p>
      {/if}
      <FileRefs refs={refs.get(entryRef('task', c.id)) ?? []} />

      {#if c.status !== 'done' && c.status !== 'dropped'}
        <form class="row" onsubmit={saveStep}>
          <input bind:value={stepDraft} placeholder="следующий шаг" aria-label="Следующий шаг" maxlength="2000" />
          <button type="submit" disabled={busy}>Сохранить шаг</button>
        </form>
      {:else if str(c.data, 'next_step')}
        <p class="muted">Дальше было: {str(c.data, 'next_step')}</p>
      {/if}

      <div class="row actions" role="group" aria-label="Смена статуса">
        {#each targets(c) as s (s)}
          <button type="button" onclick={() => startStatus(s)} disabled={busy}>
            {c.status === 'done' || c.status === 'dropped' ? 'Переоткрыть' : `→ ${STATUS_TEXT[s]}`}
          </button>
        {/each}
        {#if claim}<button type="button" class="danger" onclick={release} disabled={busy}>Снять захват</button>{/if}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}

      {#if mode === 'status'}
        <form class="form" onsubmit={submitStatus} aria-label="Смена статуса задачи">
          <h3>Статус: {STATUS_TEXT[toStatus]}</h3>
          {#if c.status === 'done' || c.status === 'dropped'}
            <label>Причина переоткрытия <textarea bind:value={sReason} rows="2" required></textarea></label>
          {/if}
          {#if toStatus === 'blocked'}
            <label>Что мешает <input bind:value={sBlocked} maxlength="2000" /></label>
          {/if}
          {@render formEnd('Применить')}
        </form>
      {:else if actionError}
        <p class="error" role="alert">{actionError}</p>
      {/if}
    </article>
  {/if}

  {#if mode === 'create'}
    <form class="form panel" onsubmit={submitCreate} aria-label="Новая задача">
      <h3>Новая задача</h3>
      <label>Заголовок <input bind:value={nTitle} required maxlength="300" /></label>
      <label>Описание <textarea bind:value={nBody} rows="3"></textarea></label>
      <label>Следующий шаг <input bind:value={nStep} maxlength="2000" /></label>
      {@render formEnd('Добавить')}
    </form>
  {/if}
{/snippet}

{#snippet formEnd(label: string)}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>{label}</button>
    <button type="button" onclick={() => ((mode = 'none'), (actionError = ''))}>Отмена</button>
  </div>
{/snippet}

<style>
  .bar {
    margin-bottom: 8px;
  }
  .toggle[aria-pressed='true'] {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--selected);
  }
  .from-idea {
    overflow-wrap: anywhere;
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
  .form {
    display: grid;
    gap: 8px;
    margin-top: 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .form h3 {
    margin: 0;
    font-size: 15px;
  }
  .form label {
    display: grid;
    gap: 4px;
  }
  .form textarea {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 12px;
  }
  /* в панели и на телефоне кнопка переносится под поле, а не сжимает его */
  input[aria-label='Следующий шаг'] {
    flex: 1 1 200px;
    min-width: 0;
  }
</style>
