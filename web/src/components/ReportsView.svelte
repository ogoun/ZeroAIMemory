<script lang="ts">
  // Журнал «Отчёты» решения (M5.5): лента (новые — первыми) с курсором, фильтры по тегу и
  // области (на сервере, курсор их учитывает), поиск по заголовку и тексту — `POST /search`
  // `kinds=[report]` в пространстве решения (FTS; область действует и в поиске, тег — нет).
  // У владельца решения — «Корзина» с «Восстановить». Открытая запись — страница отчёта.
  // Отчёты пишет только агент: кнопок создания и правки нет. Ленивый чанк (Shell).
  import { untrack } from 'svelte';
  import ReportView from './ReportView.svelte';
  import {
    ApiError,
    errorText,
    listReports,
    restoreEntry,
    searchReports,
    type ReportItem,
    type SearchItem,
  } from '../lib/api';
  import { fmtTime } from '../lib/journals';
  import { reportAuthor, uniqueHits } from '../lib/reports';
  import { snippetParts } from '../lib/snippet';
  import { go, href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    /** Имя решения (проект) — в шапке печати отчёта. */
    project: string;
    /** Открытый отчёт; `null` — лента. */
    entry: string | null;
    /** Владелец решения: корзина, удаление и восстановление. */
    owner: boolean;
    onexpired: () => void;
  }

  let { user, space, project, entry, owner, onexpired }: Props = $props();

  const PAGE = 20;

  let items = $state<ReportItem[]>([]);
  let next = $state<string | null>(null);
  let tag = $state('');
  let area = $state('');
  let query = $state('');
  /** Применённые фильтры (лента и поиск — по ним). */
  let applied = $state({ tag: '', area: '', query: '' });
  let trash = $state(false);
  let hits = $state<SearchItem[] | null>(null);
  let loading = $state(false);
  let error = $state('');
  let notice = $state('');
  let seq = 0;

  /** Решение, для которого заданы фильтры: эффект срабатывает на любую смену маршрута. */
  let filtersFor = '';
  $effect(() => {
    const key = JSON.stringify([user, space]);
    if (key === filtersFor) return;
    filtersFor = key;
    untrack(() => {
      // другое решение: выдача прежнего не показывается и не дописывается ответом в полёте
      seq++;
      items = [];
      hits = null;
      next = null;
      error = '';
      tag = '';
      area = '';
      query = '';
      applied = { tag: '', area: '', query: '' };
      trash = false;
      notice = '';
    });
  });

  // Лента грузится, только когда она видна: при открытии и при возврате со страницы отчёта
  // (отчёт могли удалить) — свежая, с прежними фильтрами.
  $effect(() => {
    const u = user;
    const s = space;
    if (entry) return;
    untrack(() => load(u, s, true));
  });

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = errorText(e);
  }

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    const f = applied;
    try {
      if (f.query && !trash) {
        const r = await searchReports(u, s, f.query, f.area || undefined);
        if (my !== seq) return;
        // `brief` — элемент на каждый чанк: отчёт с совпадениями в нескольких разделах — один
        hits = uniqueHits(r.items);
        items = [];
        next = null;
        return;
      }
      hits = null;
      const r = await listReports(u, s, {
        tag: f.tag || undefined,
        area: f.area || undefined,
        cursor: reset ? null : next,
        deleted: trash,
        limit: PAGE,
      });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) {
        items = [];
        hits = null;
      }
      fail(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  function apply(e: SubmitEvent) {
    e.preventDefault();
    const qtext = query.trim();
    // поиск теги не фильтрует: при запросе тег сбрасывается, а не висит скрытым фильтром
    if (qtext && !trash) tag = '';
    applied = { tag: qtext && !trash ? '' : tag.trim(), area: area.trim(), query: qtext };
    notice = '';
    load(user, space, true);
  }

  function reset() {
    tag = '';
    area = '';
    query = '';
    applied = { tag: '', area: '', query: '' };
    load(user, space, true);
  }

  function toggleTrash() {
    trash = !trash;
    notice = '';
    load(user, space, true);
  }

  async function restore(it: ReportItem) {
    error = '';
    // ответ применяется к той же корзине: за время запроса ленту могли перезагрузить
    const my = seq;
    try {
      await restoreEntry(user, it.id);
      if (my !== seq || !trash) return;
      items = items.filter((x) => x.id !== it.id);
      notice = `Отчёт «${it.title}» восстановлен.`;
    } catch (e) {
      if (my !== seq) return;
      fail(e);
    }
  }

  function ondeleted(title: string) {
    notice = `Отчёт «${title}» перемещён в корзину.`;
    go({ name: 'journal', user, space, journal: 'reports', entry: null });
  }

  const open = (id: string) => href({ name: 'journal', user, space, journal: 'reports', entry: id });

  /** Превью не построилось (422: файл больше предела `[docs]`, повреждён) — один раз сам файл. */
  function thumbFailed(img: HTMLImageElement, full: string) {
    if (img.dataset.full) return;
    img.dataset.full = '1';
    img.src = full;
  }

  let filtered = $derived(!!(applied.tag || applied.area || applied.query));
</script>

{#if entry}
  <ReportView {user} {space} id={entry} {project} {owner} {ondeleted} {onexpired} />
{:else}
  <section class="journal">
    <form class="filters row" onsubmit={apply} aria-label="Фильтры отчётов">
      <label class="row grow">
        Поиск
        <input type="search" bind:value={query} placeholder="заголовок или текст" aria-label="Поиск по отчётам" disabled={trash} />
      </label>
      <label class="row">
        Тег
        <input
          bind:value={tag}
          placeholder="например, ml"
          aria-label="Тег"
          title={query.trim() && !trash ? 'Поиск по тексту теги не фильтрует' : undefined}
          disabled={!!query.trim() && !trash}
        />
      </label>
      <label class="row">
        Область
        <input bind:value={area} placeholder="например, index" aria-label="Область" />
      </label>
      <button type="submit">Применить</button>
      {#if filtered}<button type="button" onclick={reset}>Сбросить</button>{/if}
      {#if owner}
        <button type="button" class="trash" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
      {/if}
    </form>

    {#if notice}<p class="ok" role="status">{notice}</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if trash}<p class="muted small">Корзина: удалённые отчёты решения (видит только владелец).</p>{/if}

    {#if hits}
      <div class="feed" role="feed" aria-label="Найденные отчёты" aria-busy={loading}>
        {#each hits as h (h.entry_id)}
          <article class="report-card hit" data-report={h.entry_id}>
            <div class="text">
              <h3><a href={open(h.entry_id ?? '')}>{h.title ?? ''}</a></h3>
              {#if h.created_at}<div class="meta muted"><time datetime={h.created_at}>{fmtTime(h.created_at)}</time></div>{/if}
              <p class="snippet">
                {#each snippetParts(h.snippet ?? '') as p, j (j)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
              </p>
            </div>
          </article>
        {/each}
      </div>
      {#if !loading && hits.length === 0 && !error}<p class="muted">Ничего не найдено.</p>{/if}
    {:else}
      <div class="feed" role="feed" aria-label="Лента отчётов" aria-busy={loading}>
        {#each items as it (it.id)}
          <article class="report-card" data-report={it.id}>
            {#if it.first_image}
              <img
                class="thumb"
                src={it.first_image.thumb_url ?? it.first_image.url}
                alt=""
                loading="lazy"
                decoding="async"
                onerror={(e) => thumbFailed(e.currentTarget as HTMLImageElement, it.first_image!.url)}
              />
            {:else}
              <div class="thumb empty" aria-hidden="true"></div>
            {/if}
            <div class="text">
              <h3>
                {#if trash}{it.title}{:else}<a href={open(it.id)}>{it.title}</a>{/if}
              </h3>
              <div class="meta muted">
                <time datetime={it.created_at}>{fmtTime(it.created_at)}</time> · {reportAuthor(it.author)}
                {#if it.area}· область <span class="chip">{it.area}</span>{/if}
                {#each it.tags as t (t)}<span class="chip">#{t}</span>{/each}
              </div>
              {#if trash}
                <div class="meta muted">удалён {fmtTime(it.deleted_at)}</div>
                <button type="button" onclick={() => restore(it)}>Восстановить</button>
              {/if}
            </div>
          </article>
        {/each}
      </div>
      {#if !loading && items.length === 0 && !error}
        <p class="muted">
          {trash ? 'Корзина пуста.' : filtered ? 'Отчётов по выбранным фильтрам нет.' : 'Отчётов нет: их пишет агент (zam report).'}
        </p>
      {/if}
      {#if next && !loading}
        <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
      {/if}
    {/if}
    {#if loading}<p class="muted">Загрузка…</p>{/if}
  </section>
{/if}

<style>
  .filters {
    flex-wrap: wrap;
    margin-bottom: 16px;
  }
  .filters label {
    gap: 6px;
  }
  .filters .grow {
    flex: 1 1 220px;
  }
  .filters .grow input {
    flex: 1;
    min-width: 0;
  }
  .trash[aria-pressed='true'] {
    border-color: var(--accent);
    background: var(--selected);
  }
  .feed {
    display: grid;
    gap: 10px;
  }
  .report-card {
    display: flex;
    gap: 14px;
    padding: 10px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .thumb {
    flex: none;
    width: 120px;
    height: 80px;
    object-fit: cover;
    border-radius: 6px;
    background: var(--chip);
  }
  .text {
    min-width: 0;
  }
  .report-card h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .report-card h3 a {
    color: var(--text);
    text-decoration: none;
  }
  .report-card h3 a:hover {
    color: var(--accent);
  }
  .meta {
    font-size: 12px;
    margin-top: 2px;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .snippet {
    margin: 4px 0 0;
    font-size: 14px;
    overflow-wrap: anywhere;
  }
  .snippet mark {
    background: var(--flash);
    color: inherit;
    border-radius: 2px;
  }
  .small {
    font-size: 12px;
  }
  @media (max-width: 719px) {
    .thumb {
      width: 72px;
      height: 54px;
    }
  }
</style>
