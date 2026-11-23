<script lang="ts">
  // Оболочка после входа: шапка, дерево слева, справа — документ, стартовый экран пользователя
  // или администрирование. У пользователя (не администратора, §15.1) нет разделов
  // «Пользователи», «Администрирование», «Настройки» и полного «Индекса»: есть «Мои решения»,
  // «Аккаунт» и баннер перенарезки своей базы. «Администрирование» с M7.2 (§5.6) — ещё «Копии» и
  // «Аудит» (ленивые чанки) и плашка `no_recent_backup` над его страницами. «Подключение
  // агента», «Кэш выжимок» и «Сессии» (M6 §8) — у обоих (страницы грузятся лениво, §15.4; сбой
  // загрузки — `LazyFail`).
  // Классы ширины (M7.2, §5.1, `layout.ts`): телефон — ☰, заголовок раздела, поиск и меню
  // аккаунта в шапке, разделы и дерево — в выдвижной панели; планшет — разделы в шапке короткими
  // подписями, дерево — в панели; настольный — дерево постоянное (260 px до 1279, 300 px дальше).
  import { onMount, tick, untrack } from 'svelte';
  import DocView from '../components/DocView.svelte';
  import AccountMenu from '../components/AccountMenu.svelte';
  import LazyFail from '../components/LazyFail.svelte';
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
  import BackupBanner from '../components/BackupBanner.svelte';
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
    spaceRef,
    type Me,
    type NodeInfo,
    type Space,
    type User,
  } from '../lib/api';
  import { countDescription, countHint, formatCount, openLabel } from '../lib/counts';
  import { counts } from '../lib/counts.svelte';
  import { JOURNAL_TITLE, journalsOf } from '../lib/journals';
  import { current, go, href, listen, routeUser, type Route } from '../lib/router';
  import { revealSelected, viewport } from '../lib/viewport.svelte';

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
  /** Телефон и планшет: дерево — выдвижная панель (скрытая недоступна с клавиатуры). */
  let narrow = $derived(viewport.current !== 'desktop');
  /** Телефон: разделы — в панели, логин и «Выйти» — в меню аккаунта. */
  let phone = $derived(viewport.current === 'phone');
  /** Счётчик событий маршрута (повторный переход к тому же якорю тоже прокручивает). */
  let nav = $state(0);
  let searchOpen = $state(false);
  let toggleEl = $state<HTMLButtonElement | null>(null);
  let sidebarEl = $state<HTMLElement | null>(null);

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
    // пустой адрес заменяется адресом своего пользователя; маршрут — всегда по итогу
    normalize(current());
    route = current();
    loadUsers();
    return listen((r) => {
      if (!normalize(r)) return;
      const wasAdmin = isAdmin(route);
      route = r;
      nav++;
      drawer = false;
      // после администрирования список пользователей мог измениться
      if (wasAdmin && !isAdmin(r)) loadUsers();
    });
  });

  const FOCUSABLE = 'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])';

  // Панель открылась — фокус в неё (первый пункт); закрылась (☰, фон, Esc, переход) — на ☰, если
  // фокус был в панели или потерян.
  let drawerShown = false;
  $effect(() => {
    const d = drawer;
    untrack(() => {
      if (d === drawerShown) return;
      drawerShown = d;
      if (d) {
        tick().then(() => sidebarEl?.querySelector<HTMLElement>(FOCUSABLE)?.focus({ preventScroll: true }));
      } else {
        const a = document.activeElement;
        if (!a || a === document.body || sidebarEl?.contains(a)) toggleEl?.focus({ preventScroll: true });
      }
    });
  });

  // Смена класса ширины (поворот, изменение окна) закрывает панель: на настольном её нет, а
  // «Разделы» уходят из панели в шапку (или обратно) — фокус с исчезнувшей ссылки переходит на ☰
  // (эффект выше). Меню аккаунта есть только на телефоне и закрывается само вместе с ним.
  let widthShown = viewport.current;
  $effect(() => {
    const c = viewport.current;
    untrack(() => {
      if (c === widthShown) return;
      widthShown = c;
      drawer = false;
    });
  });

  /** Щелчок по ссылке панели закрывает её, даже если адрес не меняется (текущий пункт). */
  function onsidebarclick(e: MouseEvent) {
    if (narrow && drawer && e.target instanceof Element && e.target.closest('a[href]')) drawer = false;
  }

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

  /** Разделы администрирования (подменю «Пользователи», «Решения», «Настройки», «Копии», «Аудит»). */
  const isAdmin = (r: Route) =>
    r.name === 'admin' || r.name === 'solutions' || r.name === 'settings' || r.name === 'backups' || r.name === 'audit';

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
  /** Открытый журнал «пользователь | пространство | журнал» (без записи: карточка числа не меняет). */
  let journalKey = $derived(
    route.name === 'journal' && journalSpace ? `${route.user}|${spaceRef(journalSpace)}|${route.journal}` : '',
  );
  // открытие журнала перечитывает числа полосы (§5.3 п. 3): запись агента видна без перезагрузки
  $effect(() => {
    const [u, ref] = journalKey.split('|');
    if (ref) untrack(() => counts.invalidate(u, ref));
  });
  /** Числа разделов пространства открытого журнала (ключ — как в дереве: `spaceRef`). */
  let journalCounts = $derived(
    route.name === 'journal' && journalSpace ? counts.get(route.user, spaceRef(journalSpace)) : null,
  );
  /** Измеренная рамка числа (и «открыто N») каждого журнала полосы: ссылка оставляет под неё поле. */
  let stripOver = $state<Record<string, DOMRectReadOnly | undefined>>({});

  /** Пользователь, в заметках которого ищет Ctrl+K: открытый, иначе свой. */
  let searchUser = $derived(routeUser(route) ?? me.user_id);

  function onkeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      searchOpen = true;
      return;
    }
    if (e.key !== 'Escape' || e.defaultPrevented || searchOpen) return;
    // Esc окна и меню (`role="dialog"`, `role="menu"`; меню аккаунта отмечает свой Esc
    // обработанным) закрывает их, а не панель
    if (e.target instanceof Element && e.target.closest('[role="dialog"], [role="menu"]')) return;
    // Esc закрыл панель — обработан: слой ниже (панель карточки доски, JournalSplit) его не берёт
    if (drawer) {
      drawer = false;
      e.preventDefault();
    }
  }

  /** Заголовок текущего раздела — в шапке телефона (разделы там — в панели). */
  let sectionTitle = $derived.by(() => {
    const r = route;
    if (routeUser(r)) return 'Заметки';
    if (isAdmin(r)) return 'Администрирование';
    const titles: Partial<Record<Route['name'], string>> = {
      index: 'Индекс',
      mysolutions: 'Мои решения',
      account: 'Аккаунт',
      agentsetup: 'Подключение агента',
      cache: 'Кэш выжимок',
      sessions: 'Сессии',
    };
    return titles[r.name] ?? 'Заметки';
  });

  /** Имена пользователей для баннера перенарезки администратора. */
  let userNames = $derived(Object.fromEntries(users.map((u) => [u.id, u.name])));

  let userName = $derived.by(() => {
    const r = route;
    const u = routeUser(r);
    return u ? (users.find((x) => x.id === u)?.name ?? '') : '';
  });
</script>

<svelte:window {onkeydown} />

<!-- Разделы: в шапке (на планшете — короткие подписи) или строками в панели телефона -->
{#snippet sections(rows: boolean)}
  <nav class="topnav" class:rows aria-label="Разделы">
    <a href={href({ name: 'home', user: me.user_id })} aria-current={routeUser(route) ? 'page' : undefined}>Заметки</a>
    {#if admin}
      <a href={href({ name: 'index' })} aria-current={route.name === 'index' ? 'page' : undefined}>Индекс</a>
      <!-- на планшете — короткая подпись (имя ссылки для доступности — полное) -->
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
    <a href={href({ name: 'sessions' })} aria-current={route.name === 'sessions' ? 'page' : undefined}>Сессии</a>
  </nav>
{/snippet}

<div class="shell" class:drawer-open={drawer}>
  <header class="topbar">
    <button
      type="button"
      class="drawer-toggle"
      bind:this={toggleEl}
      aria-label={drawer ? 'Скрыть дерево' : 'Показать дерево'}
      aria-expanded={drawer}
      aria-controls="sidebar"
      onclick={() => (drawer = !drawer)}
    >
      <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 5h14M3 10h14M3 15h14" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" /></svg>
    </button>
    <a class="brand" href={href({ name: 'home', user: me.user_id })}>ZeroAIMemory</a>
    {#if phone}
      <span class="section-title">{sectionTitle}</span>
    {:else}
      {@render sections(false)}
    {/if}
    <button type="button" class="search-btn" onclick={() => (searchOpen = true)} title="Поиск (Ctrl+K)" aria-label="Поиск">
      <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" fill="none" stroke="currentColor" stroke-width="1.8" /><path d="M13 13l4 4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" /></svg>
      <span class="search-label">Поиск</span><kbd>Ctrl K</kbd>
    </button>
    {#if phone}
      <!-- меню аккаунта телефона: логин и «Выйти» (§5.1 п. 3) -->
      <AccountMenu login={me.login} {onlogout} closeKey={nav} />
    {:else}
      <span class="who">
        <span class="muted login">{me.login}</span>
        <button type="button" onclick={onlogout}>Выйти</button>
      </span>
    {/if}
  </header>

  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
  <aside id="sidebar" class="sidebar" bind:this={sidebarEl} inert={narrow && !drawer} onclick={onsidebarclick}>
    {#if phone}
      <div class="drawer-heading" aria-hidden="true">Разделы</div>
      {@render sections(true)}
    {/if}
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
        <nav class="subnav strip" aria-label="Администрирование" use:revealSelected={route.name}>
          <a href={href({ name: 'admin' })} aria-current={route.name === 'admin' ? 'page' : undefined}>Пользователи</a>
          <a href={href({ name: 'solutions' })} aria-current={route.name === 'solutions' ? 'page' : undefined}>Решения</a>
          <a href={href({ name: 'settings' })} aria-current={route.name === 'settings' ? 'page' : undefined}>Настройки</a>
          <a href={href({ name: 'backups' })} aria-current={route.name === 'backups' ? 'page' : undefined}>Копии</a>
          <a href={href({ name: 'audit' })} aria-current={route.name === 'audit' ? 'page' : undefined}>Аудит</a>
        </nav>
        <!-- плашка `no_recent_backup` (§5.6 п. 3): на «Копиях» то же — текстом страницы -->
        {#if route.name !== 'backups'}<BackupBanner key={nav} {onexpired} />{/if}
        {#if route.name === 'solutions'}
          <Solutions {onexpired} />
        {:else if route.name === 'settings'}
          <Settings {onexpired} />
        {:else if route.name === 'backups'}
          {#await import('./Backups.svelte')}
            <p class="muted">Загрузка…</p>
          {:then { default: Backups }}
            <Backups {onexpired} />
          {:catch e}
            <LazyFail error={e} />
          {/await}
        {:else if route.name === 'audit'}
          {#await import('./Audit.svelte')}
            <p class="muted">Загрузка…</p>
          {:then { default: Audit }}
            <Audit {users} {onexpired} />
          {:catch e}
            <LazyFail error={e} />
          {/await}
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
          <!-- полоса журналов: одна строка с прокруткой на телефоне и планшете (§5.1 п. 4) -->
          <nav
            class="subnav strip"
            aria-label="Журналы пространства"
            use:revealSelected={`${r.journal}|${journalSpace?.kind ?? ''}`}
          >
            {#each journalsOf(journalSpace?.kind ?? '') as j (j)}
              {@const n = journalCounts?.journals[j]}
              {@const oq = j === 'questions' ? (journalCounts?.open.questions ?? 0) : 0}
              {@const ow = stripOver[j]?.width}
              {@const w = n === undefined ? undefined : ow ? `${ow}px` : `${formatCount(n).length}ch`}
              <!-- число и «открыто N» — отдельный элемент поверх правого поля ссылки (вне её имени и
                   текста, N3): место под него ссылка оставляет полем по его измеренной ширине, перенос
                   строки их не разделяет -->
              <a
                href={href({ name: 'journal', user: r.user, space: r.space, journal: j, entry: null })}
                class:counted={n !== undefined}
                style:--num-w={w}
                aria-describedby={n !== undefined ? `strip-count-${j}` : undefined}
                aria-current={r.journal === j ? 'page' : undefined}>{JOURNAL_TITLE[j]}</a
              >
              {#if n !== undefined}
                <span class="strip-over" style:--num-w={w} bind:contentRect={stripOver[j]} aria-hidden="true">
                  <span class="strip-num" class:zero={n === 0} data-count={j} title={countHint(j)}>{formatCount(n)}</span>
                  {#if oq > 0}<span class="tag open">{openLabel(oq)}</span>{/if}
                </span>
                <span id="strip-count-{j}" hidden>{countDescription(j, n, oq)}</span>
              {/if}
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
            решениях (Changelog, Releases, Отчёты, Код, Insights, Словарь, Задачи, Идеи, Вопросы, Vault) и в
            Knowledge (Insights, Словарь, Задачи, Идеи, Вопросы, Vault).
          </p>
        {:else if r.journal === 'changelog'}
          <ChangelogView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'releases'}
          <ReleasesView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'reports'}
          <!-- отчёты — ленивым чанком (страница, лента, корзина); удаляет только владелец решения -->
          {#await import('../components/ReportsView.svelte')}
            <p class="muted">Загрузка…</p>
          {:then { default: ReportsView }}
            <ReportsView
              user={r.user}
              space={r.space}
              project={journalSpace.name}
              entry={r.entry}
              owner={journalSpace.role === 'owner' && r.user === me.user_id}
              {onexpired}
            />
          {:catch e}
            <LazyFail error={e} />
          {/await}
        {:else if r.journal === 'code'}
          <!-- карта кода решения — ленивым чанком (M5.6); очищает только владелец решения -->
          {#await import('../components/CodeView.svelte')}
            <p class="muted">Загрузка…</p>
          {:then { default: CodeView }}
            <CodeView
              user={r.user}
              space={r.space}
              entry={r.entry}
              owner={journalSpace.role === 'owner' && r.user === me.user_id}
              {onexpired}
            />
          {:catch e}
            <LazyFail error={e} />
          {/await}
        {:else if r.journal === 'insights'}
          <InsightsView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'glossary'}
          <GlossaryView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'tasks'}
          <TasksView user={r.user} space={r.space} entry={r.entry} {onexpired} />
        {:else if r.journal === 'ideas'}
          <!-- идеи — ленивым чанком (M5.7); пространства — для выбора решения задачи идеи Knowledge -->
          {#await import('../components/IdeasView.svelte')}
            <p class="muted">Загрузка…</p>
          {:then { default: IdeasView }}
            <IdeasView user={r.user} space={r.space} entry={r.entry} spaces={journalSpaces?.spaces ?? []} viewer={me.user_id} {onexpired} />
          {:catch e}
            <LazyFail error={e} />
          {/await}
        {:else if r.journal === 'questions'}
          <QuestionsView user={r.user} space={r.space} entry={r.entry} spaces={journalSpaces?.spaces ?? []} {onexpired} />
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
          <LazyFail error={e} />
        {/await}
      </div>
    {:else if route.name === 'cache'}
      <div class="admin">
        {#await import('./Cache.svelte')}
          <p class="muted">Загрузка…</p>
        {:then { default: Cache }}
          <Cache {me} {users} {onexpired} />
        {:catch e}
          <LazyFail error={e} />
        {/await}
      </div>
    {:else if route.name === 'sessions'}
      <!-- «список — карточка», как журналы: ширина страницы журнала (M7.2 §5.8 п. 3) -->
      <div class="journal-page">
        {#await import('../components/SessionsView.svelte')}
          <p class="muted">Загрузка…</p>
        {:then { default: SessionsView }}
          <SessionsView {me} {users} {onexpired} session={route.session ?? null} />
        {:catch e}
          <LazyFail error={e} />
        {/await}
      </div>
    {:else if route.name === 'mysolutions'}
      <div class="admin">
        <Solutions mine {onexpired} />
      </div>
    {:else if route.name === 'attachment'}
      {@const r = route}
      {#await import('../components/AttachmentView.svelte')}
        <p class="muted">Загрузка…</p>
      {:then { default: AttachmentView }}
        <AttachmentView
          user={r.user}
          space={r.space}
          node={r.node}
          sha={r.sha}
          file={r.file}
          locator={r.locator}
          {onexpired}
        />
      {:catch e}
        <LazyFail error={e} />
      {/await}
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
  /* Ширины — из web/src/lib/layout.ts: 1280 (дерево 300 → 260), 1024 (дерево — панель, разделы
     короткими подписями), 720 (телефон: разделы — в панели), 420 (без названия). */
  .shell {
    min-height: var(--app-h);
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
    padding: var(--safe-top) max(16px, var(--safe-right)) 0 max(16px, var(--safe-left));
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .brand {
    flex: none;
    font-weight: 650;
    color: var(--text);
    text-decoration: none;
  }
  .section-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .topnav {
    display: flex;
    gap: 4px;
    /* пунктов больше, чем влезает на узком экране: меню прокручивается само, страница — нет */
    min-width: 0;
    overflow-x: auto;
    /* с мышью при переполнении видна тонкая полоса прокрутки, при касании — скрыта */
    scrollbar-width: thin;
  }
  .topnav a {
    flex: none;
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
  /* разделы строками по 44 px — в панели телефона */
  .topnav.rows {
    display: grid;
    gap: 2px;
    overflow: visible;
    margin: 0 0 12px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }
  .topnav.rows a {
    display: flex;
    align-items: center;
    min-height: 44px;
    padding: 0 12px;
    color: var(--text);
  }
  .drawer-heading {
    padding: 4px 10px 6px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .search-btn {
    flex: none;
    margin-left: auto;
    display: flex;
    align-items: center;
    justify-content: center;
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
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .drawer-toggle {
    flex: none;
    display: none;
    place-items: center;
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
    height: calc(var(--app-h) - var(--header-h));
    overflow: auto;
    overscroll-behavior: contain;
    padding: 12px 8px calc(24px + var(--safe-bottom)) max(8px, var(--safe-left));
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .scrim {
    display: none;
  }
  .content {
    grid-area: main;
    min-width: 0;
    padding: 24px max(32px, var(--safe-right)) calc(64px + var(--safe-bottom)) 32px;
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
  /* полоса журналов и подменю администрирования (раскладка — `.strip`, app.css); линия под
     пунктами — тенью: при прокрутке полосы подчёркивание выбранного не обрезается */
  .subnav {
    margin-bottom: 16px;
    box-shadow: inset 0 -1px var(--border);
  }
  .subnav a {
    padding: 6px 12px;
    color: var(--muted);
    text-decoration: none;
    border-bottom: 2px solid transparent;
  }
  .subnav a[aria-current='page'] {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  /* числа полосы журналов (§5.3 п. 2) — с «открыто N» у «Вопросов» одним элементом поверх правого
     поля своей ссылки: ссылка оставляет под него место (`--num-w` — измеренная ширина элемента, до
     замера — число знаков × 1ch), элемент сдвинут влево на поле, свою ширину и зазор полосы (2 px,
     `.strip` в app.css) — в ряду он места не занимает: перенос строки на настольном не отрывает ни
     число, ни метку от журнала (ревью r1, M3), щелчок по ним попадает в ссылку */
  .subnav a.counted {
    padding-right: calc(18px + var(--num-w));
  }
  .strip-over {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin: 0 12px 0 calc(-12px - var(--num-w) - 2px);
    align-self: center;
    pointer-events: none;
    scroll-snap-align: none;
  }
  /* ноль — `--muted` без прозрачности: на `--bg` 4,64 : 1 (светлая) и 6,98 : 1 (тёмная), WCAG AA
     (ревью r1, M2); прочие числа — цветом текста */
  .strip-num {
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }
  .strip-num.zero {
    color: var(--muted);
  }
  .strip-over .tag.open {
    margin-left: 0;
    background: var(--accent);
    color: var(--accent-text);
  }
  .welcome {
    max-width: 640px;
    margin: 10vh auto 0;
    text-align: center;
  }

  @media (pointer: coarse) {
    /* разделы шапки в ряд — с зазором между целями (`--touch-gap`, app.css) */
    .topnav:not(.rows) {
      gap: var(--touch-gap);
      scrollbar-width: none;
    }
    .topnav:not(.rows)::-webkit-scrollbar {
      display: none;
    }
    /* строки «Разделов» панели — свои правила (44 px, по левому краю) */
    .topnav:not(.rows) a {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 44px;
      min-width: 44px;
    }
    .drawer-toggle,
    .search-btn {
      min-width: 44px;
      min-height: 44px;
    }
    /* зазор полосы при касании — `--touch-gap` */
    .strip-over {
      margin-left: calc(-12px - var(--num-w) - var(--touch-gap));
    }
  }

  /* настольный до 1279: дерево уже, подсказка Ctrl K — только в title */
  @media (max-width: 1279px) {
    .shell {
      grid-template-columns: 260px minmax(0, 1fr);
    }
    .topbar {
      gap: 12px;
    }
    .search-btn kbd,
    .search-label {
      display: none;
    }
  }

  /* планшет и телефон: дерево — выдвижная панель, разделы в шапке — короткими подписями */
  @media (max-width: 1023px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-areas:
        'top'
        'main';
    }
    .topbar {
      gap: 8px;
      padding-left: max(8px, var(--safe-left));
      padding-right: max(8px, var(--safe-right));
    }
    .drawer-toggle {
      display: grid;
    }
    .topnav:not(.rows) a {
      padding: 6px;
      font-size: 14px;
    }
    .topnav:not(.rows) .long {
      display: none;
    }
    .topnav:not(.rows) .short {
      display: inline;
    }
    .login {
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
      padding: 20px max(20px, var(--safe-right)) calc(56px + var(--safe-bottom)) max(20px, var(--safe-left));
    }
  }
  @media (max-width: 719px) {
    .content {
      padding: 16px max(14px, var(--safe-right)) calc(48px + var(--safe-bottom)) max(14px, var(--safe-left));
    }
  }
  @media (max-width: 419px) {
    .brand {
      display: none;
    }
  }
</style>
