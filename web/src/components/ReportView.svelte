<script lang="ts">
  // Страница отчёта (M5.5): HTML с сервера (санитизирован ammonia, файлы — только этой записи)
  // «оживляется» как документ — якоря, копирование кода, лайтбокс, вики-ссылки, формулы и
  // диаграммы. Шапка: заголовок (первый `#` тела, совпадающий с ним, не дублируется), дата,
  // автор, область, теги; ссылки `data.refs`; прочие файлы — на скачивание. Отчёт неизменен:
  // кнопок правки нет; «Удалить» — только владельцу решения (в корзину, с подтверждением).
  import { tick, untrack } from 'svelte';
  import Lightbox from './Lightbox.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import {
    ApiError,
    deleteEntry,
    errorText,
    getNode,
    getReportHtml,
    listSpaces,
    spaceRef,
    type EntryAttachment,
    type ReportHtml,
  } from '../lib/api';
  import { copyText } from '../lib/clipboard';
  import { codeOf, flashButton } from '../lib/hydrate/dom';
  import { hydrateRendered } from '../lib/hydrate/render';
  import { fmtTime } from '../lib/journals';
  import { reportAuthor, fmtSize, refsOf, REF_TITLE } from '../lib/reports';
  import { anchorId, go, href, parse } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    id: string;
    /** Владелец решения: может удалить отчёт. */
    owner: boolean;
    /** Отчёт удалён (переход в ленту делает родитель). */
    ondeleted: (title: string) => void;
    onexpired: () => void;
  }

  let { user, space, id, owner, ondeleted, onexpired }: Props = $props();

  let report = $state<ReportHtml | null>(null);
  let error = $state('');
  let notice = $state('');
  let loading = $state(false);
  let body = $state<HTMLDivElement | null>(null);
  let lightbox = $state<{ src: string; alt: string } | null>(null);
  let ask = $state<Ask | null>(null);
  /** Адреса заметок из `data.refs.notes` (`n:<id>` → страница заметки); `null` — не найдена. */
  let noteHrefs = $state<Record<string, string | null>>({});
  let seq = 0;

  /** Загруженный отчёт: эффект срабатывает на любую смену маршрута (props — из `route`). */
  let loadedFor = '';
  $effect(() => {
    const u = user;
    const i = id;
    const key = JSON.stringify([u, i]);
    if (key === loadedFor) return;
    loadedFor = key;
    untrack(() => load(u, i));
  });

  async function load(u: string, i: string) {
    const my = ++seq;
    loading = true;
    error = '';
    notice = '';
    report = null;
    noteHrefs = {};
    try {
      const r = await getReportHtml(u, i);
      if (my !== seq) return;
      report = r;
      void resolveNotes(u, r, my);
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error =
        e instanceof ApiError && e.status === 404
          ? 'Отчёт не найден: возможно, он удалён.'
          : errorText(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  /** `n:<id>` → адрес заметки в её пространстве (узел может лежать вне решения отчёта). */
  async function resolveNotes(u: string, r: ReportHtml, my: number) {
    const notes = refsOf(r.entry.data).find(([k]) => k === 'notes')?.[1] ?? [];
    if (!notes.length) return;
    let spaces: Awaited<ReturnType<typeof listSpaces>>['spaces'] = [];
    try {
      spaces = (await listSpaces(u)).spaces;
    } catch {
      /* без списка пространств — ссылки не строятся */
    }
    const out: Record<string, string | null> = {};
    await Promise.all(
      notes.map(async (ref) => {
        const nid = ref.replace(/^n:/, '');
        try {
          const n = (await getNode(u, nid)).node;
          const s = spaces.find((x) => x.id === n.space_id);
          out[ref] = s ? href({ name: 'node', user: u, space: spaceRef(s), node: n.id, anchor: null }) : null;
        } catch {
          out[ref] = null;
        }
      }),
    );
    if (my === seq) noteHrefs = out;
  }

  // Оживление после вставки нового HTML.
  $effect(() => {
    const r = report;
    const el = body;
    if (r && el) untrack(() => hydrate(r, el));
  });

  async function hydrate(r: ReportHtml, el: HTMLElement) {
    await tick();
    if (report !== r) return;
    const ok = await hydrateRendered(el, {
      wiki: (n, a) => href({ name: 'node', user, space, node: n, anchor: a }),
      // якоря отчёта остаются `#<id>`: у страницы журнала своего якоря в адресе нет
      self: (a) => `#${a}`,
      alive: () => report === r,
    });
    if (report === r && !ok) notice = 'Не удалось загрузить модуль формул или диаграмм: обновите страницу.';
  }

  function scrollTo(raw: string) {
    const el = document.getElementById(anchorId(raw));
    if (!el || !body?.contains(el)) return;
    el.scrollIntoView({ block: 'start' });
    el.classList.remove('flash');
    void el.offsetWidth;
    el.classList.add('flash');
  }

  // Делегирование кликов: копирование кода, лайтбокс, якоря отчёта, ссылки SPA.
  async function onclick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    const btn = t.closest<HTMLButtonElement>('button[data-copy]');
    if (btn) {
      const ok = await copyText(codeOf(btn));
      flashButton(btn, ok ? 'Скопировано' : 'Не удалось');
      return;
    }
    const a = t.closest<HTMLAnchorElement>('a');
    const img = t.closest<HTMLImageElement>('img');
    if (img && !a) {
      lightbox = { src: img.currentSrc || img.src, alt: img.alt };
      return;
    }
    if (!a) return;
    if (a.classList.contains('broken')) {
      e.preventDefault();
      return;
    }
    const raw = a.getAttribute('href') ?? '';
    if (!raw.startsWith('#') || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    if (raw.startsWith('#/')) go(parse(raw));
    else scrollTo(raw.slice(1));
  }

  /** Первый элемент тела — `<h1>` с текстом заголовка: в шапке заголовок не повторяется. */
  let titleInBody = $derived.by(() => {
    const r = report;
    if (!r) return false;
    const t = document.createElement('template');
    t.innerHTML = r.html;
    const first = t.content.firstElementChild;
    return first?.tagName === 'H1' && (first.textContent ?? '').trim() === r.entry.title.trim();
  });

  let refs = $derived(report ? refsOf(report.entry.data) : []);
  /** Файлы, кроме картинок (картинки — в теле). */
  let others = $derived((report?.files ?? []).filter((f) => !f.mime.startsWith('image/')));

  const taskHref = (ref: string) =>
    href({ name: 'journal', user, space, journal: 'tasks', entry: ref.replace(/^k:/, '') });

  function askDelete() {
    const r = report;
    if (!r) return;
    ask = {
      title: 'Удалить отчёт?',
      message: `Отчёт «${r.entry.title}» будет перемещён в корзину; восстановить его можно в «Корзине» ленты.`,
      confirm: 'Удалить',
      danger: true,
      run: async () => {
        try {
          await deleteEntry(user, r.entry.id, r.entry.rev);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) return onexpired();
          throw new Error(errorText(e));
        }
        ondeleted(r.entry.title);
      },
    };
  }

  const fileLabel = (f: EntryAttachment) => `${f.filename} (${fmtSize(f.size)})`;
</script>

<svelte:head>
  <link rel="stylesheet" href="api/v1/render/highlight.css" />
</svelte:head>

<article class="report" data-report={id}>
  <a class="back" href={href({ name: 'journal', user, space, journal: 'reports', entry: null })}>← Все отчёты</a>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading && !report}<p class="muted">Загрузка…</p>{/if}
  {#if report}
    {@const e = report.entry}
    <header class="head">
      {#if !titleInBody}<h2 class="title">{e.title}</h2>{/if}
      <div class="meta muted">
        <time datetime={e.created_at}>{fmtTime(e.created_at)}</time> · {reportAuthor(e.author)}
        {#if e.area}· область <span class="chip">{e.area}</span>{/if}
        {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
      </div>
      {#if owner}
        <div class="actions">
          <button type="button" class="danger" onclick={askDelete}>Удалить</button>
        </div>
      {/if}
    </header>
    {#if notice}<p class="warn" role="status">{notice}</p>{/if}
    {#if report.warnings.length}
      <details class="render-warnings">
        <summary>Предупреждения рендера: {report.warnings.length}</summary>
        <ul>
          {#each report.warnings as w, i (i)}<li>{w}</li>{/each}
        </ul>
      </details>
    {/if}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="doc-click" {onclick}>
      {#if report.toc.length >= 3}
        <details class="toc">
          <summary>Содержание</summary>
          <ul>
            {#each report.toc as t, i (i)}
              <li style:--lvl={t.level}><a href={`#${encodeURIComponent(`h-${t.anchor}`)}`}>{t.title}</a></li>
            {/each}
          </ul>
        </details>
      {/if}
      {#key report}
        <!-- HTML ответа сервера: санитизирован ammonia, картинки — файлы этой записи -->
        <div class="doc" bind:this={body}>{@html report.html}</div>
      {/key}
    </div>
    {#if refs.length}
      <section class="refs" aria-label="Ссылки отчёта">
        <h3>Ссылки</h3>
        {#each refs as [k, v] (k)}
          <div class="ref-row" data-ref={k}>
            <span class="muted">{REF_TITLE[k] ?? k}:</span>
            {#each v as x, i (i)}
              {#if k === 'notes' && noteHrefs[x]}
                <a href={noteHrefs[x]}>{x}</a>
              {:else if k === 'tasks'}
                <a href={taskHref(x)}>{x}</a>
              {:else}
                <code>{x}</code>
              {/if}
            {/each}
          </div>
        {/each}
      </section>
    {/if}
    {#if others.length}
      <section class="files" aria-label="Файлы отчёта">
        <h3>Файлы</h3>
        <ul>
          {#each others as f (f.sha256 + f.filename)}
            <li><a href={f.url} download={f.filename}>{fileLabel(f)}</a> <span class="muted">{f.mime}</span></li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</article>

{#if lightbox}
  <Lightbox src={lightbox.src} alt={lightbox.alt} onclose={() => (lightbox = null)} />
{/if}
<Prompt bind:ask />

<style>
  .report {
    max-width: 860px;
    margin: 0 auto;
  }
  .back {
    font-size: 13px;
    text-decoration: none;
  }
  .head {
    margin: 8px 0 16px;
  }
  .title {
    margin: 0 0 4px;
    font-size: 24px;
  }
  .meta {
    font-size: 12px;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .render-warnings,
  .toc {
    margin: 0 0 16px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: 14px;
  }
  .render-warnings {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .toc ul {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }
  .toc li {
    --lvl: 1;
    padding: 2px 0 2px calc((var(--lvl) - 1) * 16px);
  }
  .toc a {
    color: var(--text);
    text-decoration: none;
  }
  .refs,
  .files {
    margin-top: 24px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
    font-size: 14px;
  }
  .refs h3,
  .files h3 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .ref-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 4px;
    overflow-wrap: anywhere;
  }
  .files ul {
    margin: 0;
    padding-left: 18px;
  }
</style>
