<script lang="ts">
  // Страница «Индекс» (#/admin/index): состояние векторного индекса по GET /index/status
  // (обновление раз в 5 с, пока страница открыта и вкладка видна; при возвращении на
  // вкладку — сразу), причина деградации с действием (тексты — с сервера),
  // повреждённые и исключённые из пересборки базы, неудавшиеся задания и переиндексация
  // (POST /admin/reindex) с подтверждением; баннер перенарезки баз после смены настроек
  // нарезки («Идёт перенарезка: изменились <ключи>, N из M», §8.2).
  import { Dialog } from 'bits-ui';
  import { onMount } from 'svelte';
  import { ApiError, indexStatus, reindex, type IndexStatus, type User } from '../lib/api';
  import { hintParts, reasonText, transient } from '../lib/indexing';
  import { href } from '../lib/router';

  let { users, onexpired }: { users: User[]; onexpired: () => void } = $props();

  const REFRESH_MS = 5000;

  let st = $state<IndexStatus | null>(null);
  let error = $state('');
  let updated = $state<Date | null>(null);
  /** Подтверждение переиндексации: `full` — полная, иначе из кэша эмбеддингов. */
  let confirm = $state<{ full: boolean } | null>(null);
  let sending = $state(false);
  let reindexMsg = $state('');
  let reindexErr = $state('');
  /** Поколение запроса статуса: ответ устаревшего запроса (таймер до POST) не затирает свежий. */
  let seq = 0;

  const QDRANT_STATE: Record<string, string> = {
    off: 'не запущен',
    starting: 'запускается',
    ready: 'работает',
    restarting: 'перезапускается',
    broken: 'выключен после сбоев',
    external: 'внешний',
  };

  const userName = (id: string) => users.find((u) => u.id === id)?.name ?? id;

  const fmtTime = (ts: string | null | undefined) => {
    if (!ts) return '—';
    const d = new Date(ts);
    return Number.isNaN(d.getTime()) ? ts : d.toLocaleString('ru-RU');
  };

  const fmtBytes = (n: number | null) => (n === null ? '—' : `${(n / 1048576).toFixed(0)} МБ`);
  const num = (n: number | null | undefined) => (n === null || n === undefined ? '—' : n.toLocaleString('ru-RU'));

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
      onexpired();
      return true;
    }
    return false;
  }

  async function load() {
    const my = ++seq;
    try {
      const r = await indexStatus();
      if (my !== seq) return;
      st = r;
      updated = new Date();
      error = '';
    } catch (e) {
      if (my !== seq) return;
      if (expired(e)) return;
      error = e instanceof Error ? e.message : 'Ошибка';
    }
  }

  onMount(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // следующий запрос — после ответа на предыдущий: медленный сервер не копит запросы;
    // скрытая вкладка не опрашивает сервер
    const tick = async () => {
      clearTimeout(timer);
      timer = undefined;
      await load();
      if (alive && !document.hidden && timer === undefined) timer = setTimeout(tick, REFRESH_MS);
    };
    const onvis = () => {
      if (document.hidden) {
        clearTimeout(timer);
        timer = undefined;
      } else {
        void tick();
      }
    };
    document.addEventListener('visibilitychange', onvis);
    void tick();
    return () => {
      alive = false;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onvis);
    };
  });

  async function run() {
    if (!confirm) return;
    const full = confirm.full;
    sending = true;
    reindexMsg = '';
    reindexErr = '';
    try {
      const r = await reindex(full);
      reindexMsg =
        `${full ? 'Полная переиндексация' : 'Переиндексация из embeddings'} поставлена: заметок — ${num(r.job_count)}.` +
        (r.unavailable_dbs.length ? ` Пропущено повреждённых баз: ${r.unavailable_dbs.length}.` : '');
      confirm = null;
      await load();
    } catch (e) {
      if (expired(e)) return;
      reindexErr = e instanceof Error ? e.message : 'Ошибка';
      confirm = null;
    } finally {
      sending = false;
    }
  }

  let busyReindex = $derived(!!st && (st.reindex.running || st.reindex.queued));
  /** Базы с идущей перенарезкой (старый сервер без `dbs` — нет). */
  let rechunks = $derived(
    (st?.dbs ?? []).flatMap((d) => (d.rechunk ? [{ user_id: d.user_id, ...d.rechunk }] : [])),
  );
  const changedText = (keys: string[]) => (keys.length ? keys.join(', ') : 'параметры нарезки');
</script>

<section class="panel index-page">
  <div class="head">
    <h2>Векторный индекс</h2>
    <span class="muted small">
      {#if updated}обновлено {updated.toLocaleTimeString('ru-RU')}{:else}загрузка…{/if}
    </span>
  </div>
  {#if error}<p class="error" role="alert">Не удалось получить состояние: {error}</p>{/if}

  {#if st}
    {#if !st.vector.available && st.vector.reason}
      <div class="alert" class:alert-warn={transient(st.vector.reason)}>
        <p class="alert-title">Векторный поиск недоступен: {reasonText(st.vector.reason, st.vector.text)}</p>
        <p>Поиск работает в режиме полнотекстового (FTS), запись заметок не затронута.</p>
        {#if st.vector.hint}
          <p class="hint">
            Что сделать: {#each hintParts(st.vector.hint) as p, i (i)}{#if p.code}<code class="cmd">{p.t}</code>{:else}{p.t}{/if}{/each}
          </p>
        {/if}
        {#if st.model.error || st.qdrant.error}
          <details>
            <summary>Подробности</summary>
            {#if st.model.error}<p class="detail">Модель: {st.model.error}</p>{/if}
            {#if st.qdrant.error}<p class="detail">Qdrant: {st.qdrant.error}</p>{/if}
          </details>
        {/if}
      </div>
    {:else if st.vector.available}
      <p class="ok">Векторный поиск работает.</p>
    {/if}

    <dl class="grid">
      <div>
        <dt>Модель</dt>
        <dd>
          {#if st.model.id}<code>{st.model.id}</code>, размерность {st.model.dim}{:else}не загружена{/if}
          {#if st.model.collection_model && st.model.collection_model !== st.model.id}
            <div class="muted small">в коллекции: <code>{st.model.collection_model}</code></div>
          {/if}
        </dd>
      </div>
      <div>
        <dt>Устройство</dt>
        <dd>
          {st.device.kind === 'cuda' ? 'GPU (CUDA)' : st.device.kind === 'cpu' ? 'CPU' : '—'}
          {#if st.device.reason && st.device.kind}<div class="muted small">{st.device.reason}</div>{/if}
        </dd>
      </div>
      <div>
        <dt>Qdrant</dt>
        <dd>
          {QDRANT_STATE[st.qdrant.state] ?? st.qdrant.state}{#if st.qdrant.version}, версия {st.qdrant.version}{/if}
          <div class="muted small">
            <span class="kv">точек: {num(st.qdrant.points)}</span> · <span class="kv">память: {fmtBytes(st.qdrant.rss_bytes)}</span> ·
            <span class="kv">перезапусков: {st.qdrant.restarts}</span>
          </div>
        </dd>
      </div>
      <div>
        <dt>Очередь индексации</dt>
        <dd>
          <span class="kv">в очереди: {num(st.queue.queued)}</span> · <span class="kv">в работе: {num(st.queue.running)}</span> ·
          <span class="kv">с ошибкой: {num(st.queue.failed)}</span>
          {#if st.queue.oldest}<div class="muted small">старейшее задание: {fmtTime(st.queue.oldest)}</div>{/if}
        </dd>
      </div>
    </dl>

    {#each rechunks as r (r.user_id)}
      <div class="alert alert-warn rechunk" role="status">
        <p class="alert-title">
          Идёт перенарезка: изменились {changedText(r.changed)}, {num(r.done)} из {num(r.total)}
        </p>
        <p>
          База <b>{userName(r.user_id)}</b> перенарезается в фоне после смены настроек нарезки:
          поиск и запись работают, векторы пересчитываются только у изменившихся фрагментов.
        </p>
      </div>
    {/each}

    {#if st.unavailable_dbs.length}
      <div class="alert">
        <p class="alert-title">Базы пользователей не открываются: {st.unavailable_dbs.length}</p>
        <p>
          Файл базы повреждён или недоступен: её очередь не учтена в статусе, переиндексация её
          пропустит. Восстановите базу из резервной копии (или копии
          <code>users/&lt;id&gt;.db.bak-v*</code>) и перезапустите сервер — переиндексация из
          кэша поставится сама.
        </p>
        <ul class="dbs">
          {#each st.unavailable_dbs as d (d.user_id)}
            <li><b>{userName(d.user_id)}</b> <span class="muted">({d.user_id})</span>: {d.error}</li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if st.rebuild_skipped.length}
      <div class="alert alert-warn">
        <p class="alert-title">Исключены из пересборки индекса: {st.rebuild_skipped.length}</p>
        <p>
          Эти базы не открылись при пересборке коллекции, их заметки не ищутся векторным поиском.
          Когда база снова откроется (восстановлена из копии), сервер сам переиндексирует её и
          снимет исключение.
        </p>
        <ul class="dbs">
          {#each st.rebuild_skipped as d (d.user_id)}
            <li>
              <b>{userName(d.user_id)}</b> <span class="muted">({d.user_id})</span>: {d.error}
              <span class="muted">— попыток: {d.attempts}</span>
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    <h3>Переиндексация</h3>
    <p class="muted small">
      «Из embeddings» — пересборка точек Qdrant из кэша векторов без модели (быстро; нужна
      после смены модели, потери хранилища, восстановления базы). «Полная» — перечанковка всех
      заметок и задания модели для текстов без кэша.
    </p>
    {#if st.reindex.running}
      <p class="warn" role="status">Переиндексация выполняется…</p>
    {:else if st.reindex.queued}
      <p class="warn" role="status">Переиндексация поставлена в очередь…</p>
    {/if}
    <div class="row">
      <button type="button" disabled={busyReindex || sending} onclick={() => (confirm = { full: false })}>
        Переиндексировать из embeddings
      </button>
      <button type="button" class="danger" disabled={busyReindex || sending} onclick={() => (confirm = { full: true })}>
        Полная переиндексация
      </button>
    </div>
    {#if reindexMsg}<p class="ok" role="status">{reindexMsg}</p>{/if}
    {#if reindexErr}<p class="error" role="alert">{reindexErr}</p>{/if}
    {#if st.reindex.last}
      {@const l = st.reindex.last}
      <p class="small last">
        Последняя: {l.full ? 'полная' : 'из embeddings'}, завершена {fmtTime(l.finished_at)}
        {#if l.result}
          — заметок {num(l.result.nodes)}, точек {num(l.result.points)}, промахов кэша
          {num(l.result.misses)}, заданий {num(l.result.jobs)}, {(l.result.elapsed_ms / 1000).toFixed(1)} с
          {#if l.result.failed_users.length}<span class="error-text">; баз с ошибкой: {l.result.failed_users.length}</span>{/if}
        {:else if l.error}
          — <span class="error-text">ошибка: {l.error}</span>
        {/if}
      </p>
    {/if}

    {#if st.failed_items.length}
      <h3>Неудавшиеся задания</h3>
      <div class="scroll">
        <table>
          <thead><tr><th>Пользователь</th><th>Заметка</th><th>Операция</th><th>Ошибка</th><th>Попыток</th><th>Когда</th></tr></thead>
          <tbody>
            {#each st.failed_items as f (`${f.user_id}:${f.id}`)}
              <tr>
                <td>{userName(f.user_id)}</td>
                <td>
                  {#if f.space_id && f.path}
                    <a href={href({ name: 'node', user: f.user_id, space: f.space_id, node: f.node_id, anchor: null })}>{f.path}</a>
                    {#if f.deleted_at}<span class="muted">(в корзине)</span>{/if}
                  {:else}
                    <code>{f.node_id}</code> <span class="muted">(удалена)</span>
                  {/if}
                </td>
                <td>{f.op}</td>
                <td class="err">{f.error}</td>
                <td>{f.attempts}</td>
                <td>{fmtTime(f.updated_at)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
</section>

<Dialog.Root
  open={confirm !== null}
  onOpenChange={(o) => {
    if (!o && !sending) confirm = null;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog">
      <Dialog.Title>{confirm?.full ? 'Полная переиндексация' : 'Переиндексация из embeddings'}</Dialog.Title>
      <Dialog.Description class="warn">
        {#if confirm?.full}
          Все заметки всех пользователей будут перечанкованы и поставлены в очередь; для текстов
          без кэша работает модель — это может занять долгое время. Векторный поиск на это время
          может возвращать неполные результаты.
        {:else}
          Точки Qdrant всех пользователей будут удалены и записаны заново из кэша эмбеддингов (без
          модели). На время пересборки векторный поиск может возвращать неполные результаты.
        {/if}
      </Dialog.Description>
      <div class="row" style="margin-top: 12px;">
        <button type="button" class={confirm?.full ? 'danger-fill' : 'primary'} disabled={sending} onclick={run}>Запустить</button>
        <Dialog.Close disabled={sending}>Отмена</Dialog.Close>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }
  h3 {
    margin: 20px 0 8px;
    font-size: 15px;
    font-weight: 600;
  }
  .small {
    font-size: 13px;
  }
  .alert {
    margin: 12px 0;
    padding: 10px 12px;
    border: 1px solid var(--danger);
    border-left-width: 4px;
    border-radius: 6px;
  }
  .alert.alert-warn {
    border-color: var(--callout-warning);
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .alert p {
    margin: 4px 0;
  }
  .alert-title {
    font-weight: 600;
  }
  .alert details {
    margin-top: 4px;
    font-size: 13px;
  }
  .detail {
    overflow-wrap: anywhere;
  }
  .cmd {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--code-bg);
    color: var(--text);
  }
  .kv {
    white-space: nowrap;
  }
  .hint {
    overflow-wrap: anywhere;
  }
  .ok {
    margin: 12px 0;
    color: var(--callout-tip);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 12px 24px;
    margin: 16px 0 0;
  }
  .grid dt {
    font-size: 12px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .grid dd {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
  }
  .dbs {
    margin: 6px 0 0;
    padding-left: 20px;
    overflow-wrap: anywhere;
  }
  .last {
    margin-top: 8px;
  }
  .error-text {
    color: var(--danger);
  }
  .scroll {
    overflow-x: auto;
  }
  .err {
    overflow-wrap: anywhere;
    min-width: 200px;
  }
</style>
