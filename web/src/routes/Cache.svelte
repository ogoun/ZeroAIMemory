<script lang="ts">
  // «Кэш выжимок» (§9.4, §15.4): выжимки агентов личной базы или решения с фильтрами
  // (решение, `valid`/`stale`, автор, запрос), просмотр выжимки и её источников (у `stale` —
  // причины), удаление и «Превратить в заметку» (`POST /cache/{id}/promote`). Администратор
  // выбирает пользователя (`X-Zam-User`), пользователь видит свою базу и свои решения.
  import { onMount } from 'svelte';
  import Prompt, { type Ask } from '../components/Prompt.svelte';
  import {
    ApiError,
    deleteCache,
    errorText,
    getCache,
    isAdminSession,
    listCache,
    listSolutions,
    promoteCache,
    type CacheItem,
    type Me,
    type SolutionItem,
    type User,
  } from '../lib/api';
  import { fmtTime } from '../lib/journals';
  import { href } from '../lib/router';

  let { me, users, onexpired }: { me: Me; users: User[]; onexpired: () => void } = $props();

  const LIMIT = 200;
  const STALE_TEXT: Record<string, string> = {
    changed: 'источник изменился',
    deleted: 'источник удалён',
    superseded: 'insight заменён',
    retracted: 'insight отозван',
    new_sources: 'появились новые подходящие источники',
  };

  let admin = $derived(isAdminSession(me));
  let userId = $state('');
  let solution = $state('');
  let stateF = $state<'' | 'valid' | 'stale'>('');
  let author = $state('');
  let q = $state('');

  let items = $state<CacheItem[]>([]);
  let loaded = $state(false);
  let error = $state('');
  let notice = $state('');
  let solutions = $state<SolutionItem[]>([]);
  /** Авторы из выдачи без фильтра автора (у пользователя списка пользователей нет). */
  let authors = $state<Map<string, string>>(new Map());
  let open = $state<CacheItem | null>(null);
  let ask = $state<Ask | null>(null);

  let who = $derived(userId || me.user_id);
  const asUser = () => (admin ? who : undefined);

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = errorText(e);
  }

  let seq = 0;
  async function load() {
    const n = ++seq;
    error = '';
    try {
      const r = await listCache(
        { q: q.trim() || undefined, solution: solution || undefined, state: stateF || undefined, author: author || undefined, limit: LIMIT },
        asUser(),
      );
      if (n !== seq) return;
      items = r.items;
      if (!author) {
        const m = new Map(authors);
        for (const it of r.items) m.set(it.author_user_id, it.author ?? it.author_user_id);
        authors = m;
      }
      loaded = true;
    } catch (e) {
      if (n === seq) fail(e);
    }
  }

  $effect(() => {
    void [userId, solution, stateF, author, q];
    const t = setTimeout(load, 150);
    return () => clearTimeout(t);
  });

  /** Варианты фильтра автора: администратору — пользователи, пользователю — авторы выдачи. */
  let authorOptions = $derived.by(() => {
    if (admin) return users.map((u) => ({ id: u.id, name: u.name }));
    return [...authors].map(([id, name]) => ({ id, name }));
  });

  function pickUser(e: Event) {
    userId = (e.currentTarget as HTMLSelectElement).value;
    solution = '';
    open = null;
    authors = new Map();
  }

  async function show(it: CacheItem) {
    error = '';
    notice = '';
    try {
      open = await getCache(it.id, asUser());
    } catch (e) {
      fail(e);
    }
  }

  function remove(it: CacheItem) {
    notice = '';
    ask = {
      title: 'Удалить выжимку',
      message: `«${it.query}» — выжимка удаляется мягко: поиск и агенты её больше не получат.`,
      confirm: 'Удалить',
      danger: true,
      run: async () => {
        try {
          await deleteCache(it.id, asUser());
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onexpired();
          throw new Error(errorText(e));
        }
        if (open?.id === it.id) open = null;
        notice = 'Выжимка удалена.';
        await load();
      },
    };
  }

  function promote(it: CacheItem) {
    notice = '';
    const name = it.query.replace(/[/\\\p{Cc}]/gu, ' ').slice(0, 80).trim();
    ask = {
      title: 'Превратить в заметку',
      message:
        'Заметка создаётся в пространстве выжимки со связями derived_from на источники; выжимка остаётся в кэше.',
      label: 'Путь заметки',
      value: `Выжимки/${name}`,
      confirm: 'Создать заметку',
      run: async (path) => {
        try {
          open = await promoteCache(it.id, { as: 'note', path }, asUser());
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onexpired();
          throw new Error(errorText(e));
        }
        notice = `Заметка «${path}» создана.`;
        await load();
      },
    };
  }

  /** Ссылка на заметку `n:<id>` в пространстве выжимки. */
  function noteHref(it: CacheItem, ref: string | null): string | null {
    if (!ref?.startsWith('n:')) return null;
    return href({ name: 'node', user: who, space: it.agent_solution_id ?? it.space_id, node: ref.slice(2), anchor: null });
  }

  onMount(async () => {
    try {
      solutions = await listSolutions();
    } catch (e) {
      fail(e);
    }
  });

  let userSolutions = $derived(admin ? solutions.filter((s) => s.owner.id === who) : solutions);
</script>

<section class="panel">
  <h2>Кэш выжимок</h2>
  <p class="muted small">
    Выжимки, которые агенты сохранили после поиска (§9.3). Действующая (<code>valid</code>) отдаётся
    поиском первой; <code>stale</code> — источник изменился, удалён или появились новые.
  </p>
  <div class="row filters" role="group" aria-label="Фильтры кэша">
    {#if admin}
      <select value={who} onchange={pickUser} aria-label="Пользователь">
        {#each users as u (u.id)}<option value={u.id}>{u.name}{u.id === me.user_id ? ' (вы)' : ''}</option>{/each}
      </select>
    {/if}
    <select bind:value={solution} aria-label="Решение">
      <option value="">Личная база (Knowledge)</option>
      {#each userSolutions as s (s.agent_solution_id)}<option value={s.agent_solution_id}>{s.name}</option>{/each}
    </select>
    <select bind:value={stateF} aria-label="Статус">
      <option value="">все</option>
      <option value="valid">valid</option>
      <option value="stale">stale</option>
    </select>
    <select bind:value={author} aria-label="Автор">
      <option value="">любой автор</option>
      {#each authorOptions as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
    </select>
    <input type="search" placeholder="Запрос содержит…" bind:value={q} aria-label="Запрос" />
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="muted" role="status">{notice}</p>{/if}

  {#if loaded && !items.length}
    <p class="muted">Выжимок нет.</p>
  {:else if items.length}
    <table aria-label="Выжимки">
      <thead><tr><th>Запрос</th><th>Статус</th><th>Автор</th><th>Обращений</th><th>Последнее</th><th></th></tr></thead>
      <tbody>
        {#each items as it (it.id)}
          <tr data-cache={it.query} class:sel={open?.id === it.id}>
            <td><button type="button" class="link" onclick={() => show(it)}>{it.query}</button></td>
            <td><span class="st st-{it.state}">{it.state}</span></td>
            <td>{it.author ?? '—'}</td>
            <td>{it.hits}</td>
            <td class="muted">{fmtTime(it.last_used_at)}</td>
            <td class="row acts">
              <button type="button" onclick={() => promote(it)} disabled={!!it.promoted_to}
                title={it.promoted_to ? `уже превращена в ${it.promoted_to}` : undefined}>Превратить в заметку</button>
              <button type="button" class="danger" onclick={() => remove(it)}>Удалить</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

{#if open}
  {@const it = open}
  {@const promoted = noteHref(it, it.promoted_to)}
  <section class="panel detail" style="margin-top: 16px;" aria-label="Выжимка">
    <header class="row">
      <h2>{it.query}</h2>
      <span class="st st-{it.state}">{it.state}</span>
      <button type="button" class="close" onclick={() => (open = null)}>Закрыть</button>
    </header>
    <p class="muted small">
      {it.space ?? ''} · автор {it.author ?? '—'}{it.machine ? `, машина ${it.machine}` : ''} · {it.tokens} токенов ·
      создана {fmtTime(it.created_at)} · обращений {it.hits} · <code>{it.ref}</code>
    </p>
    <div class="summary">{it.summary}</div>
    {#if it.state === 'stale'}
      <div class="warn" role="note">
        <b>Выжимка устарела:</b>
        <ul>
          {#each it.stale as s, i (i)}
            <li><code>{s.target}</code>{s.section ? ` § ${s.section}` : ''} — {STALE_TEXT[s.reason] ?? s.reason}</li>
          {/each}
        </ul>
      </div>
    {/if}
    <h3>Источники</h3>
    <ul class="sources">
      {#each it.sources as s, i (i)}
        <li><code>{s.id}</code> · ревизия {s.rev}{s.section ? ` · § ${s.section}` : ''}</li>
      {/each}
    </ul>
    {#if it.promoted_to}
      <p class="small">
        Превращена: {#if promoted}<a href={promoted}>{it.promoted_to}</a>{:else}<code>{it.promoted_to}</code>{/if}
      </p>
    {/if}
  </section>
{/if}

<Prompt bind:ask />

<style>
  .filters {
    margin: 8px 0 12px;
  }
  .filters input {
    flex: 1 1 180px;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    text-align: left;
    cursor: pointer;
  }
  .sel {
    background: var(--selected);
  }
  .st {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .st-stale {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .acts {
    gap: 4px;
  }
  .acts button {
    padding: 2px 8px;
  }
  .detail h2 {
    margin: 0;
  }
  .close {
    margin-left: auto;
  }
  .detail h3 {
    margin: 12px 0 4px;
    font-size: 14px;
  }
  .summary {
    white-space: pre-wrap;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .warn ul,
  .sources {
    margin: 4px 0 0;
    padding-left: 20px;
  }
  .small {
    font-size: 12px;
  }
</style>
