<script lang="ts">
  // Глобальный поиск (Ctrl+K): POST /search по всем пространствам пользователя
  // (Human — для администратора), подсветка совпадений текстовыми узлами и <mark>,
  // метки каналов (`vector`, `fts`), строка деградации и предупреждения сервера,
  // переход к заметке и разделу.
  import { Dialog } from 'bits-ui';
  import { untrack } from 'svelte';
  import { ApiError, getOutline, listSpaces, search, spaceRef, type SearchItem } from '../lib/api';
  import { reasonText, warningText } from '../lib/indexing';
  import { go } from '../lib/router';

  let {
    open = $bindable(),
    user,
    onexpired,
  }: { open: boolean; user: string; onexpired: () => void } = $props();

  const DEBOUNCE_MS = 200;
  const SOURCE_TITLE: Record<string, string> = {
    vector: 'Найдено векторным (смысловым) поиском',
    fts: 'Найдено полнотекстовым поиском',
    trigram: 'Найдено поиском по подстроке',
  };
  const MARK_START = '';
  const MARK_END = '';

  let query = $state('');
  let items = $state<SearchItem[]>([]);
  let active = $state(0);
  let error = $state('');
  let busy = $state(false);
  let searched = $state(false);
  /** Векторный канал выключен: причина по-русски, иначе `null`. */
  let degraded = $state<string | null>(null);
  let warnings = $state<string[]>([]);
  let field = $state<HTMLInputElement | null>(null);

  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Поиск назначен (debounce ещё идёт). */
  let pending = false;
  /** Последний запущенный поиск. */
  let inflight: Promise<void> = Promise.resolve();
  /** Пространства пользователя (id): читаются при первом поиске для этого пользователя. */
  let spaces: { user: string; ids: string[] } | null = null;

  // Новое открытие — свежая выдача по прежнему запросу.
  $effect(() => {
    if (open) untrack(() => schedule(0));
  });

  function oninput() {
    schedule(DEBOUNCE_MS);
  }

  function schedule(delay: number) {
    clearTimeout(timer);
    pending = true;
    timer = setTimeout(start, delay);
  }

  function start() {
    clearTimeout(timer);
    pending = false;
    inflight = run();
  }

  /** Enter: выдача строго по введённому запросу (debounce не дожидается). */
  async function enter() {
    if (pending) start();
    const my = seq;
    await inflight;
    // за время ожидания запрос изменили — Enter относится к устаревшему
    if (my !== seq) return;
    void choose(items[active]);
  }

  async function run() {
    const my = ++seq;
    const qtext = query.trim();
    error = '';
    if (!qtext) {
      items = [];
      searched = false;
      degraded = null;
      warnings = [];
      busy = false;
      return;
    }
    busy = true;
    try {
      let ids = spaces?.user === user ? spaces.ids : null;
      if (!ids) {
        ids = (await listSpaces(user)).spaces.map((x) => x.id);
        spaces = { user, ids };
      }
      const r = await search(user, qtext, ids);
      if (my !== seq) return;
      items = r.items;
      degraded = r.degraded ? reasonText(r.degraded_reason, r.degraded_text) : null;
      warnings = r.warnings ?? [];
      active = 0;
      searched = true;
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) {
        open = false;
        return onexpired();
      }
      error = e instanceof Error ? e.message : 'Ошибка';
    } finally {
      if (my === seq) busy = false;
    }
  }

  /** Сниппет → фрагменты; `m` — совпадение (между U+E000 и U+E001). */
  function parts(s: string): { t: string; m: boolean }[] {
    const out: { t: string; m: boolean }[] = [];
    let m = false;
    let buf = '';
    for (const ch of s) {
      if (ch === MARK_START || ch === MARK_END) {
        if (buf) out.push({ t: buf, m });
        buf = '';
        m = ch === MARK_START;
      } else {
        buf += ch;
      }
    }
    if (buf) out.push({ t: buf, m });
    return out;
  }

  /** Якорь раздела по `heading_path` (оглавление сервера: точный путь, иначе последний заголовок). */
  async function anchorFor(it: SearchItem): Promise<string | null> {
    if (!it.heading_path) return null;
    try {
      const toc = await getOutline(user, it.node_id);
      const segs = it.heading_path.split(' > ');
      const exact = toc.find((t) => t.path.join(' > ') === it.heading_path);
      const last = segs[segs.length - 1];
      const hit = exact ?? toc.find((t) => t.title === last);
      return hit ? encodeURIComponent(`h-${hit.anchor}`) : null;
    } catch {
      // без оглавления — просто документ
      return null;
    }
  }

  async function choose(it: SearchItem | undefined) {
    if (!it || !it.space) return;
    const anchor = await anchorFor(it);
    open = false;
    go({ name: 'node', user, space: spaceRef(it.space), node: it.node_id, anchor });
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (items.length) active = (active + 1) % items.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (items.length) active = (active - 1 + items.length) % items.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      void enter();
    }
  }

  // Активный элемент — в видимой части списка.
  $effect(() => {
    const i = active;
    if (open && items.length) document.getElementById(`search-opt-${i}`)?.scrollIntoView({ block: 'nearest' });
  });
</script>

<Dialog.Root bind:open>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content
      class="panel dialog search"
      onOpenAutoFocus={(e) => {
        if (field) {
          e.preventDefault();
          field.focus();
          field.select();
        }
      }}
    >
      <Dialog.Title class="sr-title">Поиск по заметкам</Dialog.Title>
      <input
        bind:this={field}
        bind:value={query}
        {oninput}
        {onkeydown}
        class="q"
        type="search"
        placeholder="Поиск по заметкам…"
        aria-label="Поиск по заметкам"
        role="combobox"
        aria-expanded={items.length > 0}
        aria-controls="search-results"
        aria-activedescendant={items.length ? `search-opt-${active}` : undefined}
        autocomplete="off"
      />
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      {#if searched && query.trim() && (degraded !== null || warnings.length)}
        <div class="notices" role="status">
          {#if degraded !== null}
            <p class="warn degraded">
              Векторный поиск недоступен: {degraded} — показаны результаты полнотекстового поиска
            </p>
          {/if}
          {#each warnings as w, i (i)}<p class="warn">Предупреждение: {warningText(w)}</p>{/each}
        </div>
      {/if}
      {#if items.length}
        <ul id="search-results" class="results" role="listbox" aria-label="Результаты поиска">
          {#each items as it, i (`${it.node_id}:${i}`)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <li
              id={`search-opt-${i}`}
              role="option"
              aria-selected={i === active}
              class:active={i === active}
              onclick={() => choose(it)}
              onmousemove={() => (active = i)}
            >
              <div class="where">
                <span class="space">{it.space?.name ?? '?'}</span>{` / ${it.path}`}{#if it.heading_path}<span class="section">{` › ${it.heading_path}`}</span>{/if}
                {#each it.sources ?? [] as src (src)}<span class="src" title={SOURCE_TITLE[src] ?? src}>{src}</span>{/each}
              </div>
              <div class="snippet">
                {#each parts(it.snippet ?? '') as p, j (j)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
              </div>
            </li>
          {/each}
        </ul>
      {:else if searched && !busy && query.trim()}
        <p class="muted empty">Ничего не найдено.</p>
      {/if}
      <p class="muted hint">↑ ↓ — выбор, Enter — открыть, Esc — закрыть</p>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  :global(.dialog.search) {
    top: 12vh;
    transform: translateX(-50%);
    width: min(680px, 94vw);
    padding: 12px;
  }
  :global(.sr-title) {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .q {
    width: 100%;
    font-size: 16px;
    padding: 8px 12px;
  }
  .results {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    max-height: 60vh;
    overflow: auto;
  }
  .results li {
    padding: 8px 10px;
    border-radius: 6px;
    cursor: pointer;
  }
  .results li.active {
    background: var(--selected);
  }
  .where {
    font-size: 13px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .space {
    color: var(--text);
    font-weight: 500;
  }
  .snippet {
    margin-top: 2px;
    font-size: 14px;
    overflow-wrap: anywhere;
  }
  .snippet mark {
    background: var(--flash);
    color: inherit;
    border-radius: 2px;
  }
  .notices {
    display: grid;
    gap: 4px;
    margin-top: 8px;
  }
  .notices p {
    margin: 0;
    font-size: 13px;
  }
  .src {
    display: inline-block;
    margin-left: 6px;
    padding: 0 5px;
    font: 11px/16px ui-monospace, Consolas, monospace;
    border-radius: 4px;
    background: var(--chip);
    color: var(--muted);
    vertical-align: 1px;
  }
  .empty {
    margin: 12px 4px 4px;
  }
  .hint {
    margin: 8px 4px 0;
    font-size: 12px;
  }
</style>
