<script lang="ts">
  // Insights (§7.5): таблица с фильтрами (вид, статус, надёжность, область, теги) и карточка
  // записи — замещение, отзыв, пересмотр, смена статуса надёжности (причина обязательна, для
  // theorem — доказательство), триггеры, цепочка замещения, связи. Администратор в WebUI —
  // человек (author_kind = human): ему доступна axiom.
  import { untrack } from 'svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import Links from './Links.svelte';
  import {
    ApiError,
    errorText,
    getEntry,
    listEntries,
    patchEntry,
    retractEntry,
    supersedeEntry,
    type Certainty,
    type Entry,
    type EntryStatus,
    type NewTrigger,
    type TriggerType,
  } from '../lib/api';
  import {
    AUTHOR_TEXT,
    CERTAINTIES,
    CERTAINTY_TEXT,
    INSIGHT_KINDS,
    STATUSES,
    STATUS_TEXT,
    TRIGGER_TEXT,
    TRIGGER_TYPES,
    dataStr,
    fmtTime,
  } from '../lib/journals';
  import { go } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    /** Открытая запись. */
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const PAGE = 100;
  /** Предел шагов цепочки замещения в каждую сторону. */
  const CHAIN_MAX = 20;

  // ---- таблица ----
  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let fKind = $state('');
  let fStatus = $state<'' | EntryStatus>('');
  let fCertainty = $state<'' | Certainty>('');
  let fArea = $state('');
  let fTag = $state('');
  let seq = 0;

  // ---- карточка ----
  let cur = $state<Entry | null>(null);
  let curError = $state('');
  let chain = $state<Entry[]>([]);
  type Mode = 'none' | 'supersede' | 'retract' | 'certainty' | 'triggers';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');
  let eseq = 0;

  // формы
  let sTitle = $state('');
  let sBody = $state('');
  let sKind = $state('discovery');
  let sCertainty = $state<Certainty>('hypothesis');
  let rReason = $state('');
  let cTo = $state<'' | Certainty>('');
  let cReason = $state('');
  let evChecked = $state('');
  let evMethod = $state('');
  let evResult = $state('');
  let tDraft = $state<NewTrigger[]>([]);
  let tType = $state<TriggerType>('keyword');
  let tPattern = $state('');
  let tTools = $state('');

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      fKind = '';
      fStatus = '';
      fCertainty = '';
      fArea = '';
      fTag = '';
      load(u, s, true);
    });
  });

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, {
        kind: 'insight',
        status: fStatus ? [fStatus] : undefined,
        certainty: fCertainty ? [fCertainty] : undefined,
        area: fArea.trim() || undefined,
        tag: fTag.trim() || undefined,
        cursor: reset ? null : next,
        limit: PAGE,
      });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) items = [];
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  const reloadList = () => load(user, space, true);

  function applyText(e: SubmitEvent) {
    e.preventDefault();
    reloadList();
  }

  let shown = $derived(fKind ? items.filter((e) => dataStr(e.data, 'kind') === fKind) : items);

  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    curError = '';
    chain = [];
    if (!id) {
      cur = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'insight') {
        cur = null;
        curError = 'Это не insight.';
        return;
      }
      cur = e;
      chain = await chainOf(u, e);
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  /** Цепочка замещения: от самой ранней записи к самой поздней (сама запись — внутри). */
  async function chainOf(u: string, e: Entry): Promise<Entry[]> {
    if (!e.supersedes_id && !e.superseded_by_id) return [];
    const before: Entry[] = [];
    const after: Entry[] = [];
    let p = e.supersedes_id;
    for (let i = 0; p && i < CHAIN_MAX; i++) {
      try {
        const x = await getEntry(u, p);
        before.unshift(x);
        p = x.supersedes_id;
      } catch {
        break;
      }
    }
    let n = e.superseded_by_id;
    for (let i = 0; n && i < CHAIN_MAX; i++) {
      try {
        const x = await getEntry(u, n);
        after.push(x);
        n = x.superseded_by_id;
      } catch {
        break;
      }
    }
    return [...before, e, ...after];
  }

  function select(id: string) {
    notice = '';
    go({ name: 'journal', user, space, journal: 'insights', entry: id }, { replace: true });
  }

  /** Действие над карточкой: ошибка — понятным текстом, успех — перечитать. */
  async function act(fn: () => Promise<string | void>, done: string) {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      mode = 'none';
      await reloadList();
      if (target && target !== entry) {
        select(target);
      } else {
        await open(user, target);
      }
      // после select: смена записи сбрасывает уведомление, а это — о только что сделанном
      notice = done;
    } catch (e) {
      fail(e, (m) => (actionError = m));
    } finally {
      busy = false;
    }
  }

  function startSupersede() {
    if (!cur) return;
    sTitle = cur.title;
    sBody = cur.body;
    sKind = dataStr(cur.data, 'kind') || 'discovery';
    sCertainty = cur.certainty ?? 'hypothesis';
    mode = 'supersede';
  }

  function submitSupersede(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    act(async () => {
      const data: Record<string, unknown> = { ...c.data, kind: sKind };
      const r = await supersedeEntry(user, c.id, {
        title: sTitle.trim(),
        body: sBody,
        data,
        tags: c.tags,
        area: c.area,
        certainty: sCertainty,
      });
      return r.entry.id;
    }, 'Запись заменена: прежняя помечена superseded.');
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
      await retractEntry(user, c.id, reason);
    }, 'Запись отозвана.');
  }

  function toggleRevisit() {
    const c = cur;
    if (!c) return;
    const to = c.status === 'revisit' ? 'active' : 'revisit';
    act(
      async () => {
        await patchEntry(user, c.id, { status: to });
      },
      to === 'revisit' ? 'Запись отправлена на пересмотр.' : 'Запись снова действует.',
    );
  }

  function startCertainty() {
    if (!cur) return;
    cTo = cur.certainty ?? '';
    cReason = '';
    const ev = cur.data.evidence as Record<string, unknown> | undefined;
    evChecked = typeof ev?.checked === 'string' ? ev.checked : '';
    evMethod = typeof ev?.method === 'string' ? ev.method : '';
    evResult = typeof ev?.result === 'string' ? ev.result : '';
    mode = 'certainty';
  }

  function submitCertainty(e: SubmitEvent) {
    e.preventDefault();
    const c = cur;
    if (!c) return;
    const reason = cReason.trim();
    if (!reason) {
      actionError = 'Укажите причину смены статуса надёжности.';
      return;
    }
    const to = cTo === '' ? null : cTo;
    let data: Record<string, unknown> | undefined;
    if (to === 'theorem') {
      const ev = { checked: evChecked.trim(), method: evMethod.trim(), result: evResult.trim() };
      if (!ev.checked || !ev.method || !ev.result) {
        actionError = 'Для theorem заполните доказательство: что проверено, метод и результат.';
        return;
      }
      data = { ...c.data, evidence: ev };
    }
    act(async () => {
      await patchEntry(user, c.id, { certainty: to, reason, ...(data ? { data } : {}) });
    }, 'Статус надёжности изменён.');
  }

  function startTriggers() {
    if (!cur) return;
    tDraft = (cur.triggers ?? []).map((t) => ({ type: t.type, pattern: t.pattern, tools: t.tools }));
    tPattern = '';
    tTools = '';
    mode = 'triggers';
  }

  function addTrigger(e: SubmitEvent) {
    e.preventDefault();
    const pattern = tPattern.trim();
    if (!pattern) return;
    const tools = tTools
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    tDraft = [...tDraft, { type: tType, pattern, tools: tools.length ? tools : null }];
    tPattern = '';
    tTools = '';
  }

  function saveTriggers() {
    const c = cur;
    if (!c) return;
    const triggers = tDraft;
    act(async () => {
      await patchEntry(user, c.id, { triggers });
    }, 'Триггеры сохранены.');
  }

  const live = (e: Entry) => e.status === 'active' || e.status === 'revisit';
  const evidenceOf = (e: Entry): Record<string, unknown> | null => {
    const ev = e.data.evidence;
    return ev && typeof ev === 'object' ? (ev as Record<string, unknown>) : null;
  };
</script>

<section class="journal insights">
  <form class="filters row" onsubmit={applyText}>
    <label class="row">
      Вид
      <select bind:value={fKind} aria-label="Вид insight">
        <option value="">все</option>
        {#each INSIGHT_KINDS as k (k)}<option value={k}>{k}</option>{/each}
      </select>
    </label>
    <label class="row">
      Статус
      <select bind:value={fStatus} aria-label="Статус" onchange={reloadList}>
        <option value="">все</option>
        {#each STATUSES as s (s)}<option value={s}>{STATUS_TEXT[s]}</option>{/each}
      </select>
    </label>
    <label class="row">
      Надёжность
      <select bind:value={fCertainty} aria-label="Надёжность" onchange={reloadList}>
        <option value="">любая</option>
        {#each CERTAINTIES as c (c)}<option value={c}>{c}</option>{/each}
      </select>
    </label>
    <input bind:value={fArea} placeholder="область" aria-label="Область" />
    <input bind:value={fTag} placeholder="тег" aria-label="Тег" />
    <button type="submit">Применить</button>
  </form>

  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <div class="table-wrap">
    <table aria-label="Insights">
      <thead>
        <tr><th>Надёжность</th><th>Заголовок</th><th>Вид</th><th>Статус</th><th>Область</th><th>Изменено</th></tr>
      </thead>
      <tbody>
        {#each shown as e (e.id)}
          <tr class:selected={e.id === entry} class="st-{e.status}" data-entry={e.id}>
            <td><CertaintyBadge value={e.certainty} /></td>
            <td>
              <button type="button" class="link" onclick={() => select(e.id)}>{e.title}</button>
              {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
            </td>
            <td>{dataStr(e.data, 'kind')}</td>
            <td>{e.status ? STATUS_TEXT[e.status] : ''}</td>
            <td>{e.area ?? ''}</td>
            <td class="muted">{fmtTime(e.updated_at)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if !loading && shown.length === 0 && !error}<p class="muted">Записей нет.</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if next && !loading}
    <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
  {/if}

  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    <article class="card panel" aria-label="Карточка insight">
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <CertaintyBadge value={c.certainty} />
          {#if !c.certainty}<span class="muted">без статуса надёжности</span>{/if}
          <span class="chip status">{c.status ? STATUS_TEXT[c.status] : ''}</span>
          <span class="muted">{dataStr(c.data, 'kind')}</span>
          {#if c.area}<span class="chip">{c.area}</span>{/if}
          <span class="muted small">
            {AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}
          </span>
        </div>
      </header>
      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if evidenceOf(c)}
        {@const ev = evidenceOf(c)!}
        <dl class="evidence">
          <dt>Проверено</dt><dd>{String(ev.checked ?? '')}</dd>
          <dt>Метод</dt><dd>{String(ev.method ?? '')}</dd>
          <dt>Результат</dt><dd>{String(ev.result ?? '')}</dd>
        </dl>
      {/if}
      {#if c.retract_reason}<p class="warn">Отозвано: {c.retract_reason}</p>{/if}

      {#if chain.length > 1}
        <div class="chain">
          <h3>Цепочка замещения</h3>
          <ol>
            {#each chain as x (x.id)}
              <li class:here={x.id === c.id}>
                {#if x.id === c.id}
                  <strong>{x.title}</strong>
                {:else}
                  <button type="button" class="link" onclick={() => select(x.id)}>{x.title}</button>
                {/if}
                <span class="muted small">{x.status ? STATUS_TEXT[x.status] : ''}</span>
              </li>
            {/each}
          </ol>
        </div>
      {/if}

      <div class="row actions">
        {#if live(c)}
          <button type="button" onclick={startSupersede}>Заменить</button>
          <button type="button" onclick={toggleRevisit} disabled={busy}>
            {c.status === 'revisit' ? 'Вернуть в действующие' : 'Пересмотреть'}
          </button>
          <button type="button" onclick={startCertainty}>Сменить статус надёжности</button>
          <button type="button" onclick={startTriggers}>Триггеры</button>
        {/if}
        {#if c.status !== 'retracted'}
          <button type="button" class="danger" onclick={() => ((rReason = ''), (mode = 'retract'))}>Отозвать</button>
        {/if}
      </div>
      {#if notice}<p class="muted" role="status">{notice}</p>{/if}

      {#if mode === 'supersede'}
        <form class="form" onsubmit={submitSupersede} aria-label="Замена insight">
          <h3>Заменить запись</h3>
          <p class="muted small">Новая запись займёт место этой; прежняя останется в истории как superseded.</p>
          <label>Заголовок <input bind:value={sTitle} required /></label>
          <label>Текст <textarea bind:value={sBody} rows="5"></textarea></label>
          <div class="row">
            <label class="row">Вид
              <select bind:value={sKind}>{#each INSIGHT_KINDS as k (k)}<option value={k}>{k}</option>{/each}</select>
            </label>
            <label class="row">Надёжность
              <select bind:value={sCertainty} aria-label="Надёжность новой записи">
                {#each CERTAINTIES as x (x)}<option value={x}>{x}</option>{/each}
              </select>
            </label>
          </div>
          {@render formEnd('Заменить')}
        </form>
      {:else if mode === 'retract'}
        <form class="form" onsubmit={submitRetract} aria-label="Отзыв insight">
          <h3>Отозвать запись</h3>
          <p class="muted small">Отзыв ничего не удаляет: запись остаётся в истории со статусом retracted.</p>
          <label>Причина <textarea bind:value={rReason} rows="2"></textarea></label>
          {@render formEnd('Отозвать')}
        </form>
      {:else if mode === 'certainty'}
        <form class="form" onsubmit={submitCertainty} aria-label="Статус надёжности">
          <h3>Статус надёжности</h3>
          <p class="muted small">Вы действуете как человек: доступны все статусы, в том числе axiom.</p>
          <fieldset class="row">
            <legend class="sr">Новый статус</legend>
            {#each CERTAINTIES as x (x)}
              <label class="opt" title={CERTAINTY_TEXT[x]}><input type="radio" bind:group={cTo} value={x} /> {x}</label>
            {/each}
            <label class="opt"><input type="radio" bind:group={cTo} value="" /> без статуса</label>
          </fieldset>
          {#if cTo === 'theorem'}
            <div class="ev">
              <p class="muted small">Доказательство (data.evidence): что проверено, как и с каким результатом.</p>
              <label>Проверено <input bind:value={evChecked} /></label>
              <label>Метод <input bind:value={evMethod} /></label>
              <label>Результат <input bind:value={evResult} /></label>
            </div>
          {/if}
          <label>Причина <span class="muted small">(обязательно)</span> <textarea bind:value={cReason} rows="2"></textarea></label>
          {@render formEnd('Сохранить')}
        </form>
      {:else if mode === 'triggers'}
        <div class="form" aria-label="Триггеры">
          <h3>Триггеры</h3>
          {#if tDraft.length === 0}<p class="muted">Триггеров нет.</p>{/if}
          <ul class="triggers">
            {#each tDraft as t, i (i)}
              <li>
                <span class="chip">{TRIGGER_TEXT[t.type]}</span> <code>{t.pattern}</code>
                {#if t.tools?.length}<span class="muted small">({t.tools.join(', ')})</span>{/if}
                <button type="button" class="danger small-btn" aria-label="Удалить триггер {t.pattern}"
                  onclick={() => (tDraft = tDraft.filter((_, j) => j !== i))}>×</button>
              </li>
            {/each}
          </ul>
          <form class="row" onsubmit={addTrigger}>
            <select bind:value={tType} aria-label="Тип триггера">
              {#each TRIGGER_TYPES as x (x)}<option value={x}>{TRIGGER_TEXT[x]}</option>{/each}
            </select>
            <input bind:value={tPattern} placeholder="шаблон" aria-label="Шаблон триггера" />
            <input bind:value={tTools} placeholder="инструменты: Write, Bash" aria-label="Инструменты" />
            <button type="submit">Добавить</button>
          </form>
          {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
          <div class="row end">
            <button type="button" class="primary" disabled={busy} onclick={saveTriggers}>Сохранить триггеры</button>
            <button type="button" onclick={() => ((mode = 'none'), (actionError = ''))}>Отмена</button>
          </div>
        </div>
      {:else if actionError}
        <p class="error" role="alert">{actionError}</p>
      {/if}

      <div class="links">
        <h3>Связи</h3>
        <Links {user} kind="entry" id={c.id} {onexpired} />
      </div>
    </article>
  {/if}
</section>

{#snippet formEnd(label: string)}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>{label}</button>
    <button type="button" onclick={() => ((mode = 'none'), (actionError = ''))}>Отмена</button>
  </div>
{/snippet}

<style>
  .filters {
    margin-bottom: 12px;
  }
  .filters label {
    gap: 6px;
  }
  .filters input {
    width: 130px;
  }
  .table-wrap {
    overflow-x: auto;
  }
  tr.selected {
    background: var(--selected);
  }
  tr.st-superseded td,
  tr.st-retracted td {
    color: var(--muted);
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
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
  .meta {
    gap: 6px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .evidence {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 8px 0;
    font-size: 14px;
  }
  .evidence dt {
    color: var(--muted);
  }
  .evidence dd {
    margin: 0;
  }
  .chain ol {
    margin: 0;
    padding-left: 20px;
  }
  .chain li.here {
    font-weight: 500;
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 15px;
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
  }
  .form label:not(.opt):not(.row) {
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
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .ev {
    display: grid;
    gap: 6px;
    padding: 8px;
    border-left: 3px solid var(--callout-tip);
  }
  .end {
    justify-content: flex-end;
  }
  .triggers {
    margin: 0;
    padding-left: 18px;
  }
  .small {
    font-size: 12px;
  }
  .small-btn {
    padding: 0 6px;
  }
</style>
