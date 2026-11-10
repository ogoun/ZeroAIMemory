<script lang="ts">
  // «Идеи» (M5.7 §10): замыслы человека и агента. Вкладки по происхождению («Мои», «От агента»,
  // «Все» — шаблон ARIA tabs, как у «Кода»), фильтры статуса (по умолчанию активные), приоритета и
  // тега — на сервере, курсор по 50; поиск по заголовку и тексту — `POST /search kinds=[idea]`.
  // Карточка: «Начать» (decide accept — задача и ссылка на неё; у идеи Knowledge — выбор
  // решения с предупреждением об общем), «Отложить» (дата UTC), «Отказаться», «Вернуть»,
  // «Принять как свою» (у «от агента» «Начать» нет до неё), «Изменить» (PATCH с If-Match,
  // 409 — перечитать). Предлагает идею агент (propose): у WebUI этого действия нет; у идеи со
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
    IDEA_FORMS,
    decisionNotice,
    decisionOf,
    deferDefault,
    deferMin,
    ideaButtons,
    originOf,
    priorityOf,
    releasedOf,
    sharedWarning,
    tagsOf,
    type DecisionOpts,
    type IdeaAction,
    type IdeaForm,
  } from '../lib/ideaButtons';
  import {
    AUTHOR_TEXT,
    IDEA_STATUSES,
    IDEA_STATUS_SEL as STATUS_SEL,
    IDEA_TABS as TABS,
    PRIORITIES,
    PRIORITY_TEXT,
    dataStr as str,
    entryRef,
    fmtTime,
    ideaStatusText as statusText,
    type IdeaPriority,
    type IdeaStatusSel as StatusSel,
    type IdeaTab as Tab,
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

  type Mode = 'none' | IdeaForm;
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

  let chosen = $derived(targets.find((s) => spaceRef(s) === dSpace) ?? null);

  /** Кнопка карточки (`ideaButtons`): решение сразу или форма с полями. */
  function press(a: IdeaAction, c: Entry) {
    if (a === 'edit') return startEdit(c);
    if (a === 'adopt')
      return act(async () => {
        await adoptIdea(user, c.id);
      }, 'Идея принята как своя.');
    // «Начать» идеи решения — без формы; у идеи Knowledge — выбор пространства задачи
    if (a === 'start' && !knowledge) return decide(c, 'accept');
    dSpace = defaultTarget();
    dHorizon = deferDefault();
    dReason = '';
    actionError = '';
    mode = a;
  }

  /** Отправка формы карточки: создание, правка или решение (`decisionOf`). */
  function submit(e: SubmitEvent) {
    if (mode === 'create') return submitCreate(e);
    if (mode === 'edit') return submitEdit(e);
    e.preventDefault();
    if (!cur || mode === 'none') return;
    const d = decisionOf(mode, { horizon: dHorizon, reason: dReason, space: dSpace });
    if ('error' in d) actionError = d.error;
    else decide(cur, d.decision, d.opts);
  }

  const taskHref = (id: string, sp = taskSpace ?? space) => href({ name: 'journal', user, space: sp, journal: 'tasks', entry: id });
  const questionHref = (id: string) => href({ name: 'journal', user, space, journal: 'questions', entry: id });
  let filtered = $derived(!!(applied.tag || applied.query));
</script>

<section class="journal ideas">
  <div class="row bar">
    <button type="button" class="primary" onclick={startCreate}>Новая идея</button>
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
    {@const rel = releasedOf(c)}
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
  {@const f = IDEA_FORMS[m]}
  <form class="form" class:panel={m === 'create'} onsubmit={submit} aria-label={f.aria}>
    <h3>{f.title}</h3>
    {#if m === 'create' || m === 'edit'}
      {@render fields()}
    {:else if m === 'start'}
      <label>
        Пространство задачи
        <select bind:value={dSpace}>
          <option value="knowledge">Knowledge</option>
          {#each targets as s (s.id)}<option value={spaceRef(s)}>{s.name}{s.shared ? ' (общее)' : ''}</option>{/each}
        </select>
      </label>
      {#if chosen?.shared}<p class="warn" data-testid="shared-warning">{sharedWarning(chosen.name)}</p>{/if}
    {:else if m === 'defer'}
      <label>
        Не раньше
        {@render date('Отложить до', true)}
      </label>
    {:else}
      <label
        >Причина{m === 'decline' ? ' (необязательно)' : ''}
        <textarea bind:value={dReason} rows="2" maxlength="2000" required={m === 'reopen'}></textarea></label
      >
    {/if}
    {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
    <div class="row end">
      <button type="submit" class="primary" disabled={busy}>{f.submit}</button>
      <button type="button" onclick={() => ((mode = 'none'), (actionError = ''))}>Отмена</button>
    </div>
  </form>
{/snippet}

{#snippet date(label: string, decision: boolean)}
  <span class="row">
    {#if decision}
      <input type="date" bind:value={dHorizon} min={deferMin()} required aria-label={label} />
    {:else}
      <input type="date" bind:value={fHorizon} min={fHorizon && fHorizon === fHorizonWas ? undefined : deferMin()} aria-label={label} />
    {/if}
    <span class="muted small" title="Даты хранятся в UTC">UTC</span>
  </span>
{/snippet}

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
      {@render date('Не раньше', false)}
    </label>
  </div>
  <label>Теги (через запятую) <input bind:value={fTags} aria-label="Теги идеи" /></label>
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
