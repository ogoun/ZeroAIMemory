<script lang="ts">
  // «Сессии» (#/sessions, M6 §8): сессии агентов базы пользователя, свежие первыми, и выданное
  // им по событию — «событие · вызовов · токенов» (`session_tokens`, учёт сервера и хуков) — и
  // доля пометок «уже выдавалось» (`reread_calls`: у `read` — чтения REST и CLI, у `mcp_read` —
  // чтения из MCP; токены MCP — в `mcp`). Сводка — по показанным сессиям; строка сессии открывает её таблицу. Время — локальное
  // время браузера (`localTime`, M6 §4.4). Администратор выбирает пользователя (`X-Zam-User`).
  import { ApiError, errorText, isAdminSession, listSessions, type Me, type SessionItem, type TokenStat, type User } from '../lib/api';
  import { localTime } from '../lib/time';

  let { me, users, onexpired }: { me: Me; users: User[]; onexpired: () => void } = $props();

  const LIMIT = 50;
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
  let open = $state<string | null>(null);

  type Row = { event: string; calls: number; tokens: number; reread: number };

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
    const rank = (e: string) => (ORDER.includes(e) ? ORDER.indexOf(e) : ORDER.length);
    return [...m.values()].sort((a, b) => rank(a.event) - rank(b.event) || a.event.localeCompare(b.event));
  }

  const total = (rs: Row[]) => rs.reduce((n, r) => n + r.tokens, 0);
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
    open = null;
    void load();
  });

  let summary = $derived(rows(items));
  let opened = $derived(items.find((s) => s.id === open) ?? null);
  let openRows = $derived(opened ? rows([opened]) : []);
</script>

{#snippet table(rs: Row[], label: string)}
  {@const read = share(rs, 'read')}
  {@const mcp = share(rs, 'mcp_read')}
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
  <p class="small share" data-share>
    Доля «уже выдавалось»: чтения —
    {#if read}{num(read.reread)} из {num(read.calls)} ({pct(read.reread, read.calls)}){:else}—{/if}{#if mcp && mcp.reread},
      чтения MCP — {num(mcp.reread)} из {num(mcp.calls)} ({pct(mcp.reread, mcp.calls)}){/if}.
  </p>
{/snippet}

<section class="panel">
  <h2>Сессии</h2>
  <p class="muted small">
    Выданное агентам по событию: дайджест и подсказки хуков, Rules сверх бюджета дайджеста, поиск и
    чтение (REST, CLI), инструменты MCP. «Уже выдавалось» — вызовы чтения, вернувшие текст, который
    сессия уже получала (перечитывать не нужно, если контекст не сжат).
  </p>
  {#if admin}
    <div class="row filters">
      <select value={who} onchange={(e) => (userId = (e.currentTarget as HTMLSelectElement).value)} aria-label="Пользователь">
        {#each users as u (u.id)}<option value={u.id}>{u.name}{u.id === me.user_id ? ' (вы)' : ''}</option>{/each}
      </select>
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if loaded && !items.length}
    <p class="muted">Сессий нет.</p>
  {:else if items.length}
    <table aria-label="Список сессий">
      <thead><tr><th>Сессия</th><th>Агент</th><th>Начало</th><th>Последнее обращение</th><th class="n">Токенов</th></tr></thead>
      <tbody>
        {#each items as s (s.id)}
          {@const rs = rows([s])}
          <tr data-session={s.id} class:sel={open === s.id}>
            <td>
              <button type="button" class="link" onclick={() => (open = open === s.id ? null : s.id)}>{s.id}</button>
              {#if s.parent_id}<span class="tag" title="родитель {s.parent_id}">субагент{s.agent_type ? ` ${s.agent_type}` : ''}</span>{/if}
              {#if s.holdout}<span class="tag" title="контрольная группа A/B">holdout</span>{/if}
              {#if s.ended_at}<span class="tag">завершена</span>{/if}
            </td>
            <td>{s.agent ?? '—'}</td>
            <td class="muted">{localTime(s.started_at)}</td>
            <td class="muted">{localTime(s.last_seen_at)}</td>
            <td class="n">{num(total(rs))}</td>
          </tr>
        {/each}
      </tbody>
    </table>

    {#if opened}
      <section class="detail" aria-label="Сессия {opened.id}">
        <h3>Сессия <code>{opened.id}</code></h3>
        <p class="muted small">
          {opened.agent ?? 'агент неизвестен'}{opened.machine ? ` · машина ${opened.machine}` : ''} · записей в память
          {opened.memory_writes} · эпоха {opened.seen_epoch}
        </p>
        {@render table(openRows, 'Выданное сессии')}
      </section>
    {/if}

    <section class="detail" aria-label="Сводка по сессиям">
      <h3>Всего по {items.length} {items.length === 1 ? 'сессии' : 'сессиям'}</h3>
      {@render table(summary, 'Выданное по событиям')}
    </section>
  {/if}
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
  .sel {
    background: var(--selected);
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
  .detail h3 {
    margin: 0 0 4px;
    font-size: 14px;
  }
  .share {
    margin: 6px 0 0;
  }
  .small {
    font-size: 12px;
  }
</style>
