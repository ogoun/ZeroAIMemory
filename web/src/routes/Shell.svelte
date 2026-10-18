<script lang="ts">
  // Оболочка после входа: шапка, дерево слева (на узком экране — выдвижная панель),
  // справа — документ, стартовый экран пользователя или администрирование.
  import { onMount } from 'svelte';
  import DocView from '../components/DocView.svelte';
  import Search from '../components/Search.svelte';
  import Tree, { type Reveal, type TreeChange } from '../components/Tree.svelte';
  import Users from './Users.svelte';
  import { ApiError, listUsers, type Me, type NodeInfo, type User } from '../lib/api';
  import { current, go, href, listen, type Route } from '../lib/router';

  let { me, onlogout, onexpired }: { me: Me; onlogout: () => void; onexpired: () => void } = $props();

  let route = $state<Route>(current());
  let users = $state<User[]>([]);
  let usersError = $state('');
  let drawer = $state(false);
  let opened = $state<{ user: string; node: NodeInfo } | null>(null);
  let reveal = $state<Reveal | null>(null);
  let reloadKey = $state(0);
  /** Узкий экран: дерево — выдвижная панель (скрытая недоступна с клавиатуры). */
  let narrow = $state(false);
  /** Счётчик событий маршрута (повторный переход к тому же якорю тоже прокручивает). */
  let nav = $state(0);
  let searchOpen = $state(false);

  async function loadUsers() {
    usersError = '';
    try {
      const all = await listUsers();
      // свой пользователь — первым, остальные по имени
      users = [
        ...all.filter((u) => u.id === me.user_id),
        ...all.filter((u) => u.id !== me.user_id).sort((a, b) => a.name.localeCompare(b.name)),
      ];
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      usersError = e instanceof Error ? e.message : 'Ошибка';
    }
  }

  function normalize(r: Route) {
    // пустой адрес — свой пользователь
    if (r.name === 'home' && !r.user) {
      go({ name: 'home', user: me.user_id }, { replace: true });
      return false;
    }
    return true;
  }

  onMount(() => {
    const mq = window.matchMedia('(max-width: 719px)');
    narrow = mq.matches;
    const onmq = (e: MediaQueryListEvent) => (narrow = e.matches);
    mq.addEventListener('change', onmq);
    // пустой адрес заменяется адресом своего пользователя; маршрут — всегда по итогу
    normalize(current());
    route = current();
    loadUsers();
    const off = listen((r) => {
      if (!normalize(r)) return;
      const wasAdmin = route.name === 'admin';
      route = r;
      nav++;
      drawer = false;
      // после администрирования список пользователей мог измениться
      if (wasAdmin && r.name !== 'admin') loadUsers();
    });
    return () => {
      off();
      mq.removeEventListener('change', onmq);
    };
  });

  function onloaded(n: NodeInfo) {
    if (route.name !== 'node') return;
    opened = { user: route.user, node: n };
    reveal = { user: route.user, space: route.space, path: n.path };
  }

  function onchange(c: TreeChange) {
    const o = opened;
    if (!o || o.user !== c.user || route.name !== 'node') return;
    const inside =
      o.node.id === c.node.id ||
      (o.node.space_id === c.node.space_id && o.node.path.startsWith(`${c.node.path}/`));
    if (!inside) return;
    if (c.kind === 'removed') {
      opened = null;
      go({ name: 'home', user: c.user });
    } else {
      reloadKey++;
    }
  }

  /** Пользователь, в заметках которого ищет Ctrl+K: открытый, иначе свой. */
  let searchUser = $derived(route.name === 'admin' ? me.user_id : (route.user ?? me.user_id));

  function onkeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      searchOpen = true;
    }
  }

  let userName = $derived.by(() => {
    const r = route;
    return r.name === 'admin' ? '' : (users.find((u) => u.id === r.user)?.name ?? '');
  });
</script>

<svelte:window {onkeydown} />

<div class="shell" class:drawer-open={drawer}>
  <header class="topbar">
    <button
      type="button"
      class="drawer-toggle"
      aria-label={drawer ? 'Скрыть дерево' : 'Показать дерево'}
      aria-expanded={drawer}
      aria-controls="sidebar"
      onclick={() => (drawer = !drawer)}
    >
      <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 5h14M3 10h14M3 15h14" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" /></svg>
    </button>
    <a class="brand" href={href({ name: 'home', user: me.user_id })}>ZeroAIMemory</a>
    <nav class="topnav" aria-label="Разделы">
      <a href={href({ name: 'home', user: me.user_id })} aria-current={route.name !== 'admin' ? 'page' : undefined}>Заметки</a>
      <a href={href({ name: 'admin' })} aria-current={route.name === 'admin' ? 'page' : undefined}>Администрирование</a>
    </nav>
    <button type="button" class="search-btn" onclick={() => (searchOpen = true)} title="Поиск (Ctrl+K)" aria-label="Поиск">
      <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" fill="none" stroke="currentColor" stroke-width="1.8" /><path d="M13 13l4 4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" /></svg>
      <span class="search-label">Поиск</span><kbd>Ctrl K</kbd>
    </button>
    <span class="who">
      <span class="muted login">{me.login}</span>
      <button type="button" onclick={onlogout}>Выйти</button>
    </span>
  </header>

  <aside id="sidebar" class="sidebar" inert={narrow && !drawer}>
    {#if usersError}<p class="error" role="alert">{usersError}</p>{/if}
    {#if users.length}
      <Tree {users} ownId={me.user_id} {route} {reveal} {onchange} {onexpired} />
    {/if}
  </aside>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" onclick={() => (drawer = false)}></div>

  <main class="content">
    {#if route.name === 'admin'}
      <div class="admin">
        <Users onExpired={onexpired} />
      </div>
    {:else if route.name === 'node'}
      <DocView
        user={route.user}
        space={route.space}
        node={route.node}
        anchor={route.anchor}
        {reloadKey}
        {nav}
        {onloaded}
        {onexpired}
      />
    {:else}
      <section class="welcome">
        <h1>{userName || 'Заметки'}</h1>
        <p class="muted">
          Выберите заметку в дереве. Правый клик или кнопка «⋯» у пространства и папки — создание
          заметок и папок, у заметки — перенос и удаление.
        </p>
      </section>
    {/if}
  </main>
</div>

<Search bind:open={searchOpen} user={searchUser} {onexpired} />

<style>
  .shell {
    min-height: 100vh;
    display: grid;
    grid-template-columns: 300px minmax(0, 1fr);
    grid-template-rows: var(--header-h) 1fr;
    grid-template-areas:
      'top top'
      'side main';
  }
  .topbar {
    grid-area: top;
    position: sticky;
    top: 0;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 16px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .brand {
    font-weight: 650;
    color: var(--text);
    text-decoration: none;
  }
  .topnav {
    display: flex;
    gap: 4px;
  }
  .topnav a {
    padding: 6px 10px;
    border-radius: 6px;
    color: var(--muted);
    text-decoration: none;
  }
  .topnav a:hover {
    background: var(--hover);
  }
  .topnav a[aria-current='page'] {
    color: var(--text);
    background: var(--selected);
  }
  .search-btn {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    color: var(--muted);
  }
  .search-btn svg {
    width: 16px;
    height: 16px;
  }
  .search-btn kbd {
    font: 11px ui-monospace, Consolas, monospace;
    padding: 1px 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .drawer-toggle {
    display: none;
    padding: 6px;
    border: 0;
    background: transparent;
  }
  .drawer-toggle svg {
    display: block;
    width: 20px;
    height: 20px;
  }
  .sidebar {
    grid-area: side;
    position: sticky;
    top: var(--header-h);
    height: calc(100vh - var(--header-h));
    overflow: auto;
    padding: 12px 8px 24px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .scrim {
    display: none;
  }
  .content {
    grid-area: main;
    min-width: 0;
    padding: 24px 32px 64px;
  }
  .admin {
    max-width: 880px;
    margin: 0 auto;
  }
  .welcome {
    max-width: 640px;
    margin: 10vh auto 0;
    text-align: center;
  }

  @media (max-width: 719px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-areas:
        'top'
        'main';
    }
    .topbar {
      gap: 8px;
      padding: 0 8px;
    }
    .drawer-toggle {
      display: block;
    }
    .topnav a {
      padding: 6px;
      font-size: 14px;
    }
    .login,
    .search-btn kbd,
    .search-label {
      display: none;
    }
    .sidebar {
      position: fixed;
      top: var(--header-h);
      left: 0;
      bottom: 0;
      z-index: 25;
      width: min(320px, 86vw);
      height: auto;
      transform: translateX(-100%);
      transition: transform 0.18s ease-out;
      box-shadow: none;
    }
    .drawer-open .sidebar {
      transform: none;
      box-shadow: 0 0 32px rgb(0 0 0 / 0.25);
    }
    .drawer-open .scrim {
      display: block;
      position: fixed;
      inset: var(--header-h) 0 0 0;
      z-index: 24;
      background: rgb(0 0 0 / 0.35);
    }
    .content {
      padding: 16px 14px 48px;
    }
  }
  @media (max-width: 420px) {
    .brand {
      display: none;
    }
  }
</style>
