<script lang="ts">
  // «Словарь» (M4.5 §2): действующие термины пространства по алфавиту, поиск по имени,
  // псевдониму и определению, карточка (псевдонимы, «не путать с», код) и правка. Сессия WebUI —
  // человек. Правка идёт с ревизией карточки (If-Match): запись изменилась — 409, карточка
  // перечитывается. Имя или псевдоним занят другим термином — 409 term-conflict со ссылками
  // на занявшие термины. Корзина (M7.2, спецификация §5.9 пп. 2–3): «Удалить» карточки — в
  // корзину, с подтверждением; «Корзина» — вместо списка удалённые термины с «Восстановить»
  // (`JournalTrash`, ленивый чанк; имя, занятое за это время, — 409 со ссылкой в тексте ошибки).
  // Несохранённое (финальное ревью M7.2, I1): поля открытой формы (создание, правка, отзыв) против
  // их значений при открытии — `unsaved()`; «← Словарь» сначала спрашивает «Закрыть без
  // сохранения?» (`JournalSplit`), уход по адресу и другая форма — `LEAVE_EDIT` (`lib/leaveEdit.ts`).
  import { onMount, untrack } from 'svelte';
  import Notice from './Notice.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import TrashPanel from './TrashPanel.svelte';
  import { trashAsk } from '../lib/entryTrash';
  import {
    ApiError,
    createEntry,
    errorText,
    getEntry,
    getGlossary,
    isStaleEntry,
    patchEntry,
    retractEntry,
    termConflicts,
    type Entry,
    type GlossaryItem,
  } from '../lib/api';
  import { AUTHOR_TEXT, JOURNAL_TITLE, fmtTime } from '../lib/journals';
  import { LEAVE_EDIT, guardLeave } from '../lib/leaveEdit';
  import { go, href } from '../lib/router';
  import JournalSplit from './JournalSplit.svelte';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const collator = new Intl.Collator('ru', { sensitivity: 'base' });

  let items = $state<GlossaryItem[]>([]);
  let loading = $state(false);
  let error = $state('');
  let search = $state('');
  let seq = 0;

  let cur = $state<Entry | null>(null);
  let curError = $state('');
  let eseq = 0;
  type Mode = 'none' | 'create' | 'edit' | 'retract';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let conflicts = $state<{ id: string; title: string; matched: string }[]>([]);
  let notice = $state('');
  /** «Корзина»: вместо списка — удалённые термины. */
  let trash = $state(false);
  /** Итог действия, закрывшего карточку (удаление), — над списком. */
  let listNotice = $state('');
  let ask = $state<Ask | null>(null);

  // форма
  let fName = $state('');
  let fDefinition = $state('');
  let fAliases = $state('');
  let fNot = $state('');
  let fCode = $state('');
  let fBody = $state('');
  let rReason = $state('');

  /** Поля открытой формы. */
  const formState = (): string =>
    mode === 'create' || mode === 'edit'
      ? JSON.stringify([fName, fDefinition, fAliases, fNot, fCode, fBody])
      : mode === 'retract'
        ? rReason
        : '';
  /** Поля формы при её открытии (пустые или из термина карточки). */
  let pristine = '';

  /** Открыть форму `m`: поля уже заданы — их снимок для `unsaved()`. */
  function openForm(m: Mode) {
    mode = m;
    pristine = formState();
  }

  /** В открытой форме есть несохранённое. */
  const unsaved = (): boolean => mode !== 'none' && formState() !== pristine;

  /** Действие закроет открытую форму: при несохранённом — сначала вопрос; `false` — остаться. */
  const leaveForm = (): boolean => !unsaved() || window.confirm(LEAVE_EDIT);

  onMount(() => guardLeave(unsaved));

  const lines = (s: string) =>
    s
      .split('\n')
      .map((x) => x.trim())
      .filter(Boolean);
  const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  // Список — заново только при смене пользователя или пространства: свойства приходят из объекта
  // маршрута, и эффект перезапускается на каждый переход (открытие термина) — поиск и загруженный
  // список при этом целы (§5.1 п. 5, карточка вместо списка в узкой области).
  let listOf = '';
  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      if (listOf === `${u}|${s}`) return;
      listOf = `${u}|${s}`;
      search = '';
      trash = false;
      listNotice = '';
      load(u, s);
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  async function load(u: string, s: string) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await getGlossary(u, s);
      if (my !== seq) return;
      items = r.items;
    } catch (e) {
      if (my !== seq) return;
      items = [];
      // неудача — следующий переход в журнал загрузит список снова
      listOf = '';
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    conflicts = [];
    curError = '';
    if (!id) {
      cur = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'term') {
        cur = null;
        curError = 'Это не термин.';
        return;
      }
      cur = e;
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  let shown = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const list = q
      ? items.filter(
          (t) =>
            t.title.toLowerCase().includes(q) ||
            t.definition.toLowerCase().includes(q) ||
            t.aliases.some((a) => a.toLowerCase().includes(q)),
        )
      : items;
    return [...list].sort((a, b) => collator.compare(a.title, b.title));
  });

  /** Термины по первой букве (алфавитный указатель). */
  let groups = $derived.by(() => {
    const m = new Map<string, GlossaryItem[]>();
    for (const t of shown) {
      const k = (t.title.trim()[0] ?? '#').toUpperCase();
      m.set(k, [...(m.get(k) ?? []), t]);
    }
    return [...m.entries()];
  });

  function jump(letter: string) {
    document.getElementById(`gl-${letter}`)?.scrollIntoView({ block: 'start' });
  }

  /** Открыть термин: новая запись истории — «назад» браузера возвращает к списку (§5.1 п. 5). */
  function select(id: string) {
    notice = '';
    listNotice = '';
    go({ name: 'journal', user, space, journal: 'glossary', entry: id });
  }

  /** «Удалить» карточки: в корзину; карточка закрывается, итог — над списком. */
  function askDelete(c: Entry) {
    actionError = '';
    notice = '';
    ask = trashAsk(user, c, {
      deleted: async (msg) => {
        mode = 'none';
        cur = null;
        go({ name: 'journal', user, space, journal: 'glossary', entry: null }, { replace: true });
        await load(user, space);
        listNotice = msg;
      },
      stale: async (msg) => {
        await open(user, c.id);
        actionError = msg;
      },
      expired: onexpired,
      focus: () => listEl?.querySelector('[role="status"]'),
    });
  }

  function toggleTrash() {
    trash = !trash;
    listNotice = '';
    // назад к списку: восстановленные из корзины — в нём
    if (!trash) load(user, space);
  }

  /** Итог над списком (`Notice`): фокус после удаления — сюда, кнопки карточки уже нет. */
  let listEl = $state<HTMLElement | null>(null);

  /** Карточка или форма показана (в узкой области — вместо списка). */
  let detailOpen = $derived(cur !== null || curError !== '' || mode === 'create' || mode === 'edit');

  /** «← Словарь»: адрес без записи закрывает карточку, форма нового термина закрывается здесь. */
  function closeDetail() {
    mode = 'none';
    actionError = '';
    conflicts = [];
  }

  function startCreate() {
    if (!leaveForm()) return;
    fName = '';
    fDefinition = '';
    fAliases = '';
    fNot = '';
    fCode = '';
    fBody = '';
    actionError = '';
    conflicts = [];
    openForm('create');
  }

  function startEdit() {
    const c = cur;
    if (!c || !leaveForm()) return;
    fName = c.title;
    fDefinition = typeof c.data.definition === 'string' ? c.data.definition : '';
    fAliases = strs(c.data.aliases).join('\n');
    fNot = strs(c.data.not_to_confuse).join('\n');
    fCode = strs(c.data.code).join('\n');
    fBody = c.body;
    actionError = '';
    conflicts = [];
    openForm('edit');
  }

  function startRetract() {
    if (!leaveForm()) return;
    rReason = '';
    actionError = '';
    openForm('retract');
  }

  const formData = () => ({
    definition: fDefinition.trim(),
    aliases: lines(fAliases),
    not_to_confuse: lines(fNot),
    code: lines(fCode),
  });

  /** Действие: ошибка — понятным текстом; занятое имя — со ссылками; устаревшая ревизия — перечитать. */
  async function act(fn: () => Promise<string | void>, done: string) {
    if (busy) return;
    busy = true;
    actionError = '';
    conflicts = [];
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      mode = 'none';
      await load(user, space);
      if (target && target !== entry) select(target);
      else await open(user, target);
      notice = done;
    } catch (e) {
      const c = termConflicts(e);
      if (c.length) {
        conflicts = c;
        actionError = errorText(e);
      } else if (isStaleEntry(e) && cur) {
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

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    const title = fName.trim();
    if (!title || !fDefinition.trim()) {
      actionError = 'Укажите имя и определение.';
      return;
    }
    if (mode === 'create') {
      act(
        async () => (await createEntry(user, space, { kind: 'term', title, body: fBody, data: formData() })).id,
        'Термин добавлен.',
      );
    } else if (c) {
      act(async () => {
        await patchEntry(user, c.id, c.rev, { title, body: fBody, data: formData() });
      }, 'Термин сохранён.');
    }
  }

  function submitRetract(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    const reason = rReason.trim();
    if (!reason) {
      actionError = 'Укажите причину отзыва.';
      return;
    }
    act(async () => {
      await retractEntry(user, c.id, reason, c.rev);
      return '';
    }, 'Термин отозван.');
  }
</script>

<section class="journal glossary">
  <!-- список и карточка — рядом в широкой области, карточка вместо списка в узкой (§5.1 п. 5) -->
  <JournalSplit
    open={detailOpen}
    back={{ href: href({ name: 'journal', user, space, journal: 'glossary', entry: null }), label: JOURNAL_TITLE.glossary }}
    onclose={closeDetail}
    dirty={unsaved}
    {entry}
  >
    {#snippet list()}
      {@render termList()}
    {/snippet}
    {#snippet detail()}
      {@render termDetail()}
    {/snippet}
  </JournalSplit>
</section>

<Prompt bind:ask />

{#snippet termList()}
  <!-- «Корзина» — одна кнопка в обоих видах: фокус на ней переживает переключение -->
  <div class="row bar">
    {#if !trash}
      <input type="search" bind:value={search} placeholder="Поиск по имени, псевдониму, определению" aria-label="Поиск по словарю" />
      <button type="button" class="primary" onclick={startCreate}>Новый термин</button>
    {/if}
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  {#if trash}
    <TrashPanel {user} {space} kind="term" {onexpired} />
  {:else}
    {@render liveList()}
  {/if}
{/snippet}

{#snippet liveList()}
  <div bind:this={listEl}><Notice text={listNotice} /></div>

  {#if groups.length > 1}
    <!-- указатель букв — полоса (§5.2 п. 4): одна строка с прокруткой на телефоне и планшете -->
    <nav class="alpha strip" aria-label="Алфавитный указатель">
      {#each groups as [letter] (letter)}
        <button type="button" class="link" onclick={() => jump(letter)}>{letter}</button>
      {/each}
    </nav>
  {/if}

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if !loading && items.length === 0 && !error}<p class="muted">Терминов нет.</p>{/if}
  {#if !loading && items.length > 0 && shown.length === 0}<p class="muted">Ничего не найдено.</p>{/if}

  <div class="list" role="list" aria-label="Термины">
    {#each groups as [letter, terms] (letter)}
      <h2 id="gl-{letter}" class="letter">{letter}</h2>
      {#each terms as t (t.id)}
        <div role="listitem" class="term" class:selected={t.id === entry} data-term={t.id} data-entry={t.id}>
          <button type="button" class="link name" onclick={() => select(t.id)}>{t.title}</button>
          {#each t.aliases as a (a)}<span class="chip">{a}</span>{/each}
          <div class="muted def">{t.definition}</div>
        </div>
      {/each}
    {/each}
  </div>
{/snippet}

{#snippet termDetail()}
  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    <article class="card panel" aria-label="Карточка термина">
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <span class="chip status">{c.status === 'retracted' ? 'отозван' : c.status === 'superseded' ? 'заменён' : 'действует'}</span>
          <span class="muted small">{AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>
      <p class="body">{String(c.data.definition ?? '')}</p>
      {#if strs(c.data.aliases).length}
        <p><span class="muted">Псевдонимы:</span> {#each strs(c.data.aliases) as a (a)}<span class="chip">{a}</span>{/each}</p>
      {/if}
      {#if strs(c.data.not_to_confuse).length}
        <div>
          <span class="muted">Не путать с:</span>
          <ul>{#each strs(c.data.not_to_confuse) as x (x)}<li>{x}</li>{/each}</ul>
        </div>
      {/if}
      {#if strs(c.data.code).length}
        <p><span class="muted">В коде:</span> {#each strs(c.data.code) as x (x)}<code class="chip">{x}</code>{/each}</p>
      {/if}
      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if c.retract_reason}<p class="warn">Отозвано: {c.retract_reason}</p>{/if}

      <div class="row actions">
        {#if c.status === 'active'}
          <button type="button" onclick={startEdit}>Править</button>
          <button type="button" class="danger" onclick={startRetract}>Отозвать</button>
        {/if}
        <button type="button" class="danger" onclick={() => askDelete(c)} disabled={busy}>Удалить</button>
      </div>
      <Notice text={notice} />
      {#if mode === 'none' && actionError}<p class="error" role="alert">{actionError}</p>{/if}
      {#if mode === 'retract'}
        <form class="form" onsubmit={submitRetract} aria-label="Отзыв термина">
          <h3>Отозвать термин</h3>
          <p class="muted small">Запись остаётся в истории; имя и псевдонимы освобождаются.</p>
          <label>Причина <textarea bind:value={rReason} rows="2"></textarea></label>
          {@render formEnd('Отозвать')}
        </form>
      {/if}
    </article>
  {/if}

  {#if mode === 'create' || mode === 'edit'}
    <form class="form panel" onsubmit={submit} aria-label={mode === 'create' ? 'Новый термин' : 'Правка термина'}>
      <h3>{mode === 'create' ? 'Новый термин' : 'Правка термина'}</h3>
      <label>Имя <input bind:value={fName} required maxlength="200" /></label>
      <label>Определение <textarea bind:value={fDefinition} rows="3" required></textarea></label>
      <label>Псевдонимы <span class="muted small">(по одному в строке)</span> <textarea bind:value={fAliases} rows="2"></textarea></label>
      <label>Не путать с <span class="muted small">(по одному в строке)</span> <textarea bind:value={fNot} rows="2"></textarea></label>
      <label>Имена в коде <span class="muted small">(по одному в строке)</span> <textarea bind:value={fCode} rows="2"></textarea></label>
      <label>Пояснение <textarea bind:value={fBody} rows="3"></textarea></label>
      {#if conflicts.length}
        <div class="warn" role="alert" data-testid="term-conflicts">
          Занято другим термином:
          <ul>
            {#each conflicts as x (x.id + x.matched)}
              <li>
                «{x.matched}» — термин
                <a href={href({ name: 'journal', user, space, journal: 'glossary', entry: x.id })}>«{x.title}»</a>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {@render formEnd('Сохранить')}
    </form>
  {/if}
{/snippet}

{#snippet formEnd(label: string)}
  {#if actionError && conflicts.length === 0}<p class="error" role="alert">{actionError}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>{label}</button>
    <button type="button" onclick={() => ((mode = 'none'), (actionError = ''), (conflicts = []))}>Отмена</button>
  </div>
{/snippet}

<style>
  .bar {
    margin-bottom: 8px;
  }
  .bar input {
    flex: 1;
    min-width: 220px;
  }
  /* раскладка — `.strip` (app.css): при касании буквы — цели по 44 px с зазором `--touch-gap` */
  .alpha {
    margin-bottom: 8px;
  }
  .link {
    padding: 0 4px;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
  }
  .letter {
    margin: 12px 0 4px;
    font-size: 15px;
    color: var(--muted);
  }
  .term {
    padding: 4px 8px;
    border-radius: 6px;
  }
  .term.selected {
    background: var(--selected);
  }
  .name {
    font-weight: 500;
  }
  .def {
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
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
</style>
