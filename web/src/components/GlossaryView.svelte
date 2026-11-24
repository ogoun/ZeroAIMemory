<script lang="ts">
  // «Словарь» (M4.5 §2): действующие термины пространства по алфавиту, поиск по имени,
  // псевдониму и определению, карточка (псевдонимы, «не путать с», код) и правка. Сессия WebUI —
  // человек. Правка идёт с ревизией карточки (If-Match). Имя или псевдоним занят другим термином —
  // 409 term-conflict со ссылками на занявшие термины. Корзина (M7.2, спецификация §5.9 пп. 2–3):
  // «Удалить» карточки — в корзину, с подтверждением; «Корзина» — вместо списка удалённые термины с
  // «Восстановить» (`JournalTrash`, ленивый чанк; имя, занятое за это время, — 409 со ссылкой в
  // тексте ошибки).
  //
  // Карточка и формы (M7.3, спецификация §6.14, решения 80–86): ряд действий — сразу под шапкой
  // карточки, до определения и псевдонимов (правило Г); «Новый термин» и «Отозвать» — окна формы
  // (`FormDialog`, правило Б), после «Добавить» — карточка созданного термина (п. 6); «Править» —
  // правка в карточке на месте: вид карточки сменяется формой, фокус — на первом поле, «Сохранить»
  // — снова вид и фокус на заголовке, «Отмена» — фокус на «Править» (правило В, п. 3). Несохранённое
  // правки — `unsaved()`: «← Словарь» спрашивает «Закрыть без сохранения?» (`JournalSplit`), уход по
  // адресу — один вопрос стека проверок ухода (`guardLeave`); окна спрашивают о своём сами, журнал
  // сам не спрашивает (п. 4). 409 правки и отзыва — термин перечитан, набранное остаётся, нетронутое
  // поле — из свежего термина (`rebase`), поля-расхождения помечены, форма открыта (п. 5).
  import { onMount, tick, untrack } from 'svelte';
  import FormDialog from './FormDialog.svelte';
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
  import { focusSoon, revealTop } from '../lib/focus';
  import { isDirty, rebase, rebaseText } from '../lib/formDraft';
  import { AUTHOR_TEXT, JOURNAL_TITLE, fmtTime } from '../lib/journals';
  import { guardLeave } from '../lib/leaveEdit';
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
  let busy = $state(false);
  /** Ошибка действия карточки и формы правки на месте. */
  let actionError = $state('');
  let notice = $state('');
  /** «Корзина»: вместо списка — удалённые термины. */
  let trash = $state(false);
  /** Итог действия, закрывшего карточку (удаление), — над списком. */
  let listNotice = $state('');
  let ask = $state<Ask | null>(null);

  /** Поля термина в форме (списки — по одному в строке). */
  type TermFields = { name: string; definition: string; aliases: string; not: string; code: string; body: string };
  const EMPTY = (): TermFields => ({ name: '', definition: '', aliases: '', not: '', code: '', body: '' });
  /** Подписи полей в тексте 409. */
  const LABEL: Record<keyof TermFields, string> = {
    name: 'имя',
    definition: 'определение',
    aliases: 'псевдонимы',
    not: 'не путать с',
    code: 'имена в коде',
    body: 'пояснение',
  };
  type Conflict = { id: string; title: string; matched: string };

  /** Правка в карточке на месте (правило В). */
  let editing = $state(false);
  let ef = $state<TermFields>(EMPTY());
  /** Поля термина при открытии правки (после 409 — свежего): с ними сравнивается черновик. */
  let eBase: TermFields = EMPTY();
  /** Поля, изменённые и здесь, и в другом месте (409): помечены в форме. */
  let eClash = $state<(keyof TermFields)[]>([]);
  let eConflicts = $state<Conflict[]>([]);

  /** Окно «Новый термин» (правило Б). */
  let createOpen = $state(false);
  let cf = $state<TermFields>(EMPTY());
  let cConflicts = $state<Conflict[]>([]);
  /** Окно «Отозвать термин» (правило Б). */
  let retractOpen = $state(false);
  let rReason = $state('');
  /** Ошибка окна (создание, отзыв). */
  let dlgError = $state('');

  const lines = (s: string) =>
    s
      .split('\n')
      .map((x) => x.trim())
      .filter(Boolean);
  const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

  /** Поля формы правки из термина. */
  const fieldsOf = (c: Entry): TermFields => ({
    name: c.title,
    definition: typeof c.data.definition === 'string' ? c.data.definition : '',
    aliases: strs(c.data.aliases).join('\n'),
    not: strs(c.data.not_to_confuse).join('\n'),
    code: strs(c.data.code).join('\n'),
    body: c.body,
  });

  /** В правке на месте есть несохранённое (вопрос панели и проверка ухода); окна спрашивают сами. */
  const unsaved = (): boolean => editing && isDirty(ef, eBase);

  // правка на месте — в стеке проверок ухода (§6.14 п. 4); согласие уйти — черновик отпущен
  onMount(() => guardLeave(unsaved, () => (editing = false)));

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
    editing = false;
    actionError = '';
    eConflicts = [];
    curError = '';
    // другой термин (переход по адресу уже спросил о набранном): окна карточки закрываются
    if (id !== (cur?.id ?? null)) retractOpen = false;
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
        editing = false;
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
  let root = $state<HTMLElement | null>(null);
  /** Карточка термина (рядом со списком или вместо него). */
  const cardEl = () => root?.querySelector<HTMLElement>('article[aria-label="Карточка термина"]');

  /** Карточка показана (в узкой области — вместо списка). */
  let detailOpen = $derived(cur !== null || curError !== '');

  /** «← Словарь»: несохранённое панель уже спросила — правка отпускается. */
  function closeDetail() {
    editing = false;
    actionError = '';
    eConflicts = [];
  }

  /** Итог действия в карточке: фокус после закрытия окна (нажатая кнопка могла уйти). */
  const cardNotice = () => cardEl()?.querySelector<HTMLElement>('.card-notice [role="status"]');
  /** Кнопка ряда действий карточки по подписи. */
  const cardButton = (label: string) =>
    [...(cardEl()?.querySelectorAll<HTMLElement>('[aria-label="Действия с термином"] button') ?? [])].find((b) => b.textContent?.trim() === label);

  const formData = (f: TermFields) => ({
    definition: f.definition.trim(),
    aliases: lines(f.aliases),
    not_to_confuse: lines(f.not),
    code: lines(f.code),
  });

  // ---- «Новый термин» (окно) ----

  function startCreate() {
    cf = EMPTY();
    cConflicts = [];
    dlgError = '';
    createOpen = true;
  }

  const createDirty = (): boolean => isDirty(cf, EMPTY());

  /** «Добавить»: окно закрывается, открывается карточка созданного (адрес с `entry`: заголовок на
   * виду и в фокусе; на телефоне запись слоя окна становится записью карточки), список
   * перечитывается, итог — строкой (§6.14 п. 6). Занятое имя — ссылками в окне. */
  async function submitCreate() {
    const title = cf.name.trim();
    if (!title || !cf.definition.trim()) {
      dlgError = 'Укажите имя и определение.';
      return;
    }
    dlgError = '';
    cConflicts = [];
    let id: string;
    try {
      id = (await createEntry(user, space, { kind: 'term', title, body: cf.body, data: formData(cf) })).id;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      cConflicts = termConflicts(e);
      dlgError = errorText(e);
      return;
    }
    createOpen = false;
    select(id);
    notice = 'Термин добавлен.';
    await load(user, space);
  }

  // ---- правка в карточке ----

  /** «Править»: вид карточки сменяется формой, карточка — с начала, фокус — на первом поле. */
  async function startEdit() {
    const c = cur;
    if (!c) return;
    ef = fieldsOf(c);
    eBase = fieldsOf(c);
    eClash = [];
    eConflicts = [];
    actionError = '';
    notice = '';
    editing = true;
    await tick();
    revealTop(cardEl());
    cardEl()?.querySelector<HTMLElement>('form[aria-label="Правка термина"] input')?.focus();
  }

  /** «Отмена» правки: вид карточки, фокус — на «Править». */
  function cancelEdit() {
    editing = false;
    actionError = '';
    eConflicts = [];
    focusSoon(() => cardButton('Править'));
  }

  async function submitEdit(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c || busy) return;
    const title = ef.name.trim();
    if (!title || !ef.definition.trim()) {
      actionError = 'Укажите имя и определение.';
      return;
    }
    busy = true;
    actionError = '';
    eConflicts = [];
    try {
      await patchEntry(user, c.id, c.rev, { title, body: ef.body, data: formData(ef) });
    } catch (err) {
      busy = false;
      if (err instanceof ApiError && err.status === 401) return onexpired();
      const tc = termConflicts(err);
      if (tc.length) {
        eConflicts = tc;
        actionError = errorText(err);
      } else if (isStaleEntry(err)) await staleEdit(c.id, err);
      else actionError = errorText(err);
      return;
    }
    editing = false;
    try {
      await load(user, space);
      await open(user, c.id);
      notice = 'Термин сохранён.';
      focusSoon(() => cardEl()?.querySelector('h2'));
    } finally {
      busy = false;
    }
  }

  /** 409 правки: термин перечитан, черновик перенесён (`rebase`), форма открыта. */
  async function staleEdit(id: string, e: unknown) {
    let fresh: Entry;
    try {
      fresh = await getEntry(user, id);
    } catch (err) {
      actionError = `${errorText(e)} ${errorText(err)}`;
      return;
    }
    const next = fieldsOf(fresh);
    const r = rebase(ef, eBase, next);
    ef = r.fields;
    eBase = next;
    eClash = r.clash;
    cur = fresh;
    actionError = rebaseText(
      r.clash.map((k) => LABEL[k]),
      fresh.rev,
    );
    void load(user, space);
  }

  // ---- «Отозвать» (окно) ----

  function startRetract() {
    rReason = '';
    dlgError = '';
    actionError = '';
    notice = '';
    retractOpen = true;
  }

  /** Отзыв: успех — окно закрыто, карточка и список перечитаны, итог строкой; 409 — термин
   * перечитан, причина остаётся, окно открыто (отозван в другом месте — окно закрыто, причина — в
   * карточке). */
  async function submitRetract() {
    const c = cur;
    if (!c) return;
    const reason = rReason.trim();
    if (!reason) {
      dlgError = 'Укажите причину отзыва.';
      return;
    }
    dlgError = '';
    try {
      await retractEntry(user, c.id, reason, c.rev);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      if (isStaleEntry(e)) {
        let fresh: Entry;
        try {
          fresh = await getEntry(user, c.id);
        } catch (err) {
          dlgError = `${errorText(e)} ${errorText(err)}`;
          return;
        }
        cur = fresh;
        void load(user, space);
        if (fresh.status !== 'active') {
          retractOpen = false;
          actionError = `Пока окно было открыто, термин стал «${statusText(fresh)}» в другом месте — отзыв не нужен.`;
          return;
        }
        dlgError = rebaseText([], fresh.rev);
        return;
      }
      dlgError = errorText(e);
      return;
    }
    retractOpen = false;
    busy = true;
    try {
      await load(user, space);
      await open(user, c.id);
      notice = 'Термин отозван.';
    } finally {
      busy = false;
    }
  }

  const statusText = (c: Entry) => (c.status === 'retracted' ? 'отозван' : c.status === 'superseded' ? 'заменён' : 'действует');
</script>

<section class="journal glossary" bind:this={root}>
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
          <span class="chip status">{statusText(c)}</span>
          <span class="muted small">{AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}</span>
        </div>
      </header>

      {#if editing}
        <!-- правка на месте: форма вместо вида карточки (§6.14 п. 3) -->
        <form class="form" onsubmit={submitEdit} aria-label="Правка термина">
          <h3>Правка термина</h3>
          {@render termFields(ef, eClash, false)}
          {@render conflictList(eConflicts)}
          {#if actionError && eConflicts.length === 0}<p class="error" role="alert">{actionError}</p>{/if}
          <div class="row end">
            <button type="submit" class="primary" disabled={busy}>Сохранить</button>
            <button type="button" onclick={cancelEdit}>Отмена</button>
          </div>
        </form>
      {:else}
        <!-- ряд действий — сразу под шапкой, до определения и псевдонимов (правило Г) -->
        <div class="row actions" role="group" aria-label="Действия с термином">
          {#if c.status === 'active'}
            <button type="button" onclick={startEdit} disabled={busy}>Править</button>
            <button type="button" class="danger" onclick={startRetract} disabled={busy}>Отозвать</button>
          {/if}
          <button type="button" class="danger" onclick={() => askDelete(c)} disabled={busy}>Удалить</button>
        </div>
        <div class="card-notice"><Notice text={notice} /></div>
        {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

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
      {/if}
    </article>
  {/if}
{/snippet}

<!-- поля термина: окно «Новый термин» и правка в карточке; `clash` — поля-расхождения 409 -->
{#snippet termFields(f: TermFields, clash: (keyof TermFields)[], autofocus: boolean)}
  <label class:clash={clash.includes('name')}
    >Имя {@render note(clash, 'name')}<input bind:value={f.name} required maxlength="200" data-autofocus={autofocus ? '' : undefined} /></label
  >
  <label class:clash={clash.includes('definition')}>Определение {@render note(clash, 'definition')}<textarea bind:value={f.definition} rows="3" required></textarea></label>
  <label class:clash={clash.includes('aliases')}
    >Псевдонимы <span class="muted small">(по одному в строке)</span> {@render note(clash, 'aliases')}<textarea bind:value={f.aliases} rows="2"></textarea></label
  >
  <label class:clash={clash.includes('not')}
    >Не путать с <span class="muted small">(по одному в строке)</span> {@render note(clash, 'not')}<textarea bind:value={f.not} rows="2"></textarea></label
  >
  <label class:clash={clash.includes('code')}
    >Имена в коде <span class="muted small">(по одному в строке)</span> {@render note(clash, 'code')}<textarea bind:value={f.code} rows="2"></textarea></label
  >
  <label class:clash={clash.includes('body')}>Пояснение {@render note(clash, 'body')}<textarea bind:value={f.body} rows="3"></textarea></label>
{/snippet}

<!-- пометка поля-расхождения 409 в подписи (её читает и программа чтения с экрана) -->
{#snippet note(clash: (keyof TermFields)[], k: keyof TermFields)}
  {#if clash.includes(k)}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
{/snippet}

<!-- имя или псевдоним занят другим термином (409 term-conflict) — ссылками на занявшие -->
{#snippet conflictList(list: Conflict[])}
  {#if list.length}
    <div class="warn" role="alert" data-testid="term-conflicts">
      Занято другим термином:
      <ul>
        {#each list as x (x.id + x.matched)}
          <li>
            «{x.matched}» — термин
            <a href={href({ name: 'journal', user, space, journal: 'glossary', entry: x.id })}>«{x.title}»</a>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
{/snippet}

<!-- «Новый термин» — окно (§6.14, правило Б); после «Добавить» — карточка созданного (п. 6) -->
<FormDialog
  open={createOpen}
  title="Новый термин"
  submit="Добавить"
  error={cConflicts.length ? '' : dlgError}
  dirty={createDirty}
  onsubmit={submitCreate}
  onclose={() => ((createOpen = false), (dlgError = ''), (cConflicts = []))}
>
  {@render termFields(cf, [], true)}
  {@render conflictList(cConflicts)}
</FormDialog>

<!-- «Отозвать» — окно с причиной (правило Б) -->
<FormDialog
  open={retractOpen}
  title="Отозвать термин"
  subject={cur?.title}
  description="Запись остаётся в истории; имя и псевдонимы освобождаются."
  submit="Отозвать"
  danger
  error={dlgError}
  dirty={() => rReason.trim() !== ''}
  refocus={cardNotice}
  onsubmit={submitRetract}
  onclose={() => ((retractOpen = false), (dlgError = ''))}
>
  <label>Причина <textarea bind:value={rReason} rows="3" maxlength="2000" data-autofocus></textarea></label>
</FormDialog>

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
