<script lang="ts">
  // Просмотр заметки: HTML с сервера (санитизирован ammonia) вставляется через {@html}
  // и «оживляется» на клиенте — якоря, копирование кода, лайтбокс, вики-ссылки,
  // формулы (KaTeX) и диаграммы (Mermaid) лениво отдельными чанками.
  import '../doc.css';
  import { tick, untrack } from 'svelte';
  import Lightbox from './Lightbox.svelte';
  import {
    ApiError,
    getNode,
    getNodeHtml,
    listSpaces,
    spaceMatches,
    type NodeInfo,
    type RenderedNode,
  } from '../lib/api';
  import { copyText } from '../lib/clipboard';
  import { codeOf, flashButton, hydrateDom, rewriteLinks } from '../lib/hydrate/dom';
  import { anchorId, go, href, parse } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    node: string;
    anchor: string | null;
    /** Смена значения — перечитать документ (переименование, восстановление). */
    reloadKey: number;
    onloaded: (n: NodeInfo) => void;
    onexpired: () => void;
  }

  let { user, space, node, anchor, reloadKey, onloaded, onexpired }: Props = $props();

  type Tab = 'doc' | 'history' | 'links';
  let tab = $state<Tab>('doc');
  let info = $state<NodeInfo | null>(null);
  let rendered = $state<RenderedNode | null>(null);
  let spaceName = $state('');
  let error = $state('');
  let notice = $state('');
  let loading = $state(false);
  let body = $state<HTMLDivElement | null>(null);
  let lightbox = $state<{ src: string; alt: string } | null>(null);

  let seq = 0;

  $effect(() => {
    const u = user;
    const n = node;
    const s = space;
    void reloadKey;
    untrack(() => load(u, s, n));
  });

  async function load(u: string, s: string, n: string) {
    const my = ++seq;
    loading = true;
    error = '';
    notice = '';
    const [j, h, sp] = await Promise.allSettled([getNode(u, n), getNodeHtml(u, n), listSpaces(u)]);
    if (my !== seq) return;
    loading = false;
    if (sp.status === 'fulfilled') {
      spaceName = sp.value.spaces.find((x) => spaceMatches(x, s))?.name ?? s;
    }
    if (j.status === 'rejected') return failed(j.reason);
    info = j.value.node;
    onloaded(j.value.node);
    if (info.kind === 'folder') {
      rendered = null;
      error = 'Это папка: откройте заметку внутри неё.';
      return;
    }
    if (h.status === 'rejected') return failed(h.reason);
    rendered = h.value;
  }

  function failed(e: unknown) {
    info = null;
    rendered = null;
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error =
      e instanceof ApiError && e.status === 404
        ? 'Заметка не найдена: возможно, она удалена или перемещена.'
        : e instanceof Error
          ? e.message
          : 'Ошибка загрузки';
  }

  // Оживление после вставки нового HTML.
  $effect(() => {
    const r = rendered;
    const el = body;
    if (r && el) untrack(() => hydrate(r, el));
  });

  async function hydrate(r: RenderedNode, el: HTMLElement) {
    await tick();
    if (rendered !== r) return;
    hydrateDom(el);
    rewriteLinks(
      el,
      (n, a) => href({ name: 'node', user, space, node: n, anchor: a }),
      (a) => href({ name: 'node', user, space, node, anchor: a }),
    );
    scrollToAnchor(anchor);
    const jobs: Promise<void>[] = [];
    if (el.querySelector('.math')) {
      jobs.push(import('../lib/hydrate/katex').then((m) => (rendered === r ? m.renderMath(el) : undefined)));
    }
    if (el.querySelector('pre.mermaid-src')) {
      jobs.push(import('../lib/hydrate/mermaid').then((m) => (rendered === r ? m.renderMermaid(el) : undefined)));
    }
    if (jobs.length === 0) return;
    const res = await Promise.allSettled(jobs);
    if (rendered !== r) return;
    if (res.some((x) => x.status === 'rejected')) {
      notice = 'Не удалось загрузить модуль формул или диаграмм: обновите страницу.';
    }
    // формулы и диаграммы меняют высоту блоков — якорь мог сместиться
    scrollToAnchor(anchor);
  }

  // Смена только якоря (назад/вперёд, ссылка внутри документа).
  $effect(() => {
    const a = anchor;
    untrack(() => scrollToAnchor(a));
  });

  function scrollToAnchor(a: string | null) {
    if (!a || !body) return;
    const el = document.getElementById(anchorId(a));
    if (!el || !body.contains(el)) return;
    el.scrollIntoView({ block: 'start' });
    el.classList.remove('flash');
    void el.offsetWidth;
    el.classList.add('flash');
  }

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
    if (!raw.startsWith('#/') || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    const r = parse(raw);
    if (r.name === 'node' && r.user === user && r.node === node) {
      // тот же документ: прокрутка и адрес без перезагрузки
      go(r, { silent: true });
      scrollToAnchor(r.anchor);
    } else {
      go(r);
    }
  }

  const fmtTime = (ts: string) => {
    const d = new Date(ts);
    return Number.isNaN(d.getTime()) ? ts : d.toLocaleString('ru-RU', { dateStyle: 'medium', timeStyle: 'short' });
  };

  let crumbs = $derived(info ? info.path.split('/') : []);
  let tags = $derived.by(() => {
    const t = info?.frontmatter?.tags;
    return Array.isArray(t) ? t.filter((x): x is string => typeof x === 'string') : [];
  });
  let description = $derived.by(() => {
    const d = info?.frontmatter?.description;
    return typeof d === 'string' ? d : '';
  });
</script>

<svelte:head>
  <link rel="stylesheet" href="api/v1/render/highlight.css" />
</svelte:head>

<article class="docview">
  <header class="doc-head">
    <div class="crumbs" aria-label="Расположение">
      <span>{spaceName || space}</span>
      {#each crumbs as c, i (i)}
        <span class="sep" aria-hidden="true">/</span><span class:last={i === crumbs.length - 1}>{c}</span>
      {/each}
    </div>
    {#if info}
      <div class="meta muted">
        ревизия {info.current_rev} · изменено {fmtTime(info.updated_at)}
      </div>
      {#if tags.length || description}
        <div class="front">
          {#if description}<p class="desc">{description}</p>{/if}
          {#each tags as t (t)}<span class="chip">#{t}</span>{/each}
        </div>
      {/if}
    {/if}
    <div class="tabs" role="tablist" aria-label="Разделы заметки">
      <button type="button" role="tab" aria-selected={tab === 'doc'} onclick={() => (tab = 'doc')}>Документ</button>
      <button type="button" role="tab" aria-selected={tab === 'history'} onclick={() => (tab = 'history')}>История</button>
      <button type="button" role="tab" aria-selected={tab === 'links'} onclick={() => (tab = 'links')}>Связи</button>
    </div>
  </header>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if notice}
    <p class="warn" role="status">{notice}</p>
  {/if}

  <div role="tabpanel" hidden={tab !== 'doc'}>
    {#if loading && !rendered}
      <p class="muted">Загрузка…</p>
    {/if}
    {#if rendered}
      {#if rendered.warnings.length}
        <details class="render-warnings">
          <summary>Предупреждения рендера: {rendered.warnings.length}</summary>
          <ul>
            {#each rendered.warnings as w, i (i)}<li>{w}</li>{/each}
          </ul>
        </details>
      {/if}
      {#if rendered.toc.length >= 3}
        <details class="toc">
          <summary>Содержание</summary>
          <ul>
            {#each rendered.toc as t, i (i)}
              <li style:--lvl={t.level}>
                <a href={href({ name: 'node', user, space, node, anchor: encodeURIComponent(`h-${t.anchor}`) })}>{t.title}</a>
              </li>
            {/each}
          </ul>
        </details>
      {/if}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="doc-click" {onclick}>
        {#key rendered}
          <!-- HTML ответа сервера: санитизирован ammonia (единственное место {@html}) -->
          <div class="doc" bind:this={body}>{@html rendered.html}</div>
        {/key}
      </div>
    {/if}
  </div>
  <div role="tabpanel" hidden={tab !== 'history'}>
    <p class="muted placeholder">История ревизий появится в следующем обновлении.</p>
  </div>
  <div role="tabpanel" hidden={tab !== 'links'}>
    <p class="muted placeholder">Связи и обратные ссылки появятся в M3.</p>
  </div>
</article>

{#if lightbox}
  <Lightbox src={lightbox.src} alt={lightbox.alt} onclose={() => (lightbox = null)} />
{/if}

<style>
  .docview {
    max-width: 860px;
    margin: 0 auto;
  }
  .doc-head {
    margin-bottom: 16px;
  }
  .crumbs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    font-size: 13px;
    color: var(--muted);
  }
  .crumbs .last {
    color: var(--text);
    font-weight: 500;
  }
  .sep {
    opacity: 0.6;
  }
  .meta {
    margin-top: 2px;
    font-size: 12px;
  }
  .front {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .desc {
    flex-basis: 100%;
    margin: 0;
    color: var(--muted);
    font-style: italic;
  }
  .chip {
    font-size: 12px;
    padding: 1px 8px;
    border-radius: 10px;
    background: var(--chip);
    color: var(--muted);
  }
  .tabs {
    display: flex;
    gap: 2px;
    margin-top: 12px;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: transparent;
    padding: 6px 12px;
    color: var(--muted);
    margin-bottom: -1px;
  }
  .tabs button[aria-selected='true'] {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .placeholder {
    padding: 24px 0;
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
  .toc a:hover {
    color: var(--accent);
  }
</style>
