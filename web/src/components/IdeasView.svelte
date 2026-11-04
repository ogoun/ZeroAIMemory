<script lang="ts">
  // «Идеи» (M5.7 §10): замыслы человека и агента. Вкладки по происхождению («Мои», «От агента»,
  // «Все» — шаблон ARIA tabs, как у «Кода»), фильтры статуса (по умолчанию активные), приоритета и
  // тега — на сервере, курсор по 50; поиск по заголовку и тексту — `POST /search kinds=[idea]`.
  // Карточка: «Начать» (decide accept — задача и ссылка на неё; у идеи Knowledge — выбор
  // решения с предупреждением об общем), «Отложить» (дата UTC), «Отказаться», «Вернуть»,
  // «Принять как свою» (у «от агента» «Начать» нет до неё), «Изменить» (PATCH с If-Match,
  // 409 — перечитать). Предлагает идею агент (propose): у WebUI этого действия нет. Ленивый чанк.
  import { untrack } from 'svelte';
  import {
    ApiError,
    adoptIdea,
    createEntry,
    decideIdea,
    errorText,
    getEntry,
    listEntries,
    patchEntry,
    searchIdeas,
    spaceRef,
    type Entry,
    type EntryStatus,
    type SearchItem,
    type Space,
  } from '../lib/api';
  import {
    AUTHOR_TEXT,
    IDEA_ACTIVE,
    IDEA_STATUSES,
    IDEA_STATUS_TEXT,
    PRIORITIES,
    PRIORITY_TEXT,
    entryRef,
    fmtTime,
    utcDate,
    type IdeaPriority,
  } from '../lib/journals';
  import { snippetParts } from '../lib/snippet';
  import { go, href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    /** Пространства пользователя (`GET /spaces`): решения для «Начать» у идеи Knowledge. */
    spaces: Space[];
    onexpired: () => void;
  }

  let { user, space, entry, spaces, onexpired }: Props = $props();

  const PAGE = 50;

  /** `brief` — элемент на чанк: идея с совпадениями в нескольких местах — один раз. */
  function uniqueHits(xs: SearchItem[]): SearchItem[] {
    const seen = new Set<string>();
    return xs.filter((x) => x.kind === 'idea' && x.entry_id && !seen.has(x.entry_id) && !!seen.add(x.entry_id));
  }
  const uid = $props.id();

  type Tab = 'human' | 'agent' | 'all';
  const TABS: { id: Tab; title: string }[] = [
    { id: 'human', title: 'Мои' },
    { id: 'agent', title: 'От агента' },
    { id: 'all', title: 'Все' },
  ];
  type StatusSel = 'active' | 'accepted' | 'done' | 'declined' | 'all';
  const STATUS_SEL: { id: StatusSel; title: string; statuses: EntryStatus[] }[] = [
    { id: 'active', title: 'Активные', statuses: IDEA_ACTIVE },
    { id: 'accepted', title: IDEA_STATUS_TEXT.accepted, statuses: ['accepted'] },
    { id: 'done', title: IDEA_STATUS_TEXT.done, statuses: ['done'] },
    { id: 'declined', title: IDEA_STATUS_TEXT.declined, statuses: ['declined'] },
    { id: 'all', title: 'все', statuses: [] },
  ];
  const statusesOf = (s: StatusSel) => STATUS_SEL.find((x) => x.id === s)?.statuses ?? [];

  let tab = $state<Tab>('human');
  let tabEls = $state<HTMLButtonElement[]>([]);
  let statusSel = $state<StatusSel>('active');
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

  let cur = $state<Entry | null>(null);
  /** Открытый вопрос-предложение идеи (`proposed`): текст агента и `data.task_space`. */
  let curQuestion = $state<Entry | null>(null);
  /** Ссылка на пространство задачи идеи (у идеи Knowledge задача может быть в решении). */
  let taskSpace = $state<string | null>(null);
  let curError = $state('');
  let eseq = 0;

  type Mode = 'none' | 'create' | 'edit' | 'start' | 'defer' | 'decline' | 'reopen';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');

  // форма создания и правки
  let fTitle = $state('');
  let fBody = $state('');
  let fPriority = $state<IdeaPriority>('normal');
  let fHorizon = $state('');
  let fTags = $state('');
  /** `horizon` идеи до правки: неизменённая (в том числе наступившая) дата не ограничивается min. */
  let fHorizonWas = $state('');
  // формы решений
  let dHorizon = $state('');
  let dReason = $state('');
  let dSpace = $state('');

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  /** Пространство, для которого заданы фильтры: другое — фильтры и выдача сбрасываются. */
  let filtersFor = '';
  $effect(() => {
    const key = JSON.stringify([user, space]);
    if (key === filtersFor) return;
    filtersFor = key;
    untrack(() => {
      seq++;
      items = [];
      hits = null;
      next = null;
      tab = 'human';
      statusSel = 'active';
      priority = '';
      tag = '';
      query = '';
      applied = { tag: '', query: '' };
      load(user, space, true);
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  const str = (d: Record<string, unknown>, k: string) => (typeof d[k] === 'string' ? (d[k] as string) : '');
  const originOf = (e: Entry) => (str(e.data, 'origin') === 'agent' ? 'agent' : 'human');
  const priorityOf = (e: Entry): IdeaPriority => {
    const p = str(e.data, 'priority');
    return p === 'low' || p === 'high' ? p : 'normal';
  };
  const statusText = (s: EntryStatus | null) => (s && s in IDEA_STATUS_TEXT ? IDEA_STATUS_TEXT[s as keyof typeof IDEA_STATUS_TEXT] : (s ?? ''));

  /** Пространство идеи — Knowledge (задачу «Начать» можно поставить в своё решение). */
  let knowledge = $derived(space === 'knowledge' || spaces.find((s) => s.id === space)?.kind === 'knowledge');
  /** Решения той же базы, куда можно поставить задачу идеи Knowledge (свои, не чужие). */
  let targets = $derived(spaces.filter((s) => s.kind === 'solution' && s.role !== 'member'));

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    const statuses = statusesOf(statusSel);
    try {
      if (applied.query) {
        // «все» — явным перечнем: без статуса сервер берёт умолчание поиска (без declined, done)
        const r = await searchIdeas(u, s, applied.query, statuses.length ? statuses : IDEA_STATUSES);
        if (my !== seq) return;
        hits = uniqueHits(r.items);
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

  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'ideas', entry: id }, { replace: true });
  }

  function selectTab(t: Tab) {
    if (tab === t) return;
    tab = t;
    load(user, space, true);
  }

  /** Клавиатура tablist: ←/→ по кругу, Home, End; выбор следует за фокусом. */
  function ontabkey(e: KeyboardEvent, i: number) {
    const n = TABS.length;
    let j = -1;
    if (e.key === 'ArrowRight') j = (i + 1) % n;
    else if (e.key === 'ArrowLeft') j = (i - 1 + n) % n;
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = n - 1;
    if (j < 0) return;
    e.preventDefault();
    selectTab(TABS[j].id);
    tabEls[j]?.focus();
  }

  function apply(e?: SubmitEvent) {
    e?.preventDefault();
    applied = { tag: tag.trim(), query: query.trim() };
    load(user, space, true);
  }

  function reset() {
    tag = '';
    query = '';
    applied = { tag: '', query: '' };
    load(user, space, true);
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

  const tagsOf = (s: string) =>
    s
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean);

  function startCreate() {
    fTitle = '';
    fBody = '';
    fPriority = 'normal';
    fHorizon = '';
    fHorizonWas = '';
    fTags = '';
    actionError = '';
    mode = 'create';
  }

  function submitCreate(e: SubmitEvent) {
    e.preventDefault();
    const title = fTitle.trim();
    if (!title) {
      actionError = 'Укажите заголовок.';
      return;
    }
    act(
      async () =>
        (
          await createEntry(user, space, {
            kind: 'idea',
            title,
            body: fBody,
            tags: tagsOf(fTags),
            data: { priority: fPriority, ...(fHorizon ? { horizon: fHorizon } : {}) },
          })
        ).id,
      'Идея записана.',
    );
  }

  function startEdit(c: Entry) {
    fTitle = c.title;
    fBody = c.body;
    fPriority = priorityOf(c);
    fHorizon = str(c.data, 'horizon');
    fHorizonWas = fHorizon;
    fTags = c.tags.join(', ');
    actionError = '';
    mode = 'edit';
  }

  function submitEdit(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    const title = fTitle.trim();
    if (!title) {
      actionError = 'Укажите заголовок.';
      return;
    }
    // в data — только изменённое: прежний horizon мог наступить, и сервер отверг бы его
    const data: Record<string, unknown> = {};
    if (fPriority !== priorityOf(c)) data.priority = fPriority;
    if (fHorizon !== str(c.data, 'horizon')) data.horizon = fHorizon || null;
    act(async () => {
      await patchEntry(user, c.id, c.rev, {
        title,
        body: fBody,
        tags: tagsOf(fTags),
        ...(Object.keys(data).length ? { data } : {}),
      });
    }, 'Идея сохранена.');
  }

  /** Решение по идее; `accept` — ещё и ссылка на созданную задачу. */
  function decide(c: Entry, decision: 'accept' | 'defer' | 'decline' | 'reopen', opts: { horizon?: string; reason?: string; space?: string } = {}) {
    let task = '';
    act(
      async () => {
        const r = await decideIdea(user, c.id, decision, opts);
        if (r.task) task = r.task.id;
      },
      () =>
        decision === 'accept'
          ? `Идея начата: задача ${entryRef('task', task)}.`
          : decision === 'defer'
            ? `Идея отложена до ${opts.horizon} (UTC).`
            : decision === 'decline'
              ? 'Отказ записан.'
              : 'Идея возвращена в новые.',
    );
  }

  /** Решение для задачи по умолчанию: `data.task_space` вопроса-предложения или Knowledge. */
  function defaultTarget(): string {
    const ts = curQuestion ? str(curQuestion.data, 'task_space') : '';
    const sp = ts ? spaces.find((s) => s.id === ts) : null;
    return sp && sp.kind === 'solution' ? spaceRef(sp) : 'knowledge';
  }

  function startStart(c: Entry) {
    if (!knowledge) return decide(c, 'accept');
    dSpace = defaultTarget();
    actionError = '';
    mode = 'start';
  }

  let chosen = $derived(targets.find((s) => spaceRef(s) === dSpace) ?? null);

  function submitStart(e: SubmitEvent) {
    e.preventDefault();
    if (cur) decide(cur, 'accept', { space: dSpace });
  }

  function startDecision(m: 'defer' | 'decline' | 'reopen') {
    dHorizon = utcDate(30);
    dReason = '';
    actionError = '';
    mode = m;
  }

  function submitDefer(e: SubmitEvent) {
    e.preventDefault();
    if (!cur) return;
    if (!dHorizon || dHorizon < utcDate(1)) {
      actionError = `Дата — не раньше ${utcDate(1)} (UTC).`;
      return;
    }
    decide(cur, 'defer', { horizon: dHorizon });
  }

  function submitDecline(e: SubmitEvent) {
    e.preventDefault();
    if (cur) decide(cur, 'decline', dReason.trim() ? { reason: dReason.trim() } : {});
  }

  function submitReopen(e: SubmitEvent) {
    e.preventDefault();
    if (!cur) return;
    if (!dReason.trim()) {
      actionError = 'Укажите причину.';
      return;
    }
    decide(cur, 'reopen', { reason: dReason.trim() });
  }

  function adopt(c: Entry) {
    act(async () => {
      await adoptIdea(user, c.id);
    }, 'Идея принята как своя.');
  }

  const ACTIVE = new Set<string>(IDEA_ACTIVE);
  /** Задача «начать» снята или удалена: идея `accepted` без живой задачи (§5). */
  const released = (c: Entry): '' | 'снята' | 'удалена' =>
    c.status !== 'accepted' || !c.task ? '' : c.task.deleted ? 'удалена' : c.task.status === 'dropped' ? 'снята' : '';

  const taskHref = (id: string, sp = taskSpace ?? space) => href({ name: 'journal', user, space: sp, journal: 'tasks', entry: id });
  const questionHref = (id: string) => href({ name: 'journal', user, space, journal: 'questions', entry: id });
  const minDate = () => utcDate(1);
  let filtered = $derived(!!(applied.tag || applied.query));
</script>

<section class="journal ideas">
  <div class="row bar">
    <button type="button" class="primary" onclick={startCreate}>Новая идея</button>
  </div>

  {#if mode === 'create'}
    <form class="form panel" onsubmit={submitCreate} aria-label="Новая идея">
      <h3>Новая идея</h3>
      {@render fields()}
      {@render formEnd('Записать')}
    </form>
  {/if}

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
    <form class="filters row" onsubmit={apply} aria-label="Фильтры идей">
      <label class="row">
        Статус
        <select bind:value={statusSel} onchange={() => load(user, space, true)} aria-label="Статус идей">
          {#each STATUS_SEL as s (s.id)}<option value={s.id}>{s.title}</option>{/each}
        </select>
      </label>
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
      {#if filtered}<button type="button" onclick={reset}>Сбросить</button>{/if}
    </form>
    {#if applied.query}<p class="muted small">Поиск учитывает статус; вкладка, приоритет и тег к нему не применяются.</p>{/if}

    {#if error}<p class="error" role="alert">{error}</p>{/if}

    {#if hits}
      <div class="feed" role="feed" aria-label="Найденные идеи" aria-busy={loading}>
        {#each hits as h (h.entry_id)}
          <article class="idea" class:selected={h.entry_id === entry} data-idea={h.entry_id}>
            <button type="button" class="link title" onclick={() => select(h.entry_id ?? '')}>{h.title ?? ''}</button>
            {#if h.status}<span class="chip">{statusText(h.status)}</span>{/if}
            <p class="snippet small">
              {#each snippetParts(h.snippet ?? '') as p, j (j)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
            </p>
          </article>
        {/each}
      </div>
      {#if !loading && hits.length === 0 && !error}<p class="muted">Ничего не найдено.</p>{/if}
    {:else}
      <div class="feed" role="feed" aria-label="Лента идей" aria-busy={loading}>
        {#each items as it (it.id)}
          {@const task = str(it.data, 'task_id')}
          {@const q = str(it.data, 'question_id')}
          <article class="idea" class:selected={it.id === entry} data-idea={it.id}>
            <button type="button" class="link title" onclick={() => select(it.id)}>{it.title}</button>
            <span class="chip prio" data-priority={priorityOf(it)}>{PRIORITY_TEXT[priorityOf(it)]}</span>
            <span class="chip">{statusText(it.status)}</span>
            {#if it.status === 'deferred' && str(it.data, 'horizon')}<span class="muted small">не раньше {str(it.data, 'horizon')} UTC</span>{/if}
            {#if originOf(it) === 'agent'}<span class="badge small">от агента</span>{/if}
            {#if task && !knowledge}<a class="small" href={taskHref(task, space)}>задача {entryRef('task', task)}</a>
            {:else if task}<button type="button" class="link small" onclick={() => select(it.id)} title="Задача может быть в решении: ссылка — в карточке">задача {entryRef('task', task)}</button>
            {:else if q && it.status === 'proposed'}<a class="small" href={questionHref(q)}>вопрос {entryRef('question', q)}</a>{/if}
            {#each it.tags as t (t)}<span class="chip">#{t}</span>{/each}
          </article>
        {/each}
      </div>
      {#if !loading && items.length === 0 && !error}<p class="muted">{filtered ? 'Идей по выбранным фильтрам нет.' : 'Идей нет.'}</p>{/if}
      {#if next && !loading}
        <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
      {/if}
    {/if}
    {#if loading}<p class="muted">Загрузка…</p>{/if}
  </div>

  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const agent = originOf(c) === 'agent'}
    {@const rel = released(c)}
    {@const active = ACTIVE.has(c.status ?? '')}
    <article class="card panel" aria-label="Карточка идеи" data-idea-card={c.id}>
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status" data-status={c.status}>{statusText(c.status)}</span>
          <span class="chip prio">приоритет: {PRIORITY_TEXT[priorityOf(c)]}</span>
          {#if agent}<span class="badge small">от агента</span>{/if}
          <span class="muted small">{entryRef('idea', c.id)} · {AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>
      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if c.tags.length}<div class="row">{#each c.tags as t (t)}<span class="chip">#{t}</span>{/each}</div>{/if}
      {#if c.status === 'deferred' && str(c.data, 'horizon')}<p class="muted">Отложена до {str(c.data, 'horizon')} (UTC).</p>{/if}
      {#if c.status === 'proposed' && curQuestion}
        <div class="proposal">
          <p><strong>Агент предлагает начать.</strong> <a href={questionHref(curQuestion.id)}>Вопрос {entryRef('question', curQuestion.id)}</a></p>
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
          Задача {rel}{#if c.task && !c.task.deleted}: <a href={taskHref(c.task.id)}>{entryRef('task', c.task.id)}</a>{/if}. Идею можно вернуть в
          новые или отказаться от неё.
        </p>
      {/if}

      <div class="row actions" role="group" aria-label="Решение по идее">
        {#if active && !agent}<button type="button" class="primary" onclick={() => startStart(c)} disabled={busy}>Начать</button>{/if}
        {#if agent && (c.status === 'new' || c.status === 'deferred')}
          <button type="button" class="primary" onclick={() => adopt(c)} disabled={busy}>Принять как свою</button>
        {/if}
        {#if active}<button type="button" onclick={() => startDecision('defer')} disabled={busy}>Отложить</button>{/if}
        {#if active || rel}<button type="button" class="danger" onclick={() => startDecision('decline')} disabled={busy}>Отказаться</button>{/if}
        {#if c.status === 'declined' || c.status === 'deferred' || rel}
          <button type="button" onclick={() => startDecision('reopen')} disabled={busy}>Вернуть</button>
        {/if}
        {#if c.status === 'new' || c.status === 'deferred'}
          <button type="button" onclick={() => startEdit(c)} disabled={busy}>Изменить</button>
        {/if}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}

      {#if mode === 'start'}
        <form class="form" onsubmit={submitStart} aria-label="Начать идею">
          <h3>Начать: где поставить задачу</h3>
          <label>
            Пространство задачи
            <select bind:value={dSpace}>
              <option value="knowledge">Knowledge</option>
              {#each targets as s (s.id)}<option value={spaceRef(s)}>{s.name}{s.shared ? ' (общее)' : ''}</option>{/each}
            </select>
          </label>
          {#if chosen?.shared}
            <p class="warn" data-testid="shared-warning">Задача появится в решении «{chosen.name}», её увидят участники.</p>
          {/if}
          {@render formEnd('Начать')}
        </form>
      {:else if mode === 'defer'}
        <form class="form" onsubmit={submitDefer} aria-label="Отложить идею">
          <h3>Отложить</h3>
          <label>
            Не раньше
            <span class="row">
              <input type="date" bind:value={dHorizon} min={minDate()} required aria-label="Отложить до" />
              <span class="muted small" title="Даты хранятся в UTC">UTC</span>
            </span>
          </label>
          {@render formEnd('Отложить')}
        </form>
      {:else if mode === 'decline'}
        <form class="form" onsubmit={submitDecline} aria-label="Отказаться от идеи">
          <h3>Отказаться</h3>
          <label>Причина (необязательно) <textarea bind:value={dReason} rows="2" maxlength="2000"></textarea></label>
          {@render formEnd('Отказаться')}
        </form>
      {:else if mode === 'reopen'}
        <form class="form" onsubmit={submitReopen} aria-label="Вернуть идею">
          <h3>Вернуть в новые</h3>
          <label>Причина <textarea bind:value={dReason} rows="2" maxlength="2000" required></textarea></label>
          {@render formEnd('Вернуть')}
        </form>
      {:else if mode === 'edit'}
        <form class="form" onsubmit={submitEdit} aria-label="Правка идеи">
          <h3>Изменить идею</h3>
          {@render fields()}
          {@render formEnd('Сохранить')}
        </form>
      {:else if actionError && mode !== 'create'}
        <p class="error" role="alert">{actionError}</p>
      {/if}
    </article>
  {/if}
</section>

{#snippet fields()}
  <label>Заголовок <input bind:value={fTitle} required maxlength="500" /></label>
  <label>Замысел <textarea bind:value={fBody} rows="4"></textarea></label>
  <div class="row">
    <label>
      Приоритет
      <select bind:value={fPriority}>
        {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
      </select>
    </label>
    <label>
      Не раньше
      <span class="row">
        <input type="date" bind:value={fHorizon} min={fHorizon && fHorizon === fHorizonWas ? undefined : minDate()} aria-label="Не раньше" />
        <span class="muted small" title="Даты хранятся в UTC">UTC</span>
      </span>
    </label>
  </div>
  <label>Теги (через запятую) <input bind:value={fTags} aria-label="Теги идеи" /></label>
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
  .feed {
    display: grid;
    gap: 6px;
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
    margin-top: 16px;
  }
  .card h2 {
    margin: 0 0 4px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .proposal {
    padding: 8px 10px;
    border-left: 3px solid var(--accent);
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
</style>
