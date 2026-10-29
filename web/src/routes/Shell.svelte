<script lang="ts">
  // Оболочка после входа: шапка, дерево слева (на узком экране — выдвижная панель),
  // справа — документ, стартовый экран пользователя или администрирование. У пользователя
  // (не администратора, §15.1) нет разделов «Пользователи», «Администрирование», «Настройки»
  // и полного «Индекса»: есть «Мои решения», «Аккаунт» и баннер перенарезки своей базы.
  // «Подключение агента» и «Кэш выжимок» — у обоих (страницы грузятся лениво, §15.4).
  import { onMount, untrack } from 'svelte';
  import DocView from '../components/DocView.svelte';
  import Search from '../components/Search.svelte';
  import Tree, { type Reveal, type TreeChange } from '../components/Tree.svelte';
  import ChangelogView from '../components/ChangelogView.svelte';
  import InsightsView from '../components/InsightsView.svelte';
  import GlossaryView from '../components/GlossaryView.svelte';
  import TasksView from '../components/TasksView.svelte';
  import QuestionsView from '../components/QuestionsView.svelte';
  import ReleasesView from '../components/ReleasesView.svelte';
  import VaultView from '../components/VaultView.svelte';
  import RechunkBanner from '../components/RechunkBanner.svelte';
  import Account from './Account.svelte';
  import IndexStatus from './IndexStatus.svelte';
  import Settings from './Settings.svelte';
  import Solutions from './Solutions.svelte';
  import Users from './Users.svelte';
  import {
    ApiError,
    errorText,
    isAdminSession,
    listSpaces,
    listUsers,
    spaceMatches,
    type Me,
    type NodeInfo,
    type Space,
    type User,
  } from '../lib/api';
  import { JOURNAL_TITLE, journalsOf } from '../lib/journals';
  import { current, go, href, listen, routeUser, type Route } from '../lib/router';

  let { me, onlogout, onexpired }: { me: Me; onlogout: () => void; onexpired: () => void } = $props();

  /** Сессия администратора: разделы управления сервисом и все пользователи. */
  let admin = $derived(isAdminSession(me));

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
    if (!admin) {
      // пользователь видит только себя: список пользователей — администраторский маршрут
      users = [
        {
          id: me.user_id,
          name: me.name,
          is_admin: false,
          created_at: '',
          disabled_at: null,
          tokens: 0,
          has_password: true,
          must_change_password: false,
        },
      ];
      return;
    }
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
    // пустой адрес — свой пользователь; разделы администратора пользователю недоступны
    if ((r.name === 'home' && !r.user) || (!admin && (isAdmin(r) || r.name === 'index'))) {
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
      const wasAdmin = isAdmin(route);
      route = r;
      nav++;
      drawer = false;
      // после администрирования список пользователей мог измениться
      if (wasAdmin && !isAdmin(r)) loadUsers();
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

  /** Разделы администрирования (подменю «Пользователи», «Решения», «Настройки»). */
  const isAdmin = (r: Route) => r.name === 'admin' || r.name === 'solutions' || r.name === 'settings';

  // Пространство открытого журнала: имя и вид — для заголовка и вкладок журналов.
  let journalSpaces = $state<{ user: string; spaces: Space[] } | null>(null);
  let journalError = $state('');
  $effect(() => {
    const r = route;
    // вне журналов кэш сбрасывается: роли и решения могли измениться
    if (r.name !== 'journal') {
      if (untrack(() => journalSpaces) !== null) journalSpaces = null;
      return;
    }
    if (untrack(() => journalSpaces?.user) === r.user) return;
    const u = r.user;
    journalError = '';
    listSpaces(u)
      .then((x) => (journalSpaces = { user: u, spaces: x.spaces }))
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) return onexpired();
        journalError = errorText(e);
      });
  });
  /** Пространства пользователя журнала загружены. */
  let journalLoaded = $derived(route.name === 'journal' && journalSpaces?.user === route.user);
  let journalSpace = $derived.by(() => {
    const r = route;
    if (r.name !== 'journal' || journalSpaces?.user !== r.user) return null;
    return journalSpaces.spaces.find((s) => spaceMatches(s, r.space)) ?? null;
  });

  /** Пользователь, в заметках которого ищет Ctrl+K: открытый, иначе свой. */
  let searchUser = $derived(routeUser(route) ?? me.user_id);

  function onkeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      searchOpen = true;
    }
  }

  /** Имена пользователей для баннера перенарезки администратора. */
  let userNames = $derived(Object.fromEntries(users.map((u) => [u.id, u.name])));

  let userName = $derived.by(() => {
    const r = route;
    const u = routeUser(r);
    return u ? (users.find((x) => x.id === u)?.name ?? '') : '';
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
      <a href={href({ name: 'home', user: me.user_id })} aria-current={routeUser(route) ? 'page' : undefined}>Заметки</a>
      {#if admin}
        <a href={href({ name: 'index' })} aria-current={route.name === 'index' ? 'page' : undefined}>Индекс</a>
        <!-- на узком экране — короткая подпись (имя ссылки для доступности — полное) -->
        <a href={href({ name: 'admin' })} aria-current={isAdmin(route) ? 'page' : undefined} aria-label="Администрирование"
          ><span class="long">Администрирование</span><span class="short" aria-hidden="true">Админ</span></a
        >
      {:else}
        <a href={href({ name: 'mysolutions' })} aria-current={route.name === 'mysolutions' ? 'page' : undefined}
          aria-label="Мои решения"><span class="long">Мои решения</span><span class="short" aria-hidden="true">Решения</span></a
        >
        <a href={href({ name: 'account' })} aria-current={route.name === 'account' ? 'page' : undefined}>Аккаунт</a>
      {/if}
      <a href={href({ name: 'agentsetup' })} aria-current={route.name === 'agentsetup' ? 'page' : undefined}
        aria-label="Подключение агента"><span class="long">Подключение агента</span><span class="short" aria-hidden="true">Агент</span></a
      >
      <a href={href({ name: 'cache' })} aria-current={route.name === 'cache' ? 'page' : undefined}
        aria-label="Кэш выжимок"><span class="long">Кэш выжимок</span><span class="short" aria-hidden="true">Кэш</span></a
      >
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
      <Tree {users} ownId={me.user_id} heading={admin ? 'Пользователи' : 'Моя память'} {route} {reveal} {onchange} {onexpired} />
    {/if}
  </aside>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" onclick={() => (drawer = false)}></div>

  <main class="content">
    <!-- перенарезка: у пользователя — своя база, у администратора — все (на «Индексе» — его статус) -->
    {#if !admin}
      <RechunkBanner userId={me.user_id} {onexpired} />
    {:else if route.name !== 'index'}
      <RechunkBanner names={userNames} {onexpired} />
    {/if}
    {#if isAdmin(route) && admin}
      <div class="admin">
        <nav class="subnav" aria-label="Администрирование">
          <a href={href({ name: 'admin' })} aria-current={route.name === 'admin' ? 'page' : undefined}>Пользователи</a>
          <a href={href({ name: 'solutions' })} aria-current={route.name === 'solutions' ? 'page' : undefined}>Решения</a>
          <a href={href({ name: 'settings' })} aria-current={route.name === 'settings' ? 'page' : undefined}>Настройки</a>
        </nav>
        {#if route.name === 'solutions'}
          <Solutions {onexpired} />
        {:else if route.name === 'settings'}
          <Settings {onexpired} />
        {:else}
          <Users onExpired={onexpired} />
        {/if}
      </div>
    {:else if route.name === 'journal'}
      {@const r = route}
      <div class="journal-page">
        <header class="journal-head">
          <div class="crumbs muted">
            {userName ? `${userName} / ` : ''}{journalSpace?.name ?? r.space}
            {#if journalSpace?.role === 'member'}
              <span class="tag">участник {journalSpace.access} · владелец {journalSpace.owner?.name ?? '?'}</span>
            {/if}
          </div>
          <h1>{JOURNAL_TITLE[r.journal]}</h1>
          <nav class="subnav" aria-label="Журналы пространства">
            {#each journalsOf(journalSpace?.kind ?? '') as j (j)}
              <a
                href={href({ name: 'journal', user: r.user, space: r.space, journal: j, entry: null })}
                aria-current={r.journal === j ? 'page' : undefined}>{JOURNAL_TITLE[j]}</a
              >
            {/each}
          </nav>
        </header>
        {#if journalError}
          <p class="error" role="alert">{journalError}</p>
        {:else if !journalLoaded}
          <p class="muted">Загрузка…</p>
        {:else if !journalSpace}
          <p class="error" role="alert">Пространство «{r.space}» не найдено или недоступно.</p>
        {:else if !journalsOf(journalSpace.kind).includes(r.journal)}
          <p class="warn" role="alert">
            У пространства «{journalSpace.name}» нет журнала {JOURNAL_TITLE[r.journal]}: журналы ведутся в
            решениях (Changelog, Releases, Insights, Словарь, Задачи, Вопросы, Vault) и в Knowledge
            (Insights, Словарь, Задачи, Вопросы, Vault).
          </p>
        {:else if r.journal === 'changelog'}
          <ChangelogView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'releases'}
          <ReleasesView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'insights'}
          <InsightsView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'glossary'}
          <GlossaryView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'tasks'}
          <TasksView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'questions'}
          <QuestionsView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else}
          <VaultView user={r.user} space={r.space} {onexpired} />
        {/if}
      </div>
    {:else if route.name === 'index' && admin}
      <div class="admin">
        <IndexStatus {users} {onexpired} />
      </div>
    {:else if route.name === 'account'}
      <div class="admin">
        <Account {me} {onexpired} />
      </div>
    {:else if route.name === 'agentsetup'}
      <div class="admin">
        {#await import('./AgentSetup.svelte')}
          <p class="muted">Загрузка…</p>
        {:then { default: AgentSetup }}
          <AgentSetup {me} {users} {onexpired} />
        {:catch e}
          <p class="error" role="alert">Страница не загрузилась: {errorText(e)}</p>
        {/await}
      </div>
    {:else if route.name === 'cache'}
      <div class="admin">
        {#await import('./Cache.svelte')}
          <p class="muted">Загрузка…</p>
        {:then { default: Cache }}
          <Cache {me} {users} {onexpired} />
        {:catch e}
          <p class="error" role="alert">Страница не загрузилась: {errorText(e)}</p>
        {/await}
      </div>
    {:else if route.name === 'mysolutions'}
      <div class="admin">
        <Solutions mine {onexpired} />
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
    /* пунктов больше, чем влезает на узком экране: меню прокручивается само, страница — нет */
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .topnav a {
    flex: none;
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
  .topnav .short {
    display: none;
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
  .journal-page {
    max-width: 1100px;
    margin: 0 auto;
  }
  .journal-head {
    margin-bottom: 16px;
  }
  .journal-head h1 {
    margin: 2px 0 8px;
  }
  .crumbs {
    font-size: 13px;
  }
  .tag {
    margin-left: 6px;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .subnav {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    margin-bottom: 16px;
    border-bottom: 1px solid var(--border);
  }
  .subnav a {
    padding: 6px 12px;
    margin-bottom: -1px;
    color: var(--muted);
    text-decoration: none;
    border-bottom: 2px solid transparent;
  }
  .subnav a[aria-current='page'] {
    color: var(--text);
    border-bottom-color: var(--accent);
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
    .topnav .long {
      display: none;
    }
    .topnav .short {
      display: inline;
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
