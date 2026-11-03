<script lang="ts">
  // Страница «Код» решения (M5.6, спецификация §11): состояние снимка карты (языки, пропуски по
  // причинам, возраст, ветка и коммит, доля других снимков, пересчёт рангов), очистка карты
  // (только владелец), поиск символа (Ctrl+K страницы), таблица файлов по рангу с фильтрами
  // (`GET /code/files` постранично), «Обзор» — Mermaid первых 40 файлов и рёбер между ними
  // (CodeOverview, свой ленивый чанк; ответ карты кэшируется по версии снимка), карточка символа по
  // постоянному адресу `путь#qual`. Вкладки — шаблон ARIA tabs (стрелки, Home, End, как у листов
  // SheetViewer).
  // Данные сервера — только текстом (никакого `{@html}`). Ленивый чанк (Shell).
  import { untrack } from 'svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import { ApiError, errorText } from '../lib/api';
  import {
    FILES_PAGE,
    SKIP_TEXT,
    ageText,
    clearCode,
    codeFiles,
    codeStatus,
    codeSymbol,
    codeSymbols,
    dropOverviewCache,
    filesText,
    overviewVersion,
    snapshotText,
    symbolsText,
    type CodeFilesPage,
    type CodeKnowledge,
    type CodeStatus,
    type CodeSymbolCard,
    type CodeSymbolHit,
  } from '../lib/code';
  import { JOURNAL_OF_KIND } from '../lib/journals';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    /** Открытая карточка символа — `путь#qual`; `null` — карта. */
    entry: string | null;
    /** Владелец решения в своей сессии: «Очистить карту». */
    owner: boolean;
    onexpired: () => void;
  }

  let { user, space, entry, owner, onexpired }: Props = $props();

  let status = $state<CodeStatus | null>(null);
  let statusError = $state('');
  type Tab = 'files' | 'overview';
  const TABS: { id: Tab; title: string }[] = [
    { id: 'files', title: 'Файлы' },
    { id: 'overview', title: 'Обзор' },
  ];
  let tab = $state<Tab>('files');
  let tabEls = $state<HTMLButtonElement[]>([]);
  /** Префикс id вкладок и панелей (`aria-controls`/`aria-labelledby`). */
  const uid = $props.id();
  let ask = $state<Ask | null>(null);
  let notice = $state('');

  // поиск символа
  let query = $state('');
  let hits = $state<CodeSymbolHit[] | null>(null);
  let searchError = $state('');
  let searchBox = $state<HTMLInputElement | null>(null);

  // таблица файлов
  let lang = $state('');
  let pathQ = $state('');
  let applied = $state({ lang: '', q: '' });
  let page = $state<CodeFilesPage | null>(null);
  let offset = $state(0);
  let filesError = $state('');

  // карточка
  let card = $state<CodeSymbolCard | null>(null);
  let cardError = $state('');

  /** Поколения ответов: карта (другое решение, очистка), таблица, поиск, карточка — у каждого
   * своё: опоздавший ответ прежнего запроса не перетирает новый. */
  let seq = 0;
  let filesSeq = 0;
  let searchSeq = 0;
  let cardSeq = 0;
  let empty = $derived(!!status && (!status.exists || status.files === 0));

  function fail(e: unknown, set: (s: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  /** Другое решение — всё заново. */
  let loadedFor = '';
  $effect(() => {
    const key = JSON.stringify([user, space]);
    if (key === loadedFor) return;
    loadedFor = key;
    untrack(() => {
      seq++;
      filesSeq++;
      searchSeq++;
      ask = null;
      status = null;
      statusError = '';
      page = null;
      filesError = '';
      hits = null;
      searchError = '';
      query = '';
      lang = '';
      pathQ = '';
      applied = { lang: '', q: '' };
      offset = 0;
      tab = 'files';
      notice = '';
      loadStatus();
    });
  });

  async function loadStatus() {
    const my = seq;
    statusError = '';
    try {
      const s = await codeStatus(user, space);
      if (my !== seq) return;
      status = s;
      if (s.exists && s.files > 0) loadFiles();
      else page = null;
    } catch (e) {
      if (my === seq) fail(e, (t) => (statusError = t));
    }
  }

  async function loadFiles() {
    const my = ++filesSeq;
    filesError = '';
    try {
      const p = await codeFiles(user, space, { lang: applied.lang, q: applied.q, offset, limit: FILES_PAGE });
      if (my === filesSeq) page = p;
    } catch (e) {
      if (my === filesSeq) fail(e, (t) => (filesError = t));
    }
  }

  function applyFilters(e?: SubmitEvent) {
    e?.preventDefault();
    applied = { lang, q: pathQ.trim() };
    offset = 0;
    loadFiles();
  }

  function pageTo(o: number) {
    offset = Math.max(0, o);
    loadFiles();
  }

  async function search(e: SubmitEvent) {
    e.preventDefault();
    const text = query.trim();
    const my = ++searchSeq;
    searchError = '';
    if (!text) {
      hits = null;
      return;
    }
    try {
      const r = await codeSymbols(user, space, text);
      if (my === searchSeq) hits = r.symbols;
    } catch (err) {
      if (my === searchSeq) fail(err, (t) => (searchError = t));
    }
  }

  // Ctrl+K на странице «Код» — поиск символа, а не общий поиск по заметкам (Shell): перехват
  // в фазе захвата, до обработчика окна Shell. Пустая карта (поиск выключен) и открытый диалог —
  // без перехвата: тогда работает общий поиск.
  $effect(() => {
    const onkey = (e: KeyboardEvent) => {
      if (empty || ask || !searchBox) return;
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        e.stopPropagation();
        searchBox?.focus();
        searchBox?.select();
      }
    };
    window.addEventListener('keydown', onkey, { capture: true });
    return () => window.removeEventListener('keydown', onkey, { capture: true });
  });

  // карточка — по адресу
  $effect(() => {
    const ref = entry;
    const u = user;
    const s = space;
    untrack(() => {
      card = null;
      cardError = '';
      if (ref) loadCard(u, s, ref);
    });
  });

  async function loadCard(u: string, s: string, ref: string) {
    const my = ++cardSeq;
    try {
      const c = await codeSymbol(u, s, ref);
      if (my === cardSeq) card = c;
    } catch (e) {
      if (my !== cardSeq) return;
      if (e instanceof ApiError && e.status === 404) {
        cardError = `Символ не найден: ${ref} — файл мог измениться или карта очищена.`;
      } else fail(e, (t) => (cardError = t));
    }
  }

  function askClear() {
    const s = status;
    if (!s) return;
    // решение — на момент вопроса: «Очистить» после смены страницы не тронет другую карту
    const u = user;
    const sp = space;
    const my = seq;
    ask = {
      title: 'Очистить карту кода?',
      message: `Будут удалены ${filesText(s.files)} и ${symbolsText(s.symbols)} карты решения; знания памяти не меняются. Карта соберётся заново при следующем zam code sync.`,
      confirm: 'Очистить',
      danger: true,
      run: async () => {
        try {
          const r = await clearCode(u, sp);
          if (my !== seq) return;
          notice = `Карта очищена: удалено ${filesText(r.removed)}.`;
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) return onexpired();
          throw new Error(errorText(e));
        }
        seq++;
        filesSeq++;
        searchSeq++;
        hits = null;
        tab = 'files';
        dropOverviewCache();
        await loadStatus();
      },
    };
  }

  /** Сводка пропусков: причина → пути (сервер отдаёт до своего предела, всего — `skipped_total`). */
  let skipGroups = $derived.by(() => {
    const g = new Map<string, string[]>();
    for (const s of status?.skipped ?? []) g.set(s.reason, [...(g.get(s.reason) ?? []), s.path]);
    return [...g.entries()].sort((a, b) => b[1].length - a[1].length);
  });

  const cardHref = (ref: string) => href({ name: 'journal', user, space, journal: 'code', entry: ref });
  const mapHref = () => href({ name: 'journal', user, space, journal: 'code', entry: null });

  /**
   * Знания карточки: у записи могут быть строки и о файле, и о символе (уникальный индекс 0023 —
   * `(…, path, COALESCE(symbol, ''))`) — остаётся одна, символьная (сервер отдаёт их первыми).
   */
  let knowledge = $derived.by(() => {
    const seen = new Set<string>();
    return (card?.knowledge ?? []).filter((k) => !seen.has(k.id) && !!seen.add(k.id));
  });

  /**
   * Ссылка на знание: заметка — документ, запись — её журнал. Пространство — текущее: сервер
   * (`knowledge_json`) пространство записи не отдаёт, и знание из Knowledge участника ведёт мимо
   * (нужен `space` в ответе карточки — журнал этапа, задача 9B).
   */
  function knowledgeHref(k: CodeKnowledge): string | null {
    const id = k.id.includes(':') ? k.id.slice(k.id.indexOf(':') + 1) : k.id;
    if (k.kind === 'note') return href({ name: 'node', user, space, node: id, anchor: null });
    const j = JOURNAL_OF_KIND[k.kind];
    return j ? href({ name: 'journal', user, space, journal: j, entry: id }) : null;
  }

  /** Клавиатура tablist: ←/→ по кругу, Home, End; выбор следует за фокусом. */
  function ontabkey(e: KeyboardEvent, i: number) {
    const n = TABS.length;
    let j = -1;
    if (e.key === 'ArrowRight') j = (i + 1) % n;
    else if (e.key === 'ArrowLeft') j = (i - 1 + n) % n;
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = n - 1;
    if (j < 0) return;
    e.preventDefault();
    tab = TABS[j].id;
    tabEls[j]?.focus();
  }

  const pct = (x: number) => `${Math.round(x * 100)} %`;
  const rankText = (r: number) => r.toFixed(4);
</script>

<section class="code-page">
  <form class="search row" role="search" onsubmit={search} aria-label="Поиск символа по карте">
    <input
      type="search"
      bind:this={searchBox}
      bind:value={query}
      placeholder="имя или Тип::метод (Ctrl+K)"
      aria-label="Поиск символа"
      disabled={empty}
    />
    <button type="submit" disabled={empty}>Найти</button>
  </form>
  {#if searchError}<p class="error" role="alert">{searchError}</p>{/if}
  {#if hits}
    <ul class="hits" aria-label="Найденные символы">
      {#each hits as h (h.id)}
        <li>
          <a href={cardHref(`${h.path}#${h.qual}`)}>{h.qual}</a>
          <span class="chip">{h.kind}</span>
          <span class="muted mono">{h.path}:{h.line}</span>
          {#if h.sig}<code class="sig-line">{h.sig}</code>{/if}
        </li>
      {/each}
    </ul>
    {#if hits.length === 0}<p class="muted">Символов не найдено.</p>{/if}
  {/if}

  {#if entry}
    <p class="back"><a href={mapHref()}>← К карте</a></p>
    {#if cardError}
      <p class="error" role="alert">{cardError}</p>
    {:else if !card}
      <p class="muted" role="status">Загрузка…</p>
    {:else}
      {@const s = card.symbol}
      <article class="code-card" aria-label="Карточка символа">
        <h2 class="mono">{s.qual}</h2>
        <div class="meta muted">
          <span class="chip">{s.kind}</span>
          <span data-testid="code-where" class="mono">{s.path}:{s.line}–{s.end_line}</span>
          {#if s.exported === true}· экспортируется{:else if s.exported === false}· внутренний{/if}
          · ранг {rankText(s.rank)}
        </div>
        {#if s.sig}
          <pre class="sig" data-testid="code-sig"><code>{s.sig}</code></pre>
        {:else}
          <p class="muted">Сигнатура не передана (code.send_signatures выключен).</p>
        {/if}

        <h3>Кто ссылается</h3>
        {#if card.callers.length}
          <ul class="plain" aria-label="Кто ссылается">
            {#each card.callers as c (c.file_id)}
              <li><span class="mono">{c.path}</span> <span class="muted">×{c.count}</span></li>
            {/each}
          </ul>
        {:else}
          <p class="muted">Ссылок из других файлов нет.</p>
        {/if}

        <h3>На что ссылается файл</h3>
        {#if card.uses.length}
          <ul class="plain" aria-label="На что ссылается">
            {#each card.uses as u (`${u.name}|${u.kind}`)}
              <li>
                <span class="mono">{u.name}</span> <span class="chip">{u.kind}</span>
                <span class="muted">×{u.count}, строка {u.first_line}</span>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="muted">Ссылок нет.</p>
        {/if}

        <h3>Привязанные знания</h3>
        {#if knowledge.length}
          <ul class="plain" aria-label="Привязанные знания">
            {#each knowledge as k (`${k.id}|${k.symbol ?? ''}`)}
              {@const link = knowledgeHref(k)}
              <li>
                {#if link}<a href={link}>{k.title}</a>{:else}{k.title}{/if}
                <span class="chip">{k.kind}</span>
                <span class="muted">{k.symbol ? 'о символе' : `о файле ${k.path}`}</span>
                {#if k.state === 'stale'}<span class="stale">возможно устарело</span>{/if}
              </li>
            {/each}
          </ul>
          {#if card.knowledge_left > 0}<p class="muted">…и ещё {card.knowledge_left}.</p>{/if}
        {:else}
          <p class="muted">Знаний о символе нет: привяжите ссылкой {s.path}#{s.qual} в refs записи или заметки.</p>
        {/if}
      </article>
    {/if}
  {:else}
    <section class="status" aria-label="Состояние карты">
      {#if statusError}
        <p class="error" role="alert">{statusError}</p>
      {:else if !status}
        <p class="muted" role="status">Загрузка…</p>
      {:else if empty}
        <p class="hint">
          Карты кода нет. Выполните <code>zam code sync</code> в каталоге решения — клиент разберёт
          рабочую копию и отправит на сервер только имена, сигнатуры и связи символов.
        </p>
      {:else}
        <p class="summary">
          <strong>{filesText(status.files)}, {symbolsText(status.symbols)}</strong>
          {#if status.age_s != null}· <span data-testid="code-age">снимок {ageText(status.age_s)}</span>{/if}
          {#if snapshotText(status)}· <span class="mono">{snapshotText(status)}</span>{/if}
          {#if status.machine}· машина <span class="mono">{status.machine}</span>{/if}
        </p>
        {#if (status.other_share ?? 0) > 0}
          <p class="warn">Файлов из других снимков: {pct(status.other_share ?? 0)} — карту обновляли с разных машин или веток.</p>
        {/if}
        {#if status.rank_state === 'pending' || status.warnings.includes('rank pending')}
          <p class="muted">Ранги пересчитываются: порядок файлов может измениться.</p>
        {/if}
        <div class="status-grid">
          <table class="langs">
            <caption>Языки</caption>
            <thead><tr><th>язык</th><th class="num">файлов</th><th class="num">символов</th></tr></thead>
            <tbody>
              {#each status.by_lang as l (l.lang)}
                <tr><td>{l.lang}</td><td class="num">{l.files}</td><td class="num">{l.symbols}</td></tr>
              {/each}
            </tbody>
          </table>
          {#if status.skipped_total > 0}
            <details class="skips" data-testid="code-skips">
              <summary>Пропущено: {status.skipped_total}</summary>
              {#each skipGroups as [reason, paths] (reason)}
                <h4>{SKIP_TEXT[reason] ?? reason} — {paths.length}</h4>
                <ul class="plain">
                  {#each paths as p (p)}<li><span class="mono">{p}</span> <span class="muted">{SKIP_TEXT[reason] ?? reason}</span></li>{/each}
                </ul>
              {/each}
              {#if status.skipped_total > status.skipped.length}
                <p class="muted">…показаны {status.skipped.length} из {status.skipped_total}.</p>
              {/if}
            </details>
          {/if}
        </div>
        {#if owner}
          <button type="button" class="danger" onclick={askClear}>Очистить карту</button>
        {/if}
      {/if}
      {#if notice}<p class="ok" role="status">{notice}</p>{/if}
    </section>

    {#if status && !empty}
      <div class="tabs" role="tablist" aria-label="Вид карты">
        {#each TABS as t, i (t.id)}
          <button
            type="button"
            role="tab"
            id={`${uid}-tab-${t.id}`}
            bind:this={tabEls[i]}
            aria-selected={tab === t.id}
            aria-controls={`${uid}-panel-${t.id}`}
            tabindex={tab === t.id ? 0 : -1}
            onclick={() => (tab = t.id)}
            onkeydown={(e) => ontabkey(e, i)}>{t.title}</button
          >
        {/each}
      </div>

      <div role="tabpanel" id={`${uid}-panel-files`} aria-labelledby={`${uid}-tab-files`} hidden={tab !== 'files'}>
        <form class="filters row" onsubmit={applyFilters} aria-label="Фильтры файлов">
          <label class="row">
            Язык
            <select bind:value={lang} onchange={() => applyFilters()}>
              <option value="">все</option>
              {#each status.by_lang as l (l.lang)}<option value={l.lang}>{l.lang}</option>{/each}
            </select>
          </label>
          <label class="row grow">
            Путь содержит
            <input bind:value={pathQ} placeholder="например, src/store" />
          </label>
          <button type="submit">Применить</button>
        </form>
        {#if filesError}<p class="error" role="alert">{filesError}</p>{/if}
        {#if page}
          <table class="files">
            <caption>Файлы карты</caption>
            <thead>
              <tr><th class="num">№</th><th>путь</th><th>язык</th><th class="num">ранг</th><th class="num">символов</th></tr>
            </thead>
            <tbody>
              {#each page.files as f, i (f.id)}
                <tr>
                  <td class="num muted">{page.offset + i + 1}</td>
                  <td class="mono path">{f.path}</td>
                  <td>{f.lang}</td>
                  <td class="num rank">{rankText(f.rank)}</td>
                  <td class="num">{f.symbols}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          {#if page.total === 0}<p class="muted">Файлов по фильтру нет.</p>{/if}
          <div class="pager row">
            <span class="muted" data-testid="code-files-total">
              {page.total ? `${page.offset + 1}–${page.offset + page.files.length}` : '0'} из {page.total}
            </span>
            {#if page.offset > 0}<button type="button" onclick={() => pageTo(page!.offset - page!.limit)}>Назад</button>{/if}
            {#if page.offset + page.files.length < page.total}
              <button type="button" onclick={() => pageTo(page!.offset + page!.limit)}>Далее</button>
            {/if}
          </div>
        {/if}
      </div>
      <div role="tabpanel" id={`${uid}-panel-overview`} aria-labelledby={`${uid}-tab-overview`} hidden={tab !== 'overview'}>
        {#if tab === 'overview'}
          <!-- «Обзор» — свой ленивый чанк (Mermaid подгружается уже из него) -->
          {#await import('./CodeOverview.svelte')}
            <p class="muted" role="status">Загрузка…</p>
          {:then { default: CodeOverview }}
            {#key `${user}|${space}`}<CodeOverview {user} {space} version={overviewVersion(status)} {onexpired} />{/key}
          {:catch e}
            <p class="error" role="alert">Обзор не загрузился: {errorText(e)}</p>
          {/await}
        {/if}
      </div>
    {/if}
  {/if}
</section>

<Prompt bind:ask />

<style>
  .code-page {
    max-width: 1100px;
  }
  .search {
    margin-bottom: 12px;
  }
  .search input {
    flex: 1 1 280px;
    min-width: 0;
  }
  .hits {
    margin: 0 0 16px;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 4px;
  }
  .hits li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .sig-line {
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .mono {
    font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .chip {
    display: inline-block;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .status,
  .code-card {
    padding: 12px 16px;
    margin-bottom: 16px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .summary {
    margin: 0 0 8px;
  }
  .status-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 24px;
    align-items: flex-start;
    margin-bottom: 12px;
  }
  table {
    border-collapse: collapse;
  }
  caption {
    text-align: left;
    font-weight: 600;
    padding-bottom: 4px;
  }
  th,
  td {
    padding: 3px 10px 3px 0;
    text-align: left;
    border-bottom: 1px solid var(--border);
  }
  th {
    font-weight: 500;
    color: var(--muted);
    font-size: 13px;
  }
  .num {
    text-align: right;
  }
  .files {
    width: 100%;
  }
  .files .path {
    width: 60%;
  }
  .skips h4 {
    margin: 8px 0 2px;
    font-size: 13px;
  }
  .plain {
    margin: 0 0 8px;
    padding-left: 18px;
  }
  .hint code {
    background: var(--code-bg);
    border-radius: 4px;
    padding: 1px 4px;
  }
  .tabs {
    display: flex;
    gap: 4px;
    margin-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    border: none;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: none;
  }
  .tabs button[aria-selected='true'] {
    border-bottom-color: var(--accent);
    color: var(--accent);
  }
  .filters {
    flex-wrap: wrap;
    margin-bottom: 10px;
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
  .pager {
    margin-top: 8px;
  }
  .code-card h2 {
    margin: 0 0 4px;
    font-size: 18px;
  }
  .code-card h3 {
    margin: 14px 0 4px;
    font-size: 14px;
  }
  .meta {
    font-size: 13px;
  }
  .sig {
    margin: 10px 0 0;
    padding: 8px 10px;
    background: var(--code-bg);
    border-radius: 6px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .stale {
    color: var(--warn-text);
    background: var(--warn-bg);
    border-radius: 4px;
    padding: 0 4px;
    font-size: 12px;
  }
  .back {
    margin: 0 0 8px;
    font-size: 13px;
  }
  /* печать: страница карты без навигации, дерева, форм и кнопок; цвета светлые (тёмные
     токены app.css — только для screen) */
  @media print {
    :global(body:has(.code-page)) {
      background: #fff;
    }
    :global(body:has(.code-page) .topbar),
    :global(body:has(.code-page) .sidebar),
    :global(body:has(.code-page) nav),
    :global(body:has(.code-page) button),
    .search,
    .filters,
    .back,
    .pager,
    .tabs {
      display: none !important;
    }
    .status,
    .code-card {
      background: #fff;
      border-color: #d9dde3;
      break-inside: avoid;
    }
    .sig {
      background: #f3f4f7;
    }
    .skips {
      display: block;
    }
  }
</style>
