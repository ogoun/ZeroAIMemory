<script lang="ts">
  // «Словарь» (M4.5 §2): действующие термины пространства по алфавиту, поиск по имени,
  // псевдониму и определению, карточка (псевдонимы, «не путать с», код) и правка. Сессия WebUI —
  // человек. Правка идёт с ревизией карточки (If-Match): запись изменилась — 409, карточка
  // перечитывается. Имя или псевдоним занят другим термином — 409 term-conflict со ссылками
  // на занявшие термины.
  import { untrack } from 'svelte';
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
  import { AUTHOR_TEXT, fmtTime } from '../lib/journals';
  import { go, href } from '../lib/router';

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

  // форма
  let fName = $state('');
  let fDefinition = $state('');
  let fAliases = $state('');
  let fNot = $state('');
  let fCode = $state('');
  let fBody = $state('');
  let rReason = $state('');

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

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      search = '';
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

  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'glossary', entry: id }, { replace: true });
  }

  function startCreate() {
    fName = '';
    fDefinition = '';
    fAliases = '';
    fNot = '';
    fCode = '';
    fBody = '';
    actionError = '';
    conflicts = [];
    mode = 'create';
  }

  function startEdit() {
    const c = cur;
    if (!c) return;
    fName = c.title;
    fDefinition = typeof c.data.definition === 'string' ? c.data.definition : '';
    fAliases = strs(c.data.aliases).join('\n');
    fNot = strs(c.data.not_to_confuse).join('\n');
    fCode = strs(c.data.code).join('\n');
    fBody = c.body;
    actionError = '';
    conflicts = [];
    mode = 'edit';
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
  <div class="row bar">
    <input type="search" bind:value={search} placeholder="Поиск по имени, псевдониму, определению" aria-label="Поиск по словарю" />
    <button type="button" class="primary" onclick={startCreate}>Новый термин</button>
  </div>

  {#if groups.length > 1}
    <nav class="alpha" aria-label="Алфавитный указатель">
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
        <div role="listitem" class="term" class:selected={t.id === entry} data-term={t.id}>
          <button type="button" class="link name" onclick={() => select(t.id)}>{t.title}</button>
          {#each t.aliases as a (a)}<span class="chip">{a}</span>{/each}
          <div class="muted def">{t.definition}</div>
        </div>
      {/each}
    {/each}
  </div>

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
          <button type="button" class="danger" onclick={() => ((rReason = ''), (actionError = ''), (mode = 'retract'))}>Отозвать</button>
        {/if}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}
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
</section>

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
  .alpha {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
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
    margin-top: 16px;
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
