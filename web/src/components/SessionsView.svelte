<script lang="ts">
  // «Сессии» (#/sessions[/<id>], M6 §8): сессии агентов базы пользователя, свежие первыми, и
  // выданное им по событию — «событие · вызовов · токенов» (`session_tokens`, учёт сервера и хуков) —
  // и доля пометок «уже выдавалось» (`reread_calls`: у `read` — чтения REST и CLI, у `mcp_read` —
  // чтения из MCP; токены MCP — в `mcp`). Сводка — по показанным сессиям; строка сессии открывает её
  // карточку (сессия — в адресе). Время — локальное время браузера (`localTime`, M6 §4.4).
  // Администратор выбирает пользователя (`X-Zam-User`). У сессии со снимком (M6 §8а,
  // `snapshot_at`) карточка показывает блок «Снимок сессии» — только чтение; текст снимка пришёл из
  // журнала агента и выводится только как текст (без `{@html}` и без Markdown).
  //
  // M7.2 (§5.8 п. 3): сессии субагентов (`parent_id`) — под сессией-родителем, свёрнуты
  // («субагенты: N»), у родителя — строка «с субагентами» (его выдача и выдача субагентов);
  // раскрытие дочитывает полный список `GET /sessions?parent=`. Блок «Без сессии» — выдача MCP без
  // привязки к сессии агента (`GET /sessions/unattributed`) за 7 / 30 / 90 сут. по событиям, по
  // суткам — раскрытием. Раскладка — `JournalSplit` (§5.1 п. 5): в узкой области карточка сессии
  // вместо списка, в широкой — рядом; таблицы — в `.table-wrap`.
  import {
    ApiError,
    errorText,
    isAdminSession,
    listSessions,
    sessionSnapshot,
    unattributed,
    type Me,
    type SessionItem,
    type SessionSnapshot,
    type TokenStat,
    type UnattributedRow,
    type User,
  } from '../lib/api';
  import { untrack } from 'svelte';
  import { go, href } from '../lib/router';
  import { localTime } from '../lib/time';
  import JournalSplit from './JournalSplit.svelte';

  let {
    me,
    users,
    onexpired,
    session = null,
  }: { me: Me; users: User[]; onexpired: () => void; session?: string | null } = $props();

  /** Сессий в списке: субагенты занимают места списка, но сворачиваются под родителем. */
  const LIMIT = 200;
  /** Субагентов одной сессии при раскрытии (предел `GET /sessions`). */
  const SUBAGENTS_LIMIT = 500;
  /** Периоды блока «Без сессии», суток. */
  const PERIODS = [7, 30, 90] as const;
  /** Подписи событий учёта; неизвестное событие — как есть. */
  const EVENT_TITLE: Record<string, string> = {
    session_start: 'дайджест при старте',
    rules: 'Rules (резерв дайджеста и восстановления)',
    recovery: 'восстановление',
    prompt: 'подсказки к запросу',
    post_tool: 'подсказки после инструмента',
    brief: 'пакет субагенту',
    pre_tool: 'подсказки до инструмента (локально)',
    post_tool_local: 'после инструмента (локально)',
    subagent_local: 'субагент (локально)',
    search: 'поиск (REST, CLI)',
    read: 'чтение (REST, CLI)',
    mcp: 'инструменты MCP',
    mcp_read: 'из них чтения MCP',
  };
  /** Порядок строк: события сервера, затем локальные хуков, затем прочие по имени. */
  const ORDER = Object.keys(EVENT_TITLE);

  let admin = $derived(isAdminSession(me));
  let userId = $state('');
  let who = $derived(userId || me.user_id);
  let items = $state<SessionItem[]>([]);
  let loaded = $state(false);
  let error = $state('');
  /** Субагенты, дочитанные при раскрытии (`?parent=`): родитель → полный список. */
  let fetched = $state<Record<string, SessionItem[]>>({});
  /** Раскрытые группы субагентов. */
  let expanded = $state<Record<string, boolean>>({});

  type Row = { event: string; calls: number; tokens: number; reread: number };

  const rank = (e: string) => (ORDER.includes(e) ? ORDER.indexOf(e) : ORDER.length);
  const byEvent = (a: { event: string }, b: { event: string }) =>
    rank(a.event) - rank(b.event) || a.event.localeCompare(b.event);

  /** Строки таблицы событий: сумма по сессиям `list`. */
  function rows(list: SessionItem[]): Row[] {
    const m = new Map<string, Row>();
    for (const s of list) {
      for (const [event, t] of Object.entries(s.tokens ?? {}) as [string, TokenStat][]) {
        const r = m.get(event) ?? { event, calls: 0, tokens: 0, reread: 0 };
        r.calls += t.calls;
        r.tokens += t.tokens;
        r.reread += t.reread_calls ?? 0;
        m.set(event, r);
      }
    }
    return [...m.values()].sort(byEvent);
  }

  const total = (rs: Row[]) => rs.reduce((n, r) => n + r.tokens, 0);
  const own = (s: SessionItem) => total(rows([s]));
  const pct = (part: number, all: number) => (all ? `${Math.round((part / all) * 100)} %` : '—');
  const num = (n: number) => n.toLocaleString('ru-RU');
  /** Доля «уже выдавалось» строки события: повторов из вызовов. */
  const share = (rs: Row[], event: string) => {
    const r = rs.find((x) => x.event === event);
    return r && r.calls ? { reread: r.reread, calls: r.calls } : null;
  };

  let seq = 0;
  async function load() {
    const n = ++seq;
    error = '';
    fetched = {};
    expanded = {};
    try {
      const r = await listSessions({ limit: LIMIT }, admin ? who : undefined);
      if (n !== seq) return;
      items = r.items;
      loaded = true;
    } catch (e) {
      if (n !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    }
  }

  $effect(() => {
    void who;
    void load();
  });

  /** Другой пользователь (администратор): его сессии, открытая карточка закрывается. */
  function pickUser(id: string) {
    userId = id;
    if (session) go({ name: 'sessions', session: null }, { replace: true });
  }

  /** Строка сессии: открыть её карточку или закрыть открытую (адрес — `#/sessions/<id>`). */
  const select = (id: string) => go({ name: 'sessions', session: session === id ? null : id });

  /** Раскрыть или свернуть субагентов сессии `id`; первое раскрытие дочитывает их полный список
   * (в списке страницы могут быть не все). */
  async function toggle(id: string) {
    const on = !expanded[id];
    expanded = { ...expanded, [id]: on };
    if (!on || fetched[id]) return;
    const n = seq;
    try {
      const r = await listSessions({ parent: id, limit: SUBAGENTS_LIMIT }, admin ? who : undefined);
      if (n === seq) fetched = { ...fetched, [id]: r.items };
    } catch (e) {
      if (n !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    }
  }

  /** Все известные сессии по id: список страницы и дочитанные субагенты. */
  let byId = $derived.by(() => {
    const m = new Map<string, SessionItem>();
    for (const s of items) m.set(s.id, s);
    for (const list of Object.values(fetched)) for (const s of list) if (!m.has(s.id)) m.set(s.id, s);
    return m;
  });
  /** Субагенты по родителю из списка; дочитанные при раскрытии — вместо них. */
  let children = $derived.by(() => {
    const ids = new Set(items.map((s) => s.id));
    const m = new Map<string, SessionItem[]>();
    for (const s of items) {
      if (s.parent_id && ids.has(s.parent_id)) m.set(s.parent_id, [...(m.get(s.parent_id) ?? []), s]);
    }
    for (const [p, list] of Object.entries(fetched)) if (ids.has(p) && list.length) m.set(p, list);
    return m;
  });
  /** Строки верхнего уровня: сессии без родителя и субагенты, чей родитель не в списке. */
  let top = $derived.by(() => {
    const ids = new Set(items.map((s) => s.id));
    return items.filter((s) => !s.parent_id || !ids.has(s.parent_id));
  });
  const subsOf = (id: string) => children.get(id) ?? [];
  const withSubs = (s: SessionItem) => subsOf(s.id).reduce((n, c) => n + own(c), own(s));

  // ---- выдача без сессии ----

  let days = $state<number>(30);
  let un = $state<UnattributedRow[] | null>(null);
  let unError = $state('');
  let unSeq = 0;
  async function loadUnattributed(d: number, user: string | undefined) {
    const n = ++unSeq;
    unError = '';
    try {
      const r = await unattributed(d, user);
      if (n === unSeq) un = r.items;
    } catch (e) {
      if (n !== unSeq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      un = null;
      unError = errorText(e);
    }
  }

  $effect(() => {
    const d = days;
    const user = admin ? who : undefined;
    untrack(() => void loadUnattributed(d, user));
  });

  /** Выдача без сессии по событиям за период. */
  let unEvents = $derived.by(() => {
    const m = new Map<string, { event: string; calls: number; tokens: number }>();
    for (const r of un ?? []) {
      const x = m.get(r.event) ?? { event: r.event, calls: 0, tokens: 0 };
      x.calls += r.calls;
      x.tokens += r.tokens;
      m.set(r.event, x);
    }
    return [...m.values()].sort(byEvent);
  });
  let unTotal = $derived(
    unEvents.reduce((a, r) => ({ calls: a.calls + r.calls, tokens: a.tokens + r.tokens }), { calls: 0, tokens: 0 }),
  );

  /** Подписи поводов снимка; неизвестный — как есть. */
  const TRIGGER_TITLE: Record<string, string> = {
    post_compact: 'после сжатия',
    pre_compact: 'перед сжатием',
    session_start_compact: 'старт после сжатия',
    stop: 'конец ответа',
    session_end: 'конец сессии',
    catch_up: 'догоняющий (сессия оборвалась)',
    manual: 'вручную (zam session snapshot)',
  };

  let snap = $state<SessionSnapshot | null>(null);
  let snapError = $state('');
  let snapSeq = 0;
  async function loadSnapshot(id: string | null) {
    const n = ++snapSeq;
    snapError = '';
    // снимок той же сессии обновился — прежний текст остаётся до ответа: блок не мигает
    if (snap?.session_id !== id) snap = null;
    if (!id) return;
    try {
      const r = await sessionSnapshot(id, admin ? who : undefined);
      if (n === snapSeq) snap = r;
    } catch (e) {
      if (n !== snapSeq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      snapError = errorText(e);
    }
  }

  // Зависимости — только id открытой сессии и момент её снимка: перезагрузка списка даёт новый
  // объект `opened`, но снимок перечитывается, лишь когда он сменился.
  $effect(() => {
    const id = snapId;
    void snapAt;
    untrack(() => void loadSnapshot(id));
  });

  let summary = $derived(rows(items));
  let opened = $derived(session ? (byId.get(session) ?? null) : null);
  let openRows = $derived(opened ? rows([opened]) : []);
  let openSubs = $derived(opened ? subsOf(opened.id) : []);
  let snapId = $derived(opened?.snapshot_at ? opened.id : null);
  let snapAt = $derived(opened?.snapshot_at ?? null);
  let subCount = $derived(items.filter((s) => s.parent_id).length);
</script>

{#snippet table(rs: Row[], label: string)}
  {@const read = share(rs, 'read')}
  {@const mcp = share(rs, 'mcp_read')}
  <div class="table-wrap">
    <table aria-label={label}>
      <thead><tr><th>Событие</th><th class="n">Вызовов</th><th class="n">Токенов</th><th class="n">Уже выдавалось</th></tr></thead>
      <tbody>
        {#each rs as r (r.event)}
          <tr data-event={r.event}>
            <td>{EVENT_TITLE[r.event] ?? r.event} <code class="muted">{r.event}</code></td>
            <td class="n">{num(r.calls)}</td>
            <td class="n">{num(r.tokens)}</td>
            <td class="n">{r.reread ? num(r.reread) : ''}</td>
          </tr>
        {/each}
      </tbody>
      <tfoot><tr><th>Итого</th><th></th><th class="n">{num(total(rs))}</th><th></th></tr></tfoot>
    </table>
  </div>
  <p class="small share" data-share>
    Доля «уже выдавалось»: чтения —
    {#if read}{num(read.reread)} из {num(read.calls)} ({pct(read.reread, read.calls)}){:else}—{/if}{#if mcp && mcp.reread},
      чтения MCP — {num(mcp.reread)} из {num(mcp.calls)} ({pct(mcp.reread, mcp.calls)}){/if}.
  </p>
{/snippet}

{#snippet sessionRow(s: SessionItem, sub: boolean)}
  <tr data-session={s.id} data-entry={s.id} class:sel={session === s.id} class:sub>
    <td>
      <button type="button" class="link" onclick={() => select(s.id)}>{s.id}</button>
      {#if s.parent_id}<span class="tag" title="родитель {s.parent_id}">субагент{s.agent_type ? ` ${s.agent_type}` : ''}</span>{/if}
      {#if s.holdout}<span class="tag" title="контрольная группа A/B">holdout</span>{/if}
      {#if s.ended_at}<span class="tag">завершена</span>{/if}
      {#if s.snapshot_at}<span class="tag" title="снимок {localTime(s.snapshot_at)}">снимок</span>{/if}
    </td>
    <td>{s.agent ?? '—'}</td>
    <td class="muted">{localTime(s.started_at)}</td>
    <td class="muted">{localTime(s.last_seen_at)}</td>
    <td class="n">{num(own(s))}</td>
  </tr>
{/snippet}

{#snippet sessionList()}
  {#if loaded && !items.length}
    <p class="muted">Сессий нет.</p>
  {:else if items.length}
    <div class="table-wrap">
      <table aria-label="Список сессий">
        <thead><tr><th>Сессия</th><th>Агент</th><th>Начало</th><th>Последнее обращение</th><th class="n">Токенов</th></tr></thead>
        <tbody>
          {#each top as s (s.id)}
            {@const subs = subsOf(s.id)}
            {@render sessionRow(s, false)}
            {#if subs.length}
              <tr class="subs" data-subagents-of={s.id}>
                <td colspan="4">
                  <button type="button" class="link toggle" aria-expanded={!!expanded[s.id]} onclick={() => toggle(s.id)}>
                    <span aria-hidden="true">{expanded[s.id] ? '▾' : '▸'}</span> субагенты: {subs.length}
                  </button>
                </td>
                <td class="n" data-with-subagents>с субагентами: {num(withSubs(s))}</td>
              </tr>
              {#if expanded[s.id]}
                {#each subs as c (c.id)}{@render sessionRow(c, true)}{/each}
              {/if}
            {/if}
          {/each}
        </tbody>
      </table>
    </div>

    <section class="detail" aria-label="Сводка по сессиям">
      <h3>Всего по {items.length} {items.length === 1 ? 'сессии' : 'сессиям'}{subCount ? ` (из них субагентов — ${subCount})` : ''}</h3>
      {@render table(summary, 'Выданное по событиям')}
    </section>
  {/if}

  <section class="detail" aria-label="Без сессии">
    <h3>Без сессии</h3>
    <div class="row periods" role="group" aria-label="Период">
      {#each PERIODS as d (d)}
        <button type="button" aria-pressed={days === d} onclick={() => (days = d)}>{d} сут.</button>
      {/each}
    </div>
    <p class="muted small">
      Выдано через MCP по HTTP без привязки к сессии агента: этих токенов нет ни в одной сессии выше.
      Подключите MCP через <code>zam mcp</code> — он передаёт сессию агента (фрагмент <code>.mcp.json</code>
      — на странице <a href={href({ name: 'agentsetup' })}>подключения агента</a>).
    </p>
    {#if unError}<p class="error" role="alert">{unError}</p>{/if}
    {#if un === null && !unError}
      <p class="muted">Загрузка…</p>
    {:else if un && !un.length}
      <p class="muted" data-unattributed-empty>За {days} сут. выдачи без сессии нет.</p>
    {:else if un}
      <div class="table-wrap">
        <table aria-label="Без сессии по событиям">
          <thead><tr><th>Событие</th><th class="n">Вызовов</th><th class="n">Токенов</th></tr></thead>
          <tbody>
            {#each unEvents as r (r.event)}
              <tr data-event={r.event}>
                <td>{EVENT_TITLE[r.event] ?? r.event} <code class="muted">{r.event}</code></td>
                <td class="n">{num(r.calls)}</td>
                <td class="n">{num(r.tokens)}</td>
              </tr>
            {/each}
          </tbody>
          <tfoot><tr><th>Итого за {days} сут.</th><th class="n">{num(unTotal.calls)}</th><th class="n">{num(unTotal.tokens)}</th></tr></tfoot>
        </table>
      </div>
      <details class="days">
        <summary>По суткам</summary>
        <div class="table-wrap">
          <table aria-label="Без сессии по суткам">
            <thead><tr><th>Сутки (UTC)</th><th>Событие</th><th class="n">Вызовов</th><th class="n">Токенов</th></tr></thead>
            <tbody>
              {#each un as r (`${r.day}/${r.event}`)}
                <tr data-day={r.day} data-event={r.event}>
                  <td>{r.day}</td>
                  <td><code>{r.event}</code></td>
                  <td class="n">{num(r.calls)}</td>
                  <td class="n">{num(r.tokens)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </details>
    {/if}
  </section>
{/snippet}

{#snippet card()}
  {#if opened}
    <section class="detail card" aria-label="Сессия {opened.id}">
      <h3>Сессия <code>{opened.id}</code></h3>
      <p class="muted small">
        {opened.agent ?? 'агент неизвестен'}{opened.machine ? ` · машина ${opened.machine}` : ''} · записей в память
        {opened.memory_writes} · эпоха {opened.seen_epoch}
      </p>
      {#if opened.parent_id}
        <p class="small">
          Субагент{opened.agent_type ? ` ${opened.agent_type}` : ''} сессии
          <a href={href({ name: 'sessions', session: opened.parent_id })}><code>{opened.parent_id}</code></a>
        </p>
      {/if}
      {#if openSubs.length}
        <p class="small" data-card-subagents>
          Субагенты: {openSubs.length} · с субагентами — {num(withSubs(opened))} токенов
        </p>
      {/if}
      {@render table(openRows, 'Выданное сессии')}
      {#if snapError}<p class="error" role="alert">{snapError}</p>{/if}
      {#if snap}
        <section class="snapshot" aria-label="Снимок сессии">
          <h3>Снимок сессии</h3>
          <p class="muted small">
            {localTime(snap.taken_at)} · {TRIGGER_TITLE[snap.trigger] ?? snap.trigger}{snap.agent ? ` · ${snap.agent}` : ''} ·
            {num(snap.bytes)} байт{snap.status === 'partial' ? ' · неполный' : ''}{snap.skipped ? ` · пропущено строк ${snap.skipped}` : ''}
            — извлечено автоматически из журнала агента, секреты замаскированы.
          </p>
          {#if snap.data.compact_summary}
            <h4>Конспект (хвост)</h4>
            <pre class="text" data-part="summary">{snap.data.compact_summary}</pre>
          {/if}
          {#if snap.data.prompts.length}
            <h4>Запросы</h4>
            <ul data-part="prompts">{#each snap.data.prompts as p, i (i)}<li><pre class="text">{p}</pre></li>{/each}</ul>
          {/if}
          {#if snap.data.files.length}
            <h4>Файлы</h4>
            <ul data-part="files">{#each snap.data.files as f, i (i)}<li><code>{f}</code></li>{/each}</ul>
          {/if}
          {#if snap.data.commands.length}
            <h4>Команды</h4>
            <ul data-part="commands">
              {#each snap.data.commands as c, i (i)}<li><code>{c.cmd}</code>{#if c.failed}<span class="tag">ошибка</span>{/if}</li>{/each}
            </ul>
          {/if}
        </section>
      {/if}
    </section>
  {/if}
{/snippet}

<section class="panel">
  <h2>Сессии</h2>
  <p class="muted small">
    Выданное агентам по событию: дайджест и подсказки хуков, Rules сверх бюджета дайджеста, поиск и
    чтение (REST, CLI), инструменты MCP. «Уже выдавалось» — вызовы чтения, вернувшие текст, который
    сессия уже получала (перечитывать не нужно, если контекст не сжат). Сессии субагентов — под
    сессией-родителем.
  </p>
  {#if admin}
    <div class="row filters">
      <select value={who} onchange={(e) => pickUser((e.currentTarget as HTMLSelectElement).value)} aria-label="Пользователь">
        {#each users as u (u.id)}<option value={u.id}>{u.name}{u.id === me.user_id ? ' (вы)' : ''}</option>{/each}
      </select>
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <!-- список и карточка — рядом в широкой области, карточка вместо списка в узкой (§5.1 п. 5) -->
  <JournalSplit
    open={opened !== null}
    back={{ href: href({ name: 'sessions', session: null }), label: 'Сессии' }}
    entry={opened?.id ?? null}
  >
    {#snippet list()}
      {@render sessionList()}
    {/snippet}
    {#snippet detail()}
      {@render card()}
    {/snippet}
  </JournalSplit>
</section>

<style>
  .filters {
    margin: 8px 0 12px;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    text-align: left;
    cursor: pointer;
    font-family: var(--mono, monospace);
    font-size: 12px;
  }
  .toggle {
    font-family: inherit;
  }
  .sel {
    background: var(--selected);
  }
  .sub > td:first-child {
    padding-left: 24px;
  }
  .subs > td {
    padding-top: 0;
    font-size: 12px;
  }
  .tag {
    margin-left: 6px;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .n {
    text-align: right;
    white-space: nowrap;
  }
  .detail {
    margin-top: 16px;
  }
  .card {
    margin-top: 0;
  }
  .detail h3 {
    margin: 0 0 4px;
    font-size: 14px;
  }
  .periods {
    margin: 4px 0;
  }
  .periods button[aria-pressed='true'] {
    background: var(--selected);
    font-weight: 600;
  }
  .days summary {
    margin-top: 8px;
    cursor: pointer;
  }
  .share {
    margin: 6px 0 0;
  }
  .small {
    font-size: 12px;
  }
  .snapshot {
    margin-top: 12px;
  }
  .snapshot h4 {
    margin: 8px 0 2px;
    font-size: 13px;
  }
  .snapshot ul {
    margin: 0;
    padding-left: 18px;
  }
  .snapshot pre.text {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: inherit;
    font-size: 13px;
  }
  /* касание: цели 44 px (§5.1 п. 2), между кнопками периода — зазор полосы */
  @media (pointer: coarse) {
    .periods {
      gap: var(--touch-gap);
    }
    .days summary {
      display: flex;
      align-items: center;
      min-height: 44px;
    }
  }
</style>
