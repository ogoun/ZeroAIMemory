<script lang="ts">
  // Insights (§7.5): таблица с фильтрами (вид, статус, надёжность, область, теги) и карточка
  // записи — замещение, отзыв, пересмотр, смена статуса надёжности (причина и доказательство
  // theorem человеку необязательны), триггеры, цепочка замещения, связи. Сессия WebUI — человек
  // (author_kind = human). Правка идёт с ревизией открытой записи (If-Match, §11.3): запись
  // изменилась — 409, карточка перечитывается. Столбец «показы / полезно / бесполезно» и
  // кнопки оценки (`POST /entries/{id}/feedback`, источник `webui`, §10.5).
  import { untrack } from 'svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import Links from './Links.svelte';
  import {
    ApiError,
    entryFeedback,
    errorText,
    getEntry,
    isStaleEntry,
    listEntries,
    patchEntry,
    retractEntry,
    supersedeEntry,
    type Certainty,
    type Entry,
    type EntryStatus,
    type NewTrigger,
    type TriggerType,
    type UsageCounts,
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
  /** Цепочка оборвалась (запись недоступна или предел шагов): до и после. */
  let chainCut = $state({ before: false, after: false });
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
  // новая запись человека по умолчанию без статуса: аксиому человек ставит явно (§7.5)
  let sCertainty = $state<Certainty | ''>('');
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
        dataKind: fKind || undefined,
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


  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    actionError = '';
    curError = '';
    chain = [];
    chainCut = { before: false, after: false };
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
      const c = await chainOf(u, e);
      if (my !== eseq) return;
      chain = c.items;
      chainCut = c.cut;
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  /** Цепочка замещения: от самой ранней записи к самой поздней (сама запись — внутри). */
  async function chainOf(
    u: string,
    e: Entry,
  ): Promise<{ items: Entry[]; cut: { before: boolean; after: boolean } }> {
    if (!e.supersedes_id && !e.superseded_by_id) return { items: [], cut: { before: false, after: false } };
    /** Шаги в одну сторону; `true` вторым значением — дальше есть, но недоступно. */
    async function walk(start: string | null, next: (x: Entry) => string | null): Promise<[Entry[], boolean]> {
      const out: Entry[] = [];
      let id = start;
      for (let i = 0; id; i++) {
        if (i >= CHAIN_MAX) return [out, true];
        try {
          const x = await getEntry(u, id);
          out.push(x);
          id = next(x);
        } catch {
          return [out, true];
        }
      }
      return [out, false];
    }
    const [before, cb] = await walk(e.supersedes_id, (x) => x.supersedes_id);
    const [after, ca] = await walk(e.superseded_by_id, (x) => x.superseded_by_id);
    return { items: [...before.reverse(), e, ...after], cut: { before: cb, after: ca } };
  }

  const usageText = (u: UsageCounts | undefined) => (u ? `${u.shown} / ${u.useful} / ${u.useless}` : '—');

  /** Оценка полезности: событие `usage` в базе записи, счётчики — перечитать. */
  async function rate(value: 'useful' | 'useless') {
    const c = cur;
    if (!c || busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      await entryFeedback(user, c.id, value);
      const e = await getEntry(user, c.id);
      if (cur?.id === c.id) cur = e;
      items = items.map((x) => (x.id === e.id ? { ...x, usage: e.usage } : x));
      notice = value === 'useful' ? 'Отмечено: полезно.' : 'Отмечено: бесполезно.';
    } catch (e) {
      fail(e, (m) => (actionError = m));
    } finally {
      busy = false;
    }
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
      if (isStaleEntry(e) && cur) {
        // запись изменилась в другой сессии: перечитать карточку (новая ревизия), объяснить
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

  function startSupersede() {
    if (!cur) return;
    sTitle = cur.title;
    sBody = cur.body;
    sKind = dataStr(cur.data, 'kind') || 'discovery';
    sCertainty = '';
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
        ...(sCertainty ? { certainty: sCertainty } : {}),
      }, c.rev);
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
      await retractEntry(user, c.id, reason, c.rev);
    }, 'Запись отозвана.');
  }

  function toggleRevisit() {
    const c = cur;
    if (!c) return;
    const to = c.status === 'revisit' ? 'active' : 'revisit';
    act(
      async () => {
        await patchEntry(user, c.id, c.rev, { status: to });
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
    // человеку причина необязательна (§7.5)
    const reason = cReason.trim();
    const to = cTo === '' ? null : cTo;
    // человеку доказательство для theorem необязательно (§7.5, R8): заполнено — уходит в
    // data.evidence, пусто — не шлётся
    let data: Record<string, unknown> | undefined;
    if (to === 'theorem') {
      const ev = { checked: evChecked.trim(), method: evMethod.trim(), result: evResult.trim() };
      if (ev.checked || ev.method || ev.result) data = { ...c.data, evidence: ev };
    }
    act(async () => {
      await patchEntry(user, c.id, c.rev, {
        certainty: to,
        ...(reason ? { reason } : {}),
        ...(data ? { data } : {}),
      });
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
      await patchEntry(user, c.id, c.rev, { triggers });
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
      <select bind:value={fKind} aria-label="Вид insight" onchange={reloadList}>
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
        <tr>
          <th>Надёжность</th><th>Заголовок</th><th>Вид</th><th>Статус</th><th>Область</th>
          <th title="Показы подсказками / полезно / бесполезно">Показы / полезно / бесполезно</th><th>Изменено</th>
        </tr>
      </thead>
      <tbody>
        {#each items as e (e.id)}
          <tr class:selected={e.id === entry} class="st-{e.status}" data-entry={e.id}>
            <td><CertaintyBadge value={e.certainty} /></td>
            <td>
              <button type="button" class="link" onclick={() => select(e.id)}>{e.title}</button>
              {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
            </td>
            <td>{dataStr(e.data, 'kind')}</td>
            <td>{e.status ? STATUS_TEXT[e.status] : ''}</td>
            <td>{e.area ?? ''}</td>
            <td class="usage">{usageText(e.usage)}</td>
            <td class="muted">{fmtTime(e.updated_at)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if !loading && items.length === 0 && !error}<p class="muted">Записей нет.</p>{/if}
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
        {#if c.certainty !== 'theorem'}<p class="muted small ev-title">Прежнее доказательство</p>{/if}
        <dl class="evidence" class:old={c.certainty !== 'theorem'}>
          <dt>Проверено</dt><dd>{String(ev.checked ?? '')}</dd>
          <dt>Метод</dt><dd>{String(ev.method ?? '')}</dd>
          <dt>Результат</dt><dd>{String(ev.result ?? '')}</dd>
        </dl>
      {/if}
      {#if c.retract_reason}<p class="warn">Отозвано: {c.retract_reason}</p>{/if}
      <div class="row rate" role="group" aria-label="Оценка полезности">
        <span class="muted small" data-testid="usage">
          показы {c.usage?.shown ?? 0} · полезно {c.usage?.useful ?? 0} · бесполезно {c.usage?.useless ?? 0}
        </span>
        <button type="button" onclick={() => rate('useful')} disabled={busy}>Полезно</button>
        <button type="button" onclick={() => rate('useless')} disabled={busy}>Бесполезно</button>
      </div>

      {#if chain.length > 1}
        <div class="chain">
          <h3>Цепочка замещения</h3>
          <ol>
            {#if chainCut.before}<li class="muted cut">…дальше недоступно</li>{/if}
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
            {#if chainCut.after}<li class="muted cut">…дальше недоступно</li>{/if}
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
                <option value="">без статуса</option>
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
          <p class="muted small">Вы действуете как человек: доступны все статусы, в том числе axiom; причина необязательна.</p>
          <fieldset class="row">
            <legend class="sr">Новый статус</legend>
            {#each CERTAINTIES as x (x)}
              <label class="opt" title={CERTAINTY_TEXT[x]}><input type="radio" bind:group={cTo} value={x} /> {x}</label>
            {/each}
            <label class="opt"><input type="radio" bind:group={cTo} value="" /> без статуса</label>
          </fieldset>
          {#if cTo === 'theorem'}
            <div class="ev">
              <p class="muted small">Доказательство (data.evidence) <span class="muted small">(необязательно)</span>: что проверено, как и с каким результатом.</p>
              <label>Проверено <input bind:value={evChecked} /></label>
              <label>Метод <input bind:value={evMethod} /></label>
              <label>Результат <input bind:value={evResult} /></label>
            </div>
          {/if}
          <label>Причина <span class="muted small">(необязательно)</span> <textarea bind:value={cReason} rows="2"></textarea></label>
          {@render formEnd('Сохранить')}
        </form>
      {:else if mode === 'triggers'}
        <div class="form" role="group" aria-label="Триггеры">
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
  .usage {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .rate {
    margin: 8px 0;
  }
  .rate button {
    padding: 2px 10px;
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
  .evidence.old {
    opacity: 0.75;
  }
  .ev-title {
    margin: 8px 0 0;
  }
  .cut {
    list-style: none;
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
