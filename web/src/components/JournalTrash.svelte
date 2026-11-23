<script lang="ts">
  // «Корзина» журнала (M7.2, спецификация §5.9 п. 3): удалённые записи вида `kind` пространства
  // (`GET …/entries?kind=<вид>&deleted=only` — новые удаления первыми, по 50 с «Показать ещё») и
  // «Восстановить» (`POST /entries/{id}/restore`): запись уходит из корзины, журнал перечитывает
  // живую ленту (`onrestored`). Права — как у восстановления вида: участнику `ro` сервер отвечает 403
  // — текстом ошибки здесь. Своя корзина — у «Отчётов» (`ReportsView`), у Vault корзины в WebUI нет.
  // Ленивый чанк: грузится при первом нажатии «Корзина» журнала.
  import { tick, untrack } from 'svelte';
  import { ApiError, errorText, listEntries, restoreEntry, type Entry, type EntryKind } from '../lib/api';
  import { AUTHOR_TEXT, entryStatusText, fmtTime } from '../lib/journals';

  interface Props {
    user: string;
    space: string;
    kind: EntryKind;
    /** Запись восстановлена: журнал перечитывает живую ленту. */
    onrestored: () => void;
    /** 401: сессия кончилась. */
    onexpired?: () => void;
  }

  let { user, space, kind, onrestored, onexpired }: Props = $props();

  const PAGE = 50;

  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let notice = $state('');
  /** id восстанавливаемой записи. */
  let busy = $state('');
  let seq = 0;

  // корзина — заново при смене пользователя, пространства или вида
  $effect(() => {
    const u = user;
    const s = space;
    const k = kind;
    untrack(() => {
      notice = '';
      load(u, s, k, true);
    });
  });

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired?.();
    error = errorText(e);
  }

  async function load(u: string, s: string, k: EntryKind, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, { kind: k, deleted: true, cursor: reset ? null : next, limit: PAGE });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) {
        items = [];
        next = null;
      }
      fail(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function restore(e: Entry, btn: HTMLElement) {
    if (busy) return;
    busy = e.id;
    error = '';
    notice = '';
    // ответ — к той же корзине: за время запроса её могли перечитать (другой вид, пространство)
    const my = seq;
    const i = items.findIndex((x) => x.id === e.id);
    try {
      await restoreEntry(user, e.id);
      if (my !== seq) return;
      items = items.filter((x) => x.id !== e.id);
      notice = `Восстановлено: «${e.title}» — снова в журнале.`;
      onrestored();
      // кнопка ушла вместе со строкой: фокус — на соседнюю строку, иначе на итог
      await tick();
      if (!btn.isConnected) {
        const row = root?.querySelectorAll<HTMLElement>('[data-trash] button')[Math.min(i, items.length - 1)];
        (row ?? status)?.focus();
      }
    } catch (err) {
      if (my !== seq) return;
      fail(err);
    } finally {
      busy = '';
    }
  }

  let root = $state<HTMLElement | null>(null);
  let status = $state<HTMLElement | null>(null);
</script>

<section class="trash" aria-label="Корзина" bind:this={root}>
  <p class="muted small">Корзина: удалённые записи журнала, новые удаления первыми; «Восстановить» возвращает запись.</p>
  <p class="muted" role="status" tabindex="-1" bind:this={status}>{notice}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="feed" role="feed" aria-label="Удалённые записи" aria-busy={loading}>
    {#each items as e (e.id)}
      <article class="item" data-trash={e.id}>
        <div class="text">
          <h3>{e.title}</h3>
          <div class="meta muted">
            удалено <time datetime={e.deleted_at ?? ''}>{fmtTime(e.deleted_at)}</time>
            {#if e.status}· <span class="chip">{entryStatusText(e.kind, e.status)}</span>{/if}
            · {AUTHOR_TEXT[e.author.kind] ?? e.author.kind}
            {#if e.area}· область <span class="chip">{e.area}</span>{/if}
          </div>
        </div>
        <!-- имя — с заголовком записи: в списке кнопок экранного диктора строки различимы -->
        <button
          type="button"
          aria-label="Восстановить «{e.title}»"
          disabled={busy !== ''}
          onclick={(ev) => restore(e, ev.currentTarget as HTMLElement)}>Восстановить</button
        >
      </article>
    {/each}
  </div>
  {#if !loading && items.length === 0 && !error}<p class="muted">Корзина пуста.</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if next && !loading}
    <button type="button" onclick={() => load(user, space, kind, false)}>Показать ещё</button>
  {/if}
</section>

<style>
  .feed {
    display: grid;
    gap: 8px;
  }
  .item {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 14px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .text {
    flex: 1 1 220px;
    min-width: 0;
  }
  .item h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .meta {
    font-size: 12px;
    margin-top: 2px;
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .small {
    font-size: 12px;
  }
  /* живая область остаётся в документе (объявление итога), пустая — без отступа */
  [role='status']:empty {
    margin: 0;
  }
</style>
