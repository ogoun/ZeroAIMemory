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
  // группировка — `lib/sessionTree.ts` (внук — под тем же корнем, самоссылка и цикл — наверху).
  // Раскрытие и открытие карточки дочитывают потомков `GET /sessions?parent=`; до этого у сессии
  // на краю окна списка числа — «≥ N». Сессия из адреса вне окна — `GET /sessions/{id}`, нет её —
  // сообщение в карточке. Блок «Без сессии» — выдача MCP без
  // привязки к сессии агента (`GET /sessions/unattributed`) за 7 / 30 / 90 сут. по событиям, по
  // суткам — раскрытием. Раскладка — `JournalSplit` (§5.1 п. 5): в узкой области карточка сессии
  // вместо списка, в широкой — рядом; таблицы — в `.table-wrap`.
  import {
    ApiError,
    errorText,
    getSession,
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
  import { descendantsOf, groupSessions, maybePartial } from '../lib/sessionTree';
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
  /** Потомки сессии, дочитанные при раскрытии и открытии карточки (`?parent=`): сессия → список. */
  let fetched = $state<Record<string, SessionItem[]>>({});
  /** Раскрытые группы субагентов. */
  let expanded = $state<Record<string, boolean>>({});
  /** Сессии из адреса вне окна списка, дочитанные по id (`GET /sessions/{id}`). */
  let outside = $state<Record<string, SessionItem>>({});
  /** Сессия из адреса, которой нет у пользователя (404). */
  let missing = $state<string | null>(null);
  /** Идущие запросы потомков и сессий по id (повторно не шлются). */
  const pending = new Set<string>();

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
    outside = {};
    missing = null;
    pending.clear();
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

  /** Потомки сессии `id` с сервера (`?parent=`: субагенты и их субагенты) — один раз на сессию. */
  async function fetchSubs(id: string) {
    if (fetched[id] || pending.has(`p:${id}`)) return;
    pending.add(`p:${id}`);
    const n = seq;
    try {
      const r = await listSessions({ parent: id, limit: SUBAGENTS_LIMIT }, admin ? who : undefined);
      if (n === seq) fetched = { ...fetched, [id]: r.items };
    } catch (e) {
      if (n !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    } finally {
      if (n === seq) pending.delete(`p:${id}`);
    }
  }

  /** Раскрыть или свернуть субагентов сессии `id`; первое раскрытие дочитывает их полный список
   * (в окне списка могут быть не все). */
  function toggle(id: string) {
    const on = !expanded[id];
    expanded = { ...expanded, [id]: on };
    if (on) void fetchSubs(id);
  }

  /** Сессия из адреса вне окна списка — по id; нет у пользователя (404) — сообщение в карточке. */
  async function fetchOne(id: string) {
    if (pending.has(`s:${id}`)) return;
    pending.add(`s:${id}`);
    const n = seq;
    try {
      const s = await getSession(id, admin ? who : undefined);
      if (n === seq) outside = { ...outside, [id]: s };
    } catch (e) {
      if (n !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      if (e instanceof ApiError && e.status === 404) missing = id;
      else error = errorText(e);
    } finally {
      if (n === seq) pending.delete(`s:${id}`);
    }
  }

  /** Все известные сессии по id: окно списка, дочитанные потомки и сессии из адреса. */
  let byId = $derived.by(() => {
    const m = new Map<string, SessionItem>();
    for (const s of items) m.set(s.id, s);
    for (const list of Object.values(fetched)) for (const s of list) if (!m.has(s.id)) m.set(s.id, s);
    for (const s of Object.values(outside)) if (!m.has(s.id)) m.set(s.id, s);
    return m;
  });
  /** Верхний уровень и потомки под корнем (внук — под тем же корнем; самоссылка и цикл — наверху). */
  let groups = $derived(groupSessions(items, fetched));
  const subsOf = (id: string) => descendantsOf(id, groups, fetched);
  const withSubs = (s: SessionItem) => subsOf(s.id).reduce((n, c) => n + own(c), own(s));
  /** Число потомков может быть неполным: край окна списка или предел ответа (`≥`). */
  const partial = (s: SessionItem) => maybePartial(s, items, LIMIT, fetched[s.id], SUBAGENTS_LIMIT);
  const atLeast = (s: SessionItem) => (partial(s) ? '≥ ' : '');

  // ---- выдача без сессии ----

  let days = $state<number>(30);
  /** Ответ сервера — вместе с периодом и пользователем, за который он получен. */
  let un = $state<{ days: number; user: string | undefined; items: UnattributedRow[] } | null>(null);
  let unError = $state('');
  let unSeq = 0;
  async function loadUnattributed(d: number, user: string | undefined) {
    const n = ++unSeq;
    unError = '';
    try {
      const r = await unattributed(d, user);
      if (n === unSeq) un = { days: d, user, items: r.items };
    } catch (e) {
      if (n !== unSeq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      unError = errorText(e);
    }
  }

  $effect(() => {
    const d = days;
    const user = admin ? who : undefined;
    untrack(() => void loadUnattributed(d, user));
  });

  /** Строки выбранного периода; до ответа на новый период (или пользователя) — `null`: прежние
   * строки под новой подписью не показываются (ревью задачи 9, Minor 3). */
  let unItems = $derived(un && un.days === days && un.user === (admin ? who : undefined) ? un.items : null);

  /** Выдача без сессии по событиям за период. */
  let unEvents = $derived.by(() => {
    const m = new Map<string, { event: string; calls: number; tokens: number }>();
    for (const r of unItems ?? []) {
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
      // снимка нет (сессия из адреса без `snapshot_at` или снимок ушёл по сроку) — блока нет
      if (e instanceof ApiError && e.status === 404) return;
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
  // у сессии из адреса вне окна `snapshot_at` нет (`GET /sessions/{id}` его не отдаёт): снимок
  // пробуется, 404 — блока нет
  let snapId = $derived(opened && (opened.snapshot_at || outside[opened.id]) ? opened.id : null);
  let snapAt = $derived(opened?.snapshot_at ?? null);
  let subCount = $derived(items.filter((s) => s.parent_id && s.parent_id !== s.id).length);
  /** Адрес указывает на сессию, которой у пользователя нет. */
  let gone = $derived(session !== null && missing === session);

  // Открытая карточка — точные потомки сессии (ревью задачи 9, Minor 1): окно списка могло их
  // не вместить.
  $effect(() => {
    const id = opened?.id;
    if (id) untrack(() => void fetchSubs(id));
  });

  // Сессия из адреса вне окна списка — по id (ревью задачи 9, Minor 4).
  $effect(() => {
    const id = session;
    if (!id || !loaded || byId.has(id)) return;
    untrack(() => void fetchOne(id));
  });
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
      {#if s.parent_id && s.parent_id !== s.id}<span class="tag" title="родитель {s.parent_id}">субагент{s.agent_type ? ` ${s.agent_type}` : ''}</span>{/if}
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
          {#each groups.top as s (s.id)}
            {@const subs = subsOf(s.id)}
            {@const more = atLeast(s)}
            {@render sessionRow(s, false)}
            {#if subs.length}
              <tr class="subs" data-subagents-of={s.id}>
                <td colspan="4">
                  <button
                    type="button"
                    class="link toggle"
                    aria-expanded={!!expanded[s.id]}
                    title={more ? 'не все субагенты в окне списка: раскройте — полный список с сервера' : undefined}
                    onclick={() => toggle(s.id)}
                  >
                    <span aria-hidden="true">{expanded[s.id] ? '▾' : '▸'}</span> субагенты: {more}{subs.length}
                  </button>
                </td>
                <td class="n" data-with-subagents>с субагентами: {more}{num(withSubs(s))}</td>
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
    {#if unItems === null && !unError}
      <p class="muted" data-unattributed-loading>Загрузка…</p>
    {:else if unItems && !unItems.length}
      <p class="muted" data-unattributed-empty>За {days} сут. выдачи без сессии нет.</p>
    {:else if unItems}
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
              {#each unItems as r (`${r.day}/${r.event}`)}
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
  {#if gone}
    <section class="detail card" aria-label="Сессия {session}">
      <h3>Сессия <code>{session}</code></h3>
      <p class="muted" data-session-missing>
        Такой сессии нет у пользователя: сессии хранятся 30 суток после последнего обращения.
      </p>
    </section>
  {:else if opened}
    <section class="detail card" aria-label="Сессия {opened.id}">
      <h3>Сессия <code>{opened.id}</code></h3>
      <p class="muted small">
        {opened.agent ?? 'агент неизвестен'}{opened.machine ? ` · машина ${opened.machine}` : ''} · записей в память
        {opened.memory_writes} · эпоха {opened.seen_epoch}
      </p>
      {#if opened.parent_id && opened.parent_id !== opened.id}
        <p class="small">
          Субагент{opened.agent_type ? ` ${opened.agent_type}` : ''} сессии
          <a href={href({ name: 'sessions', session: opened.parent_id })}><code>{opened.parent_id}</code></a>
        </p>
      {/if}
      {#if openSubs.length}
        <p class="small" data-card-subagents>
          Субагенты: {atLeast(opened)}{openSubs.length} · с субагентами — {atLeast(opened)}{num(withSubs(opened))} токенов
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
    open={opened !== null || gone}
    back={{ href: href({ name: 'sessions', session: null }), label: 'Сессии' }}
    entry={opened?.id ?? (gone ? session : null)}
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
