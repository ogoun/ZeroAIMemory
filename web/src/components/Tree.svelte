<script lang="ts" module>
  import type { NodeInfo } from '../lib/api';

  /** Изменение узла из дерева — для Shell (открытый документ мог исчезнуть или переехать). */
  export type TreeChange = { kind: 'moved' | 'removed' | 'restored'; user: string; node: NodeInfo };
  /** Что раскрыть и выделить: узел `path` пространства `space` пользователя `user`. */
  export type Reveal = { user: string; space: string; path: string };
</script>

<script lang="ts">
  import { tick, untrack } from 'svelte';
  import ContextMenu, { type MenuItem } from './ContextMenu.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import {
    ApiError,
    createNode,
    deleteNode,
    listChildren,
    listSpaces,
    listTrash,
    moveNode,
    restoreNode,
    spaceMatches,
    spaceRef,
    type NodeKind,
    type Space,
    type User,
  } from '../lib/api';
  import { go, href, routeUser, type Route } from '../lib/router';
  import IndexBadge from './IndexBadge.svelte';

  interface Props {
    /** Пользователи; свой — первым. */
    users: User[];
    ownId: string;
    route: Route;
    reveal: Reveal | null;
    onchange: (c: TreeChange) => void;
    onexpired: () => void;
  }

  let { users, ownId, route, reveal, onchange, onexpired }: Props = $props();

  // Ключи состояния: `u|<user>` — пространства пользователя, `sol|<user>` — группа решений,
  // `s|<user>|<ref>` — корень пространства, `n|<user>|<ref>|<path>` — папка, `t|<user>` — корзина.
  const kUser = (u: string) => `u|${u}`;
  const kSol = (u: string) => `sol|${u}`;
  const kSpace = (u: string, ref: string) => `s|${u}|${ref}`;
  const kNode = (u: string, ref: string, path: string) => `n|${u}|${ref}|${path}`;
  const kTrash = (u: string) => `t|${u}`;

  let open = $state<Record<string, boolean>>({});
  let spaces = $state<Record<string, Space[]>>({});
  let kids = $state<Record<string, NodeInfo[]>>({});
  let trash = $state<Record<string, NodeInfo[]>>({});
  let busy = $state<Record<string, boolean>>({});
  let errs = $state<Record<string, string>>({});

  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let ask = $state<Ask | null>(null);

  const FIXED_ORDER = ['rules', 'human', 'knowledge'];
  const fixedSpaces = (list: Space[]) =>
    list
      .filter((s) => s.kind !== 'solution')
      .sort((a, b) => FIXED_ORDER.indexOf(a.kind) - FIXED_ORDER.indexOf(b.kind));
  const solutions = (list: Space[]) =>
    list.filter((s) => s.kind === 'solution').sort((a, b) => a.name.localeCompare(b.name));

  const parentPath = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');
  const lastSegment = (p: string) => p.slice(p.lastIndexOf('/') + 1);

  /** Пользователь корзины — тот, в чьём контексте открыт интерфейс. */
  let trashUser = $derived(routeUser(route) ?? ownId);
  let trashUserName = $derived(users.find((u) => u.id === trashUser)?.name ?? '');

  function fail(key: string, e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    errs[key] = e instanceof Error ? e.message : 'Ошибка';
  }

  /** Запуск действия из окна: потеря сессии — на экран входа, остальное — текстом в окне. */
  async function guarded(fn: () => Promise<void>) {
    try {
      await fn();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onexpired();
      throw e;
    }
  }

  async function loadSpaces(uid: string, force = false) {
    const k = kUser(uid);
    if (spaces[uid] && !force) return;
    busy[k] = true;
    delete errs[k];
    try {
      spaces[uid] = (await listSpaces(uid)).spaces;
    } catch (e) {
      fail(k, e);
    } finally {
      busy[k] = false;
    }
  }

  async function loadKids(uid: string, ref: string, path: string) {
    const k = path ? kNode(uid, ref, path) : kSpace(uid, ref);
    busy[k] = true;
    delete errs[k];
    try {
      kids[k] = (await listChildren(uid, ref, path)).items;
    } catch (e) {
      fail(k, e);
    } finally {
      busy[k] = false;
    }
  }

  async function loadTrash(uid: string) {
    const k = kTrash(uid);
    busy[k] = true;
    delete errs[k];
    try {
      trash[uid] = (await listTrash(uid)).items;
    } catch (e) {
      fail(k, e);
    } finally {
      busy[k] = false;
    }
  }

  async function toggle(key: string, load: () => Promise<void>, loaded: boolean) {
    open[key] = !open[key];
    if (open[key] && !loaded) await load();
  }

  /** Перечитывает раскрытые уровни пространства после изменения. */
  async function refreshSpace(uid: string, ref: string) {
    const root = kSpace(uid, ref);
    const prefix = `n|${uid}|${ref}|`;
    const loads: Promise<void>[] = [];
    for (const k of Object.keys(kids)) {
      if (k === root || k.startsWith(prefix)) delete kids[k];
    }
    if (open[root]) loads.push(loadKids(uid, ref, ''));
    for (const k of Object.keys(open)) {
      if (open[k] && k.startsWith(prefix)) loads.push(loadKids(uid, ref, k.slice(prefix.length)));
    }
    await Promise.all(loads);
  }

  async function refreshTrash(uid: string) {
    if (open[kTrash(uid)]) await loadTrash(uid);
    else delete trash[uid];
  }

  // Свой пользователь раскрыт сразу.
  $effect(() => {
    const own = ownId;
    untrack(() => {
      if (open[kUser(own)] === undefined) {
        open[kUser(own)] = true;
        loadSpaces(own);
      }
    });
  });

  // Раскрытие предков открытого документа.
  $effect(() => {
    const r = reveal;
    if (r) untrack(() => doReveal(r));
  });

  async function doReveal(r: Reveal) {
    open[kUser(r.user)] = true;
    await loadSpaces(r.user);
    const s = spaces[r.user]?.find((x) => spaceMatches(x, r.space));
    if (!s) return;
    const ref = spaceRef(s);
    if (s.kind === 'solution') open[kSol(r.user)] = true;
    open[kSpace(r.user, ref)] = true;
    if (!kids[kSpace(r.user, ref)]) await loadKids(r.user, ref, '');
    const segs = r.path.split('/');
    for (let i = 1; i < segs.length; i++) {
      const p = segs.slice(0, i).join('/');
      const k = kNode(r.user, ref, p);
      open[k] = true;
      if (!kids[k]) await loadKids(r.user, ref, p);
    }
    await tick();
    document
      .querySelector('.tree [aria-current="page"]')
      ?.scrollIntoView({ block: 'nearest' });
  }

  // ---- контекстное меню и действия ----

  type Target = { uid: string; space: Space; node: NodeInfo | null };

  function itemsFor(t: Target): MenuItem[] {
    const base = t.node ? (t.node.kind === 'folder' ? t.node.path : parentPath(t.node.path)) : '';
    const items: MenuItem[] = [
      { label: 'Новая заметка', action: () => askCreate(t, base, 'note') },
      { label: 'Новая папка', action: () => askCreate(t, base, 'folder') },
    ];
    const node = t.node;
    if (node) {
      items.push(
        { label: 'Переименовать/переместить', action: () => askMove(t, node) },
        { label: 'Удалить', danger: true, action: () => askDelete(t, node) },
      );
    }
    return items;
  }

  function menuAt(e: MouseEvent, t: Target) {
    e.preventDefault();
    e.stopPropagation();
    menu = { x: e.clientX, y: e.clientY, items: itemsFor(t) };
  }

  function menuBelow(e: MouseEvent, t: Target) {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 2, items: itemsFor(t) };
  }

  function askCreate(t: Target, base: string, kind: NodeKind) {
    const ref = spaceRef(t.space);
    const where = base ? `${t.space.name} / ${base}` : t.space.name;
    ask = {
      title: kind === 'note' ? 'Новая заметка' : 'Новая папка',
      message: `В «${where}». Через «/» можно указать вложенный путь.`,
      label: 'Имя',
      placeholder: kind === 'note' ? 'Например, Установка' : 'Например, Инструменты',
      confirm: 'Создать',
      run: (name) =>
        guarded(async () => {
          const path = base ? `${base}/${name}` : name;
          const n = await createNode(t.uid, {
            space: ref,
            path,
            kind,
            content: kind === 'note' ? `# ${lastSegment(name)}\n` : undefined,
          });
          open[kSpace(t.uid, ref)] = true;
          for (let p = parentPath(n.path); p; p = parentPath(p)) open[kNode(t.uid, ref, p)] = true;
          if (kind === 'folder') open[kNode(t.uid, ref, n.path)] = true;
          await refreshSpace(t.uid, ref);
          if (kind === 'note') go({ name: 'node', user: t.uid, space: ref, node: n.id, anchor: null });
        }),
    };
  }

  function askMove(t: Target, node: NodeInfo) {
    const ref = spaceRef(t.space);
    ask = {
      title: 'Переименовать или переместить',
      message: `Новый путь в пространстве «${t.space.name}».`,
      label: 'Путь',
      value: node.path,
      confirm: 'Сохранить',
      run: (path) =>
        guarded(async () => {
          if (path === node.path) return;
          const moved = await moveNode(t.uid, node.id, path);
          for (let p = parentPath(moved.path); p; p = parentPath(p)) open[kNode(t.uid, ref, p)] = true;
          await refreshSpace(t.uid, ref);
          onchange({ kind: 'moved', user: t.uid, node: moved });
        }),
    };
  }

  function askDelete(t: Target, node: NodeInfo) {
    const ref = spaceRef(t.space);
    const what = node.kind === 'folder' ? 'Папка вместе с содержимым' : 'Заметка';
    ask = {
      title: 'Удалить?',
      message: `${what} «${node.path}» будет перемещена в корзину; её можно восстановить.`,
      confirm: 'Удалить',
      danger: true,
      run: () =>
        guarded(async () => {
          await deleteNode(t.uid, node.id);
          await refreshSpace(t.uid, ref);
          await refreshTrash(t.uid);
          onchange({ kind: 'removed', user: t.uid, node });
        }),
    };
  }

  async function restore(uid: string, node: NodeInfo) {
    const k = kTrash(uid);
    delete errs[k];
    try {
      const n = await restoreNode(uid, node.id);
      await loadTrash(uid);
      await loadSpaces(uid);
      const s = spaces[uid]?.find((x) => x.id === n.space_id);
      if (s) await refreshSpace(uid, spaceRef(s));
      onchange({ kind: 'restored', user: uid, node: n });
    } catch (e) {
      fail(k, e);
    }
  }

  const spaceName = (uid: string, id: string) =>
    spaces[uid]?.find((s) => s.id === id)?.name ?? '';

  const fmtTime = (ts: string | null) => {
    if (!ts) return '';
    const d = new Date(ts);
    return Number.isNaN(d.getTime()) ? ts : d.toLocaleString('ru-RU', { dateStyle: 'short', timeStyle: 'short' });
  };

  const isSelected = (uid: string, id: string) =>
    route.name === 'node' && route.user === uid && route.node === id;
</script>

{#snippet chevron(isOpen: boolean)}
  <svg class="chev" class:open={isOpen} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
{/snippet}

{#snippet icon(kind: string)}
  <svg class="ico" viewBox="0 0 16 16" aria-hidden="true">
    {#if kind === 'folder'}
      <path d="M1.5 4.5a1 1 0 0 1 1-1h3.6l1.4 1.5h6a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.2" />
    {:else if kind === 'note'}
      <path d="M3.5 1.5h6l3 3v10h-9z M9.5 1.5v3h3 M5.5 8h5 M5.5 10.5h5" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
    {:else if kind === 'user'}
      <circle cx="8" cy="5.5" r="2.8" fill="none" stroke="currentColor" stroke-width="1.2" />
      <path d="M2.5 14c.6-3 2.8-4.5 5.5-4.5s4.9 1.5 5.5 4.5" fill="none" stroke="currentColor" stroke-width="1.2" />
    {:else if kind === 'trash'}
      <path d="M2.5 4h11 M6 4V2.5h4V4 M4 4l.7 10h6.6L12 4" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
    {:else if kind === 'solutions'}
      <path d="M2 3.5h5v4H2z M9 3.5h5v4H9z M2 9.5h5v4H2z M9 9.5h5v4H9z" fill="none" stroke="currentColor" stroke-width="1.2" />
    {:else if kind === 'human'}
      <path d="M8 14s-5.5-3.2-5.5-7.2A3 3 0 0 1 8 5a3 3 0 0 1 5.5 1.8C13.5 10.8 8 14 8 14z" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
    {:else if kind === 'rules'}
      <path d="M3 2.5h10v11H3z M5.5 5.5h5 M5.5 8h5 M5.5 10.5h3" fill="none" stroke="currentColor" stroke-width="1.2" />
    {:else}
      <path d="M8 1.8l5.5 3v6.4L8 14.2l-5.5-3V4.8z M2.5 4.8L8 8l5.5-3.2 M8 8v6.2" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
    {/if}
  </svg>
{/snippet}

{#snippet status(key: string)}
  {#if busy[key]}<div class="tree-note muted">Загрузка…</div>{/if}
  {#if errs[key]}<div class="tree-note error" role="alert">{errs[key]}</div>{/if}
{/snippet}

{#snippet nodeList(uid: string, space: Space, key: string, depth: number)}
  {@render status(key)}
  {#if kids[key]?.length === 0}
    <div class="tree-note muted" style:--depth={depth}>Пусто</div>
  {/if}
  <ul>
    {#each kids[key] ?? [] as n (n.id)}
      {@const ref = spaceRef(space)}
      {@const k = kNode(uid, ref, n.path)}
      {@const expandable = n.kind === 'folder' || n.has_children}
      {@const target = { uid, space, node: n }}
      <li>
        <div
          class="tree-row"
          class:selected={isSelected(uid, n.id)}
          style:--depth={depth}
          oncontextmenu={(e) => menuAt(e, target)}
          role="presentation"
        >
          {#if expandable}
            <button
              type="button"
              class="twisty"
              aria-expanded={!!open[k]}
              aria-label={open[k] ? `Свернуть ${n.name}` : `Развернуть ${n.name}`}
              onclick={() => toggle(k, () => loadKids(uid, ref, n.path), !!kids[k])}
            >
              {@render chevron(!!open[k])}
            </button>
          {:else}
            <span class="twisty"></span>
          {/if}
          {#if n.kind === 'note'}
            <a
              class="label"
              href={href({ name: 'node', user: uid, space: ref, node: n.id, anchor: null })}
              aria-current={isSelected(uid, n.id) ? 'page' : undefined}
              title={n.path}
            >
              {@render icon('note')}<span class="name">{n.name}</span>
            </a>
            <IndexBadge state={n.index_state} />
          {:else}
            <button
              type="button"
              class="label"
              title={n.path}
              onclick={() => toggle(k, () => loadKids(uid, ref, n.path), !!kids[k])}
            >
              {@render icon('folder')}<span class="name">{n.name}</span>
            </button>
          {/if}
          <button type="button" class="more" aria-label="Действия: {n.name}" onclick={(e) => menuBelow(e, target)}>⋯</button>
        </div>
        {#if expandable && open[k]}
          {@render nodeList(uid, space, k, depth + 1)}
        {/if}
      </li>
    {/each}
  </ul>
{/snippet}

{#snippet spaceRow(uid: string, s: Space, depth: number)}
  {@const ref = spaceRef(s)}
  {@const k = kSpace(uid, ref)}
  {@const target = { uid, space: s, node: null }}
  <li>
    <div class="tree-row space" style:--depth={depth} oncontextmenu={(e) => menuAt(e, target)} role="presentation">
      <button
        type="button"
        class="twisty"
        aria-expanded={!!open[k]}
        aria-label={open[k] ? `Свернуть ${s.name}` : `Развернуть ${s.name}`}
        onclick={() => toggle(k, () => loadKids(uid, ref, ''), !!kids[k])}
      >
        {@render chevron(!!open[k])}
      </button>
      <button type="button" class="label" onclick={() => toggle(k, () => loadKids(uid, ref, ''), !!kids[k])}>
        {@render icon(s.kind)}<span class="name">{s.name}</span>
      </button>
      <button type="button" class="more" aria-label="Действия: {s.name}" onclick={(e) => menuBelow(e, target)}>⋯</button>
    </div>
    {#if open[k]}
      {@render nodeList(uid, s, k, depth + 1)}
    {/if}
  </li>
{/snippet}

<nav class="tree" aria-label="Дерево заметок">
  <div class="tree-heading">Пользователи</div>
  <ul>
    {#each users as u (u.id)}
      {@const k = kUser(u.id)}
      <li>
        <div class="tree-row user" style:--depth={0}>
          <button
            type="button"
            class="twisty"
            aria-expanded={!!open[k]}
            aria-label={open[k] ? `Свернуть ${u.name}` : `Развернуть ${u.name}`}
            onclick={() => toggle(k, () => loadSpaces(u.id), !!spaces[u.id])}
          >
            {@render chevron(!!open[k])}
          </button>
          <button type="button" class="label" onclick={() => toggle(k, () => loadSpaces(u.id), !!spaces[u.id])}>
            {@render icon('user')}<span class="name">{u.name}</span>
            {#if u.id === ownId}<span class="tag">вы</span>{/if}
            {#if u.disabled_at}<span class="tag">отключён</span>{/if}
          </button>
        </div>
        {#if open[k]}
          {@render status(k)}
          {#if spaces[u.id]}
            <ul>
              {#each fixedSpaces(spaces[u.id]) as s (s.id)}
                {@render spaceRow(u.id, s, 1)}
              {/each}
              <li>
                <div class="tree-row" style:--depth={1}>
                  <button
                    type="button"
                    class="twisty"
                    aria-expanded={!!open[kSol(u.id)]}
                    aria-label={open[kSol(u.id)] ? 'Свернуть Solutions' : 'Развернуть Solutions'}
                    onclick={() => (open[kSol(u.id)] = !open[kSol(u.id)])}
                  >
                    {@render chevron(!!open[kSol(u.id)])}
                  </button>
                  <button type="button" class="label" onclick={() => (open[kSol(u.id)] = !open[kSol(u.id)])}>
                    {@render icon('solutions')}<span class="name">Solutions</span>
                    <span class="count">{solutions(spaces[u.id]).length}</span>
                  </button>
                </div>
                {#if open[kSol(u.id)]}
                  {#if solutions(spaces[u.id]).length === 0}
                    <div class="tree-note muted" style:--depth={2}>Решений пока нет</div>
                  {/if}
                  <ul>
                    {#each solutions(spaces[u.id]) as s (s.id)}
                      {@render spaceRow(u.id, s, 2)}
                    {/each}
                  </ul>
                {/if}
              </li>
            </ul>
          {/if}
        {/if}
      </li>
    {/each}
  </ul>

  <div class="tree-trash">
    {#if trashUser}
      {@const k = kTrash(trashUser)}
      <div class="tree-row" style:--depth={0}>
        <button
          type="button"
          class="twisty"
          aria-expanded={!!open[k]}
          aria-label={open[k] ? 'Свернуть корзину' : 'Развернуть корзину'}
          onclick={() => toggle(k, () => loadTrash(trashUser), !!trash[trashUser])}
        >
          {@render chevron(!!open[k])}
        </button>
        <button type="button" class="label" onclick={() => toggle(k, () => loadTrash(trashUser), !!trash[trashUser])}>
          {@render icon('trash')}<span class="name">Корзина</span>
          <span class="tag">{trashUserName}</span>
        </button>
      </div>
      {#if open[k]}
        {@render status(k)}
        {#if trash[trashUser]?.length === 0}
          <div class="tree-note muted" style:--depth={1}>Корзина пуста</div>
        {/if}
        <ul class="trash-list">
          {#each trash[trashUser] ?? [] as n (n.id)}
            <li class="trash-item">
              <div class="trash-text">
                <span class="name">{@render icon(n.kind)} {n.path}</span>
                <span class="muted small">{spaceName(trashUser, n.space_id)} · {fmtTime(n.deleted_at)}</span>
              </div>
              <button type="button" class="small-btn" onclick={() => restore(trashUser, n)}>Восстановить</button>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </div>
</nav>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}
<Prompt bind:ask />

<style>
  .tree {
    font-size: 14px;
    user-select: none;
  }
  .tree ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .tree-heading {
    padding: 4px 10px 6px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .tree-row {
    --depth: 0;
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: 30px;
    padding: 0 4px 0 calc(4px + var(--depth) * 14px);
    border-radius: 6px;
  }
  .tree-row:hover {
    background: var(--hover);
  }
  .tree-row.selected {
    background: var(--selected);
  }
  .tree-row.space .name {
    font-weight: 500;
  }
  .twisty {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--muted);
  }
  .chev {
    width: 14px;
    height: 14px;
    transition: transform 0.12s;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .label {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 2px;
    border: 0;
    background: transparent;
    color: var(--text);
    text-decoration: none;
    text-align: left;
    cursor: pointer;
  }
  .label .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ico {
    flex: none;
    width: 16px;
    height: 16px;
    color: var(--muted);
  }
  .tag,
  .count {
    flex: none;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    color: var(--muted);
  }
  .more {
    flex: none;
    width: 26px;
    height: 24px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--muted);
    opacity: 0;
    line-height: 1;
  }
  .tree-row:hover .more,
  .tree-row.selected .more,
  .more:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .more {
      opacity: 1;
    }
  }
  .tree-note {
    --depth: 0;
    padding: 2px 8px 2px calc(30px + var(--depth) * 14px);
    font-size: 13px;
  }
  .tree-note.error {
    color: var(--danger);
  }
  .tree-trash {
    margin-top: 16px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .trash-list {
    display: grid;
    gap: 4px;
    padding: 4px 4px 4px 28px !important;
  }
  .trash-item {
    display: flex;
    align-items: center;
    gap: 8px;
    justify-content: space-between;
  }
  .trash-text {
    min-width: 0;
    display: grid;
  }
  .trash-text .name {
    display: flex;
    align-items: center;
    gap: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: 12px;
  }
  .small-btn {
    flex: none;
    padding: 2px 8px;
    font-size: 12px;
  }
</style>
