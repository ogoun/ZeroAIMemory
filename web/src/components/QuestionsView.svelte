<script lang="ts">
  // «Вопросы» (M4.5 §4): вопросы агентов человеку. Открытые — первыми; варианты — кнопками
  // (рекомендация агента отмечена), ответ в один клик или текстом. Ответ идёт с
  // `channel: webui` и создаёт решение (insight `decision`, `decided_by: human`) — после
  // ответа карточка ссылается на него. Отвеченные и неактуальные — ниже, с ответом.
  // Вопрос-предложение идеи (`data.idea`, M5.7 §5.3) — со ссылкой «Идея w:<id>»; ответ — только
  // вариантом (у вопроса «вернуть» идею со снятой задачей, §16, — «вернуть»/«отказаться», без
  // даты), у «отложить» — дата «не раньше» (UTC, не раньше завтрашней); «начать» идеи
  // Knowledge с задачей в общем решении (`data.task_space`) — после предупреждения (§5.2, п. 1).
  // Правка и корзина (M7.2, спецификация §5.9): у открытого вопроса — «Изменить» (`EntryEdit`:
  // текст вопроса, теги, область) и «Неактуален» (`status: obsolete` с причиной); у любого —
  // «Удалить» (в корзину, с подтверждением); «Корзина» — удалённые вопросы с «Восстановить»
  // (`JournalTrash`). Форма и корзина — ленивые чанки. Вопрос, отвеченный в другом месте за время
  // правки, закрывает форму с объяснением. Переход к записи выключает «Корзину».
  //
  // Формы (M7.3, спецификация §6.14, решения 80–85): лента без карточки — «Изменить» открывает
  // правку в широком окне формы (`EditPanel` с `dialog`, правило В); «Отложить до» вопроса-
  // предложения — окно с датой, «начать» в общем решении — окно-подтверждение с предупреждением
  // (правило Б); вариант без полей — сразу (Д); ответ текстом — поле на виду (А), набранное — в
  // стеке проверок ухода (п. 4). Вопрос из адреса — на виду и в фокусе его заголовка (п. 2).
  import { onMount, tick, untrack } from 'svelte';
  import EditPanel from './EditPanel.svelte';
  import FormDialog from './FormDialog.svelte';
  import Notice from './Notice.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import TrashPanel from './TrashPanel.svelte';
  import {
    ApiError,
    answerQuestion,
    answeredDecision,
    errorText,
    isStaleEntry,
    listEntries,
    patchEntry,
    type Entry,
    type Space,
  } from '../lib/api';
  import { trashAsk } from '../lib/entryTrash';
  import { focusNow, focusSoon } from '../lib/focus';
  import { DEFER_FORM, START_SUBMIT, deferDefault, deferError, deferMin, sharedWarning } from '../lib/ideaForms';
  import { AUTHOR_TEXT, STATUS_TEXT, entryRef, fmtTime } from '../lib/journals';
  import { guardLeave } from '../lib/leaveEdit';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    /** Пространства пользователя: общее ли решение задачи вопроса-предложения. */
    spaces?: Space[];
    onexpired: () => void;
  }

  let { user, space, entry, spaces = [], onexpired }: Props = $props();

  const PAGE = 200;
  const CHANNEL_TEXT: Record<string, string> = { webui: 'WebUI', chat: 'чат', cli: 'CLI' };

  interface Opt {
    key: string;
    text: string;
  }

  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let seq = 0;
  let busy = $state('');
  /** Ошибка и текст ответа по вопросу. */
  let errors = $state<Record<string, string>>({});
  let texts = $state<Record<string, string>>({});
  /** Решение, созданное ответом только что (до перечитывания списка). */
  let created = $state<Record<string, string>>({});
  /** Окно «Отложить до» вопроса-предложения: вопрос, дата и дата при открытии. */
  let deferQ = $state<Entry | null>(null);
  let deferDate = $state('');
  let deferAt = '';
  /** Окно-подтверждение «начать» в общем решении. */
  let startQ = $state<Entry | null>(null);
  /** Ошибка окна (отложить, начать). */
  let dlgError = $state('');
  /** «Корзина»: вместо списка — удалённые вопросы. */
  let trash = $state(false);
  /** Вопрос с открытым окном правки. */
  let editing = $state('');
  let notice = $state('');
  let ask = $state<Ask | null>(null);
  /** Вопрос из адреса, заголовок которого получит фокус после загрузки ленты (п. 2). */
  let revealFor = '';

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  /** Набранный ответ текстом у открытого вопроса (поле на виду, правило А). */
  const typed = (): boolean => items.some((q) => q.status === 'open' && (texts[q.id] ?? '').trim() !== '');

  // ответ текстом — в стеке проверок ухода (§6.14 п. 4): уход по адресу спрашивает; согласие —
  // набранное отпущено
  onMount(() => guardLeave(typed, () => (texts = {})));

  /** Пространство, по которому загружена лента: эффект ниже перезапускается на каждый переход
   * (свойства — из объекта маршрута), а сброс корзины и итога и перечитывание ленты нужны только
   * при смене пользователя или пространства (M7.4 §7.7 п. 4, образец — «Словарь»): переход к
   * вопросу (поиск, связи, адрес) ленту не перечитывает — загруженные страницы и набранные ответы
   * на месте, вопрос показывается из загруженного. Его нет в ленте (задан после загрузки) —
   * лента перечитывается; неудавшаяся загрузка повторяется при следующем переходе
   * (`scopeOf = ''`). */
  let scopeOf = '';
  $effect(() => {
    const u = user;
    const s = space;
    const id = entry;
    untrack(() => {
      // корзина, выключенная переходом, могла вернуть вопросы в ленту («Восстановить»)
      let reload = !!id && trash;
      if (scopeOf !== `${u}|${s}`) {
        scopeOf = `${u}|${s}`;
        trash = false;
        notice = '';
        reload = true;
      }
      // переход: окна закрываются (уход с несохранённым спросил окно и стек проверок ухода), к
      // записи — корзина выключается, прежний итог убирается
      editing = '';
      deferQ = null;
      startQ = null;
      if (id) {
        trash = false;
        notice = '';
      }
      revealFor = id ?? '';
      if (!reload && id && !loading && !items.some((q) => q.id === id)) reload = true;
      if (reload) load(u, s, true);
      // вопрос из загруженной ленты — на виду и в фокусе после перерисовки (корзина могла быть
      // открыта); идущая загрузка покажет его сама
      else if (id && !loading) void tick().then(() => reveal(id));
    });
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, { kind: 'question', cursor: reset ? null : next, limit: PAGE });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
      if (reset && entry) {
        await tick();
        reveal(entry);
      }
    } catch (e) {
      if (my !== seq) return;
      if (reset) {
        items = [];
        scopeOf = '';
      }
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** Вопрос из адреса — на виду; после перехода к нему (не после перечитывания ленты ответом) —
   * ещё и фокус на заголовке (§6.14 п. 2, как карточка журнала). */
  function reveal(id: string) {
    const el = root?.querySelector<HTMLElement>(`[data-question="${CSS.escape(id)}"]`);
    if (!el) return;
    el.scrollIntoView({ block: revealFor === id ? 'start' : 'nearest' });
    if (revealFor !== id) return;
    revealFor = '';
    focusNow(el.querySelector('h2'));
  }

  const optionsOf = (e: Entry): Opt[] =>
    Array.isArray(e.data.options)
      ? (e.data.options as unknown[]).filter(
          (o): o is Opt => !!o && typeof o === 'object' && typeof (o as Opt).key === 'string' && typeof (o as Opt).text === 'string',
        )
      : [];
  /** Идея вопроса-предложения (`data.idea`) или пустая строка. */
  const ideaOf = (e: Entry): string => (typeof e.data.idea === 'string' ? e.data.idea : '');
  const ideaHref = (id: string) => href({ name: 'journal', user, space, journal: 'ideas', entry: id });

  /** Общее решение, где «начать» поставит задачу идеи Knowledge (иначе `null`). */
  function sharedTarget(q: Entry): Space | null {
    const here = spaces.find((s) => s.kind === 'knowledge');
    if (space !== 'knowledge' && q.space_id !== here?.id) return null;
    const ts = typeof q.data.task_space === 'string' ? q.data.task_space : '';
    const sp = spaces.find((s) => s.id === ts);
    return sp && sp.kind === 'solution' && sp.shared ? sp : null;
  }

  /** Вариант ответа: без полей — сразу (правило Д); «начать» в общем решении — подтверждение,
   * «отложить» — окно с датой (правило Б). */
  function pick(q: Entry, key: string) {
    errors[q.id] = '';
    dlgError = '';
    if (ideaOf(q) && key === 'accept' && sharedTarget(q)) {
      startQ = q;
      return;
    }
    if (ideaOf(q) && key === 'defer') {
      deferDate = deferDefault();
      deferAt = deferDate;
      deferQ = q;
      return;
    }
    answer(q, { option: key });
  }

  /** Ответ из окна: ошибка — в окне (исключение), окно открыто; вопрос уже отвечен в другом месте —
   * окно закрыто, лента перечитана, причина — у вопроса. */
  async function answerIn(q: Entry, a: { option: string; horizon?: string }, close: () => void) {
    answeredIn = q.id;
    try {
      const r = await answerQuestion(user, q.id, a);
      created[q.id] = r.decision_id;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      const d = answeredDecision(e);
      if (d === null) throw new Error(errorText(e));
      if (d) created[q.id] = d;
      close();
      await load(user, space, true);
      errors[q.id] = errorText(e);
      return;
    }
    close();
    await load(user, space, true);
  }

  function submitDefer() {
    const q = deferQ;
    if (!q) return;
    const err = deferError(deferDate);
    if (err) {
      dlgError = err;
      return;
    }
    dlgError = '';
    return answerIn(q, { option: 'defer', horizon: deferDate }, () => (deferQ = null));
  }

  function submitStart() {
    const q = startQ;
    if (!q) return;
    dlgError = '';
    return answerIn(q, { option: 'accept' }, () => (startQ = null));
  }

  /** Вопрос, на который ответили из окна: окно закрыто владельцем до своего закрытия. */
  let answeredIn = '';
  /** Заголовок вопроса, на который ответили из окна, — фокус после закрытия окна (кнопки вариантов
   * ушли вместе с ответом). */
  const answeredHeading = () => (answeredIn ? root?.querySelector(`[data-question="${CSS.escape(answeredIn)}"] h2`) : null);

  const recommended = (e: Entry): string =>
    typeof e.data.recommendation === 'string' ? e.data.recommendation : '';

  interface Ans {
    text?: string;
    option?: string;
    by: string;
    channel: string;
    answered_at: string;
    decision_id: string;
  }
  function answerOf(e: Entry): Ans | null {
    const a = e.data.answer;
    return a && typeof a === 'object' ? (a as Ans) : null;
  }
  const answerText = (e: Entry, a: Ans) => {
    const opt = optionsOf(e).find((o) => o.key === a.option);
    return [opt ? opt.text : a.option, a.text].filter(Boolean).join(' — ');
  };

  /** Открытые первыми (новые вопросы сверху внутри группы — как отдаёт сервер). */
  let ordered = $derived([...items.filter((q) => q.status === 'open'), ...items.filter((q) => q.status !== 'open')]);

  async function answer(q: Entry, a: { text?: string; option?: string; horizon?: string }) {
    if (busy) return;
    busy = q.id;
    errors[q.id] = '';
    try {
      const r = await answerQuestion(user, q.id, a);
      created[q.id] = r.decision_id;
      texts[q.id] = '';
      await load(user, space, true);
    } catch (e) {
      const d = answeredDecision(e);
      if (d !== null) {
        // уже отвечен в другом месте: показать итог
        if (d) created[q.id] = d;
        await load(user, space, true);
      }
      fail(e, (m) => (errors[q.id] = m));
    } finally {
      busy = '';
    }
  }

  function submitText(e: SubmitEvent, q: Entry) {
    e.preventDefault();
    const text = (texts[q.id] ?? '').trim();
    if (!text) {
      errors[q.id] = 'Введите ответ или выберите вариант.';
      return;
    }
    answer(q, { text });
  }

  const decisionHref = (id: string) =>
    href({ name: 'journal', user, space, journal: 'insights', entry: id });

  /** Вопрос `e` в списке — новой ревизией (после правки или перечитывания). */
  const replace = (e: Entry) => (items = items.map((x) => (x.id === e.id ? e : x)));

  /** Вопрос с открытым окном правки (из ленты: новая ревизия после перечитывания — в окне). */
  let editingEntry = $derived(editing ? (items.find((x) => x.id === editing) ?? null) : null);

  function startEdit(q: Entry) {
    editing = q.id;
    errors[q.id] = '';
    notice = '';
  }

  let root = $state<HTMLElement | null>(null);
  /** Кнопка действия вопроса `id` с именем, начинающимся на `name` («Изменить», «Удалить»). */
  const action = (id: string, name: string) =>
    root?.querySelector(`[data-question="${CSS.escape(id)}"] .acts button[aria-label^="${name}"]`);

  /** Окно правки закрыто (сохранено или «Отмена»): фокус — на «Изменить» вопроса. */
  function closeEdit(id: string) {
    editing = '';
    focusSoon(() => action(id, 'Изменить') ?? action(id, 'Удалить'));
  }

  function saved(e: Entry) {
    replace(e);
    closeEdit(e.id);
    notice = `Вопрос «${e.title}» сохранён.`;
  }

  /** 409 окна правки: вопрос перечитан. Ответили или сняли его в другом месте — правки больше нет
   * (правится только открытый): окно закрывается, причина — у вопроса, «Удалить» снова видна. */
  function reread(e: Entry) {
    replace(e);
    if (editing === e.id && e.status !== 'open') {
      editing = '';
      errors[e.id] = `Пока шла правка, вопрос стал «${e.status ? STATUS_TEXT[e.status] : ''}» в другом месте — форма закрыта, текст не сохранён.`;
      focusSoon(() => action(e.id, 'Удалить'));
    }
  }

  /** 409: список перечитан, ошибка — у карточки вопроса. */
  async function stale(id: string, message: string) {
    await load(user, space, true);
    errors[id] = message;
  }

  /** «Неактуален»: `status: obsolete` с причиной (сервер требует её, M4.5 §4). */
  function askObsolete(q: Entry) {
    errors[q.id] = '';
    notice = '';
    ask = {
      title: 'Вопрос неактуален?',
      message: `«${q.title}» уйдёт из открытых: ответить на него будет нельзя.`,
      label: 'Причина',
      placeholder: 'например, решили без вопроса',
      confirm: 'Неактуален',
      run: async (reason) => {
        try {
          replace(await patchEntry(user, q.id, q.rev, { status: 'obsolete', reason }));
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) return onexpired();
          if (isStaleEntry(e)) return stale(q.id, errorText(e));
          throw new Error(errorText(e));
        }
        if (editing === q.id) editing = '';
        notice = `Вопрос «${q.title}» отмечен неактуальным.`;
      },
      // «Неактуален» ушла вместе с вариантами ответа — фокус на «Удалить» вопроса
      refocus: () => action(q.id, 'Удалить'),
    };
  }

  function askDelete(q: Entry) {
    errors[q.id] = '';
    notice = '';
    ask = trashAsk(user, q, {
      deleted: (msg) => {
        if (editing === q.id) editing = '';
        items = items.filter((x) => x.id !== q.id);
        notice = msg;
      },
      stale: (msg) => stale(q.id, msg),
      expired: onexpired,
      focus: () => root?.querySelector(':scope > [role="status"]'),
    });
  }

  function toggleTrash() {
    trash = !trash;
    notice = '';
    // назад к списку: восстановленные из корзины — в нём
    if (!trash) load(user, space, true);
  }
</script>

<Prompt bind:ask />

<section class="journal questions" bind:this={root}>
  <div class="row bar">
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  <Notice text={notice} />
  {#if trash}
    <TrashPanel {user} {space} kind="question" {onexpired} />
  {:else}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if loading && items.length === 0}<p class="muted">Загрузка…</p>{/if}
    {#if !loading && items.length === 0 && !error}<p class="muted">Вопросов нет.</p>{/if}

    <div class="list" role="feed" aria-label="Вопросы">
      {#each ordered as q (q.id)}
        {@const a = answerOf(q)}
        {@const opts = optionsOf(q)}
        <article class="q panel" class:selected={q.id === entry} class:done={q.status !== 'open'} data-question={q.id}>
          <header class="row">
            <h2>{q.title}</h2>
            <span class="chip status">{q.status ? STATUS_TEXT[q.status] : ''}</span>
            <span class="muted small">{AUTHOR_TEXT[q.author.kind] ?? q.author.kind} · {fmtTime(q.created_at)}</span>
          </header>
          {#if q.body}<p class="body">{q.body}</p>{/if}
          {#if ideaOf(q)}
            <p><a href={ideaHref(ideaOf(q))} data-testid="idea-link">Идея {entryRef('idea', ideaOf(q))}</a></p>
          {/if}

          {#if q.status === 'open'}
            {#if opts.length}
              <div class="row opts" role="group" aria-label="Варианты ответа">
                {#each opts as o (o.key)}
                  <button
                    type="button"
                    class:primary={o.key === recommended(q)}
                    disabled={busy !== ''}
                    title={o.key === recommended(q) ? 'Рекомендация агента' : ''}
                    onclick={() => pick(q, o.key)}
                  >
                    {o.text}{#if o.key === recommended(q)}<span class="rec"> · рекомендуется</span>{/if}
                  </button>
                {/each}
              </div>
            {/if}
            {#if !ideaOf(q)}
              <!-- ответ текстом — поле на виду (правило А): ошибка — под ним -->
              <form class="row" onsubmit={(e) => submitText(e, q)} aria-label="Ответ текстом">
                <input
                  bind:value={texts[q.id]}
                  placeholder={opts.length ? 'или свой ответ' : 'ответ'}
                  aria-label="Ответ на вопрос «{q.title}»"
                  maxlength="4000"
                />
                <button type="submit" disabled={busy !== ''}>Ответить</button>
              </form>
            {/if}
          {:else if a}
            <p class="answer">
              Ответ: <strong>{answerText(q, a)}</strong>
              <span class="muted small">
                · {a.by === 'human' ? 'человек' : 'агент'} · {CHANNEL_TEXT[a.channel] ?? a.channel} · {fmtTime(a.answered_at)}
              </span>
            </p>
            {#if a.decision_id}
              <p><a href={decisionHref(a.decision_id)} data-testid="decision-link">Созданное решение</a></p>
            {/if}
          {:else if q.status === 'obsolete'}
            <p class="muted">Вопрос неактуален.</p>
          {/if}
          {#if created[q.id] && !a}
            <p><a href={decisionHref(created[q.id])} data-testid="decision-link">Созданное решение</a></p>
          {/if}
          {#if errors[q.id]}<p class="error" role="alert">{errors[q.id]}</p>{/if}
          <!-- правка, «Неактуален» и корзина (§5.9 пп. 1–2); имена — с вопросом: в списке кнопок диктора
               вопросы различимы -->
          <div class="row acts" role="group" aria-label="Действия с вопросом">
            {#if q.status === 'open'}
              <button type="button" class="small-btn" aria-label="Изменить «{q.title}»" onclick={() => startEdit(q)}>Изменить</button>
              <button type="button" class="small-btn" aria-label="Неактуален «{q.title}»" onclick={() => askObsolete(q)}>Неактуален</button>
            {/if}
            <button type="button" class="small-btn danger" aria-label="Удалить «{q.title}»" onclick={() => askDelete(q)}>Удалить</button>
          </div>
        </article>
      {/each}
    </div>
    {#if next && !loading}
      <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
    {/if}
  {/if}
</section>

<!-- «Изменить» — правка в широком окне формы (лента без карточки, правило В) -->
{#if editingEntry && editingEntry.status === 'open'}
  {@const q = editingEntry}
  {#key q.id}
    <EditPanel
      dialog
      {user}
      entry={q}
      onsaved={saved}
      oncancel={() => closeEdit(q.id)}
      onreread={reread}
      {onexpired}
      refocus={() => action(q.id, 'Изменить')}
    />
  {/key}
{/if}

<!-- «отложить» вопроса-предложения — окно с датой (правило Б) -->
<FormDialog
  open={deferQ !== null}
  title={DEFER_FORM.title}
  subject={deferQ ? `Вопрос: ${deferQ.title}` : undefined}
  description="Идея вернётся к разбору не раньше этой даты."
  submit={DEFER_FORM.submit}
  error={dlgError}
  dirty={() => deferDate !== deferAt}
  refocus={answeredHeading}
  onsubmit={submitDefer}
  onclose={() => ((deferQ = null), (dlgError = ''))}
>
  <!-- видимая подпись — та же, что имя поля (WCAG 2.5.3) -->
  <label>
    Отложить до
    <span class="row">
      <input type="date" bind:value={deferDate} min={deferMin()} required aria-label="Отложить до" data-autofocus />
      <span class="muted small" title="Даты хранятся в UTC">UTC</span>
    </span>
  </label>
</FormDialog>

<!-- «начать» идеи Knowledge с задачей в общем решении — окно-подтверждение (правило Б) -->
<FormDialog
  open={startQ !== null}
  confirm
  title="Начать идею в общем решении?"
  subject={startQ ? `Вопрос: ${startQ.title}` : undefined}
  submit={START_SUBMIT}
  error={dlgError}
  refocus={answeredHeading}
  onsubmit={submitStart}
  onclose={() => ((startQ = null), (dlgError = ''))}
>
  <p class="warn" data-testid="shared-warning">{sharedWarning((startQ && sharedTarget(startQ)?.name) ?? '')}</p>
</FormDialog>

<style>
  .list {
    display: grid;
    gap: 10px;
  }
  /* вопрос из адреса прокручивается к началу — под шапку приложения */
  .q {
    scroll-margin-top: calc(var(--header-h) + 8px);
  }
  .q.selected {
    border-color: var(--accent);
  }
  .q.done {
    opacity: 0.85;
  }
  .q h2 {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .opts {
    margin: 8px 0;
  }
  .rec {
    font-size: 12px;
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .small {
    font-size: 12px;
  }
  form input {
    flex: 1;
    min-width: min(220px, 100%);
  }
  .bar {
    margin-bottom: 8px;
  }
  .acts {
    margin-top: 8px;
  }
</style>
