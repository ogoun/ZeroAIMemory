<script lang="ts">
  // «Идеи» (M5.7 §10): замыслы человека и агента. Вкладки по происхождению («Мои», «От агента»,
  // «Все» — шаблон ARIA tabs, как у «Кода»), фильтры статуса (по умолчанию активные), приоритета и
  // тега — на сервере, курсор по 50; поиск по заголовку и тексту — `POST /search kinds=[idea]`.
  // Карточка: «Начать» (decide accept — задача и ссылка на неё; у идеи Knowledge — выбор
  // решения с предупреждением об общем), «Отложить» (дата UTC), «Отказаться», «Вернуть»,
  // «Принять как свою» (у «от агента» «Начать» нет до неё), «Изменить» (PATCH с If-Match,
  // 409 — перечитать); формы — отдельный ленивый чанк `IdeaForm.svelte`, грузится при первом
  // открытии. Предлагает идею агент (propose): у WebUI этого действия нет; у идеи со
  // снятой задачей открытый вопрос агента «вернуть» — пометка «Предложено вернуть» (§16, п. 6),
  // кнопки «Вернуть»/«Отказаться» остаются. Ленивый чанк.
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
    type IdeaDecision,
    type SearchItem,
    type Space,
  } from '../lib/api';
  import {
    IDEA_ACTIVE,
    IDEA_TABS as TABS,
    decisionNotice,
    ideaButtons,
    originOf,
    priorityOf,
    releasedOf,
    type DecisionOpts,
    type IdeaAction,
    type IdeaSubmit,
    type IdeaTab as Tab,
  } from '../lib/ideaButtons';
  import type { IdeaForm } from '../lib/ideaForms';
  import {
    AUTHOR_TEXT,
    IDEA_STATUSES,
    IDEA_STATUS_TEXT,
    PRIORITIES,
    PRIORITY_TEXT,
    dataStr as str,
    entryRef,
    fmtTime,
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

  const uid = $props.id();

  /** Фильтр статуса: подпись и статусы запроса (пусто — все). */
  type StatusSel = 'active' | 'accepted' | 'done' | 'declined' | 'all';
  const STATUS_SEL: { id: StatusSel; title: string; statuses: EntryStatus[] }[] = [
    { id: 'active', title: 'Активные', statuses: IDEA_ACTIVE },
    { id: 'accepted', title: IDEA_STATUS_TEXT.accepted, statuses: ['accepted'] },
    { id: 'done', title: IDEA_STATUS_TEXT.done, statuses: ['done'] },
    { id: 'declined', title: IDEA_STATUS_TEXT.declined, statuses: ['declined'] },
    { id: 'all', title: 'все', statuses: [] },
  ];
  const statusesOf = (s: StatusSel) => STATUS_SEL.find((x) => x.id === s)?.statuses ?? [];
  const statusText = (s: EntryStatus | null) => (IDEA_STATUS_TEXT as Record<string, string>)[s ?? ''] ?? s ?? '';

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

  type Mode = 'none' | IdeaForm;
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');

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

  /** Решение по идее; `accept` — ещё и ссылка на созданную задачу. */
  function decide(c: Entry, decision: IdeaDecision, opts: DecisionOpts = {}) {
    let task = '';
    act(
      async () => {
        const r = await decideIdea(user, c.id, decision, opts);
        if (r.task) task = r.task.id;
      },
      () => decisionNotice(decision, entryRef('task', task), opts.horizon),
    );
  }

  /** Решение для задачи по умолчанию: `data.task_space` вопроса-предложения или Knowledge. */
  function defaultTarget(): string {
    const ts = curQuestion ? str(curQuestion.data, 'task_space') : '';
    const sp = ts ? spaces.find((s) => s.id === ts) : null;
    return sp && sp.kind === 'solution' ? spaceRef(sp) : 'knowledge';
  }

  /** Номер открытия формы: повторное нажатие той же кнопки создаёт форму заново (поля сброшены). */
  let opened = $state(0);

  /** Открыть форму `m` (создание, правка, решение с полями). */
  function show(m: IdeaForm) {
    actionError = '';
    opened++;
    mode = m;
  }

  function closeForm() {
    mode = 'none';
    actionError = '';
  }

  /** Кнопка карточки (`ideaButtons`): решение сразу или форма с полями. */
  function press(a: IdeaAction, c: Entry) {
    if (a === 'adopt')
      act(async () => {
        await adoptIdea(user, c.id);
      }, 'Идея принята как своя.');
    // «Начать» идеи решения — без формы; у идеи Knowledge — выбор пространства задачи
    else if (a === 'start' && !knowledge) decide(c, 'accept');
    else show(a);
  }

  /** Заполненная форма (`IdeaForm.svelte`): создание, правка или решение. */
  function submitted(f: IdeaSubmit) {
    const c = cur;
    if ('create' in f) act(async () => (await createEntry(user, space, { kind: 'idea', ...f.create })).id, 'Идея записана.');
    else if (!c) return;
    else if ('patch' in f)
      act(async () => {
        await patchEntry(user, c.id, c.rev, f.patch);
      }, 'Идея сохранена.');
    else decide(c, f.decision, f.opts);
  }

  const taskHref = (id: string, sp = taskSpace ?? space) => href({ name: 'journal', user, space: sp, journal: 'tasks', entry: id });
  const questionHref = (id: string) => href({ name: 'journal', user, space, journal: 'questions', entry: id });
  let filtered = $derived(!!(applied.tag || applied.query));
</script>

<section class="journal ideas">
  <div class="row bar">
    <button type="button" class="primary" onclick={() => show('create')}>Новая идея</button>
  </div>

  {#if mode === 'create'}{@render form('create')}{/if}

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

    <div class="feed" role="feed" aria-label={hits ? 'Найденные идеи' : 'Лента идей'} aria-busy={loading}>
      {#each hits ?? [] as h (h.entry_id)}
        <article class="idea" class:selected={h.entry_id === entry} data-idea={h.entry_id}>
          <button type="button" class="link title" onclick={() => select(h.entry_id ?? '')}>{h.title ?? ''}</button>
          {#if h.status}<span class="chip">{statusText(h.status)}</span>{/if}
          <p class="snippet small">
            {#each snippetParts(h.snippet ?? '') as p, j (j)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
          </p>
        </article>
      {/each}
      {#each items as it (it.id)}
        {@const task = str(it.data, 'task_id')}
        {@const q = str(it.data, 'question_id')}
        {@const hz = it.status === 'deferred' && str(it.data, 'horizon')}
        <article class="idea" class:selected={it.id === entry} data-idea={it.id}>
          <button type="button" class="link title" onclick={() => select(it.id)}>{it.title}</button>
          <span class="chip prio" data-priority={priorityOf(it)}>{PRIORITY_TEXT[priorityOf(it)]}</span>
          <span class="chip">{statusText(it.status)}</span>
          {#if hz}<span class="muted small">не раньше {hz} UTC</span>{/if}
          {@render marks(originOf(it) === 'agent', [])}
          {#if task && !knowledge}<a class="small" href={taskHref(task, space)}>задача {entryRef('task', task)}</a>
          {:else if task}<button type="button" class="link small" onclick={() => select(it.id)} title="Задача может быть в решении: ссылка — в карточке">задача {entryRef('task', task)}</button>
          {:else if q && it.status === 'proposed'}<a class="small" href={questionHref(q)}>вопрос {entryRef('question', q)}</a>{/if}
          {@render marks(false, it.tags)}
        </article>
      {/each}
    </div>
    {#if !loading && !error && !(hits ?? items).length}
      <p class="muted">{hits ? 'Ничего не найдено.' : filtered ? 'Идей по выбранным фильтрам нет.' : 'Идей нет.'}</p>
    {/if}
    {#if next && !loading}
      <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
    {/if}
    {#if loading}<p class="muted">Загрузка…</p>{/if}
  </div>

  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    {@const agent = originOf(c) === 'agent'}
    {@const rel = releasedOf(c)}
    <article class="card panel" aria-label="Карточка идеи" data-idea-card={c.id}>
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status" data-status={c.status}>{statusText(c.status)}</span>
          <span class="chip prio">приоритет: {PRIORITY_TEXT[priorityOf(c)]}</span>
          {@render marks(agent, [])}
          <span class="muted small">{entryRef('idea', c.id)} · {AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>
      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if c.tags.length}<div class="row">{@render marks(false, c.tags)}</div>{/if}
      {#if c.status === 'deferred' && str(c.data, 'horizon')}<p class="muted">Отложена до {str(c.data, 'horizon')} (UTC).</p>{/if}
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
          Задача {rel}{#if c.task && !c.task.deleted}: <a href={taskHref(c.task.id)}>{entryRef('task', c.task.id)}</a>{/if}. Идею можно вернуть в
          новые или отказаться от неё.
        </p>
      {/if}

      <div class="row actions" role="group" aria-label="Решение по идее">
        {#each ideaButtons(c.status, agent, !!rel) as b (b.act)}
          <button type="button" class={b.cls} onclick={() => press(b.act, c)} disabled={busy}>{b.title}</button>
        {/each}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}

      {#if mode !== 'none' && mode !== 'create'}
        {@render form(mode)}
      {:else if actionError && mode !== 'create'}
        <p class="error" role="alert">{actionError}</p>
      {/if}
    </article>
  {/if}
</section>

{#snippet form(m: IdeaForm)}
  {#await import('./IdeaForm.svelte')}
    <p class="muted">Загрузка…</p>
  {:then { default: Form }}
    {#key opened}
      <Form
        {m}
        idea={cur}
        {targets}
        space0={defaultTarget()}
        {busy}
        error={actionError}
        onsubmit={submitted}
        onerror={(e) => (actionError = e)}
        oncancel={closeForm}
      />
    {/key}
  {:catch e}
    <!-- чанк формы не пришёл: сервер обновился (у чанков новые имена) или нет сети -->
    <p class="error" role="alert">Форма не загрузилась: {errorText(e)} — обновите страницу</p>
    <button type="button" onclick={closeForm}>Отмена</button>
  {/await}
{/snippet}

{#snippet marks(agent: boolean, tags: string[])}
  {#if agent}<span class="badge small">от агента</span>{/if}
  {#each tags as t (t)}<span class="chip">#{t}</span>{/each}
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
  .small {
    font-size: 12px;
  }
</style>
