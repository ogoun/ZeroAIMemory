<script lang="ts">
  // «Администрирование → Копии» (#/admin/backups; M7.2, спецификация M7 §5.6 п. 2; ленивый чанк):
  // последняя копия (когда и «N дн. назад»), расписание автокопий с местным временем срока или
  // «выключено» с подсказкой ключа, `no_recent_backup` текстом; «Снять копию сейчас» — копия в
  // процессе сервера (`POST /admin/backups`, подтверждение; пока идёт — «идёт с ЧЧ:ММ», опрос
  // `GET /admin/backups` раз в 3 с, скрытая вкладка не опрашивает; итог — строкой отчёта); список
  // готовых копий каталога копий (дата, размер, баз, блобов, версия сервера, расхождения снимков)
  // с «Удалить» (подтверждение); на телефоне — карточками строк. Блок «Восстановление» — порядок
  // из руководства: восстановления и скачивания копии из WebUI нет (решение координатора 24).
  import { Dialog } from 'bits-ui';
  import { onMount } from 'svelte';
  import { ApiError, deleteBackup, errorText, listBackups, startBackup, type BackupCopy, type Backups } from '../lib/api';
  import {
    BACKUP_POLL_MS,
    bannerText,
    byText,
    bytesText,
    dateTime,
    hhmm,
    reportLine,
    scheduleText,
    sinceText,
  } from '../lib/backups';
  import { viewport } from '../lib/viewport.svelte';

  let { onexpired }: { onexpired: () => void } = $props();

  /** Раздел руководства о переносе и восстановлении (тот же файл лежит в комплекте сервера). */
  const GUIDE_URL =
    'https://github.com/ogoun/ZeroAIMemory/blob/main/deploy/README-install.md#перенос-на-другую-машину';

  let st = $state<Backups | null>(null);
  let error = $state('');
  /** Время ответа: давность считается от него, а не от часов отрисовки. */
  let now = $state(Date.now());
  let confirmRun = $state(false);
  let confirmDel = $state<BackupCopy | null>(null);
  let sending = $state(false);
  let actionMsg = $state('');
  let actionErr = $state('');
  /** Поколение запроса: ответ устаревшего опроса не затирает свежий. */
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = true;

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
      onexpired();
      return true;
    }
    return false;
  }

  /** Пока идёт копия — следующий опрос через 3 с после ответа (медленный сервер не копит
   * запросы); скрытая вкладка не опрашивает. */
  function plan() {
    clearTimeout(timer);
    timer = undefined;
    if (alive && st?.running && !document.hidden) timer = setTimeout(load, BACKUP_POLL_MS);
  }

  async function load() {
    const my = ++seq;
    try {
      const r = await listBackups();
      if (my !== seq) return;
      st = r;
      now = Date.now();
      error = '';
    } catch (e) {
      if (my !== seq) return;
      if (expired(e)) return;
      error = errorText(e);
    }
    plan();
  }

  onMount(() => {
    const onvis = () => {
      if (document.hidden) {
        clearTimeout(timer);
        timer = undefined;
      } else if (st?.running) {
        void load();
      }
    };
    document.addEventListener('visibilitychange', onvis);
    void load();
    return () => {
      alive = false;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onvis);
    };
  });

  async function run() {
    sending = true;
    actionMsg = '';
    actionErr = '';
    try {
      await startBackup();
    } catch (e) {
      if (expired(e)) return;
      // копия уже идёт (по расписанию или из другой вкладки) — её ход виден ниже
      if (e instanceof ApiError && e.status === 409) actionMsg = e.message;
      else actionErr = errorText(e);
    } finally {
      sending = false;
      confirmRun = false;
    }
    await load();
  }

  async function remove() {
    const c = confirmDel;
    if (!c) return;
    sending = true;
    actionMsg = '';
    actionErr = '';
    try {
      await deleteBackup(c.name);
      actionMsg = `Копия ${c.name} удалена.`;
    } catch (e) {
      if (expired(e)) return;
      actionErr = e instanceof ApiError && e.status === 404 ? `Копии ${c.name} уже нет в каталоге копий.` : errorText(e);
    } finally {
      sending = false;
      confirmDel = null;
    }
    await load();
  }

  let phone = $derived(viewport.current === 'phone');
  let stale = $derived(st ? bannerText(st, now) : null);
  let busy = $derived(!!st?.running || sending);
  const driftText = (c: BackupCopy) => (c.drift.length ? c.drift.join('; ') : 'нет');
</script>

<section class="panel backups-page">
  <h2>Копии</h2>
  {#if error}<p class="error" role="alert">Не удалось получить копии: {error}</p>{/if}

  {#if st}
    <dl class="facts">
      <div>
        <dt>Последняя копия</dt>
        <dd data-testid="backup-last">
          {#if st.last_at}{dateTime(st.last_at)} <span class="muted">({sinceText(st.last_at, now)})</span>{:else}копий нет{/if}
        </dd>
      </div>
      <div>
        <dt>Автокопии</dt>
        <dd>
          <span data-testid="backup-schedule">{scheduleText(st.schedule, st.at, now)}</span>{#if st.schedule === 'daily'}, хранятся {st.keep} последних{/if}
          {#if st.schedule !== 'daily'}
            <div class="muted small">
              Включаются в конфигурации сервера: <code>[backup] schedule = "daily"</code> (время — <code>at</code>, UTC;
              хранить — <code>keep</code>).
            </div>
          {/if}
        </dd>
      </div>
      <div>
        <dt>Каталог копий</dt>
        <dd><code class="path">{st.dir}</code></dd>
      </div>
    </dl>

    {#if stale}
      <div class="alert" role="note" aria-label="Нет свежей копии">
        <p class="alert-title">{stale} (no_recent_backup)</p>
        <p>
          Предупреждение стоит, пока готовой копии в каталоге копий нет или она старше {st.warn_days} дн. Архив
          <code>--zip</code> и копия <code>--dir</code> в другое место его не снимают.
        </p>
      </div>
    {/if}

    <div class="row actions">
      <button type="button" class="primary" disabled={busy} onclick={() => (confirmRun = true)}>Снять копию сейчас</button>
    </div>
    {#if st.running}
      <p class="warn" role="status" data-testid="backup-running">
        Копия идёт с {hhmm(st.running.started_at)} ({byText(st.running.by)})…
      </p>
    {:else if st.last_run}
      {@const l = st.last_run}
      <p class="small last" role="status" data-testid="backup-last-run">
        {#if l.ok && l.report}
          <span class="ok">Готово в {hhmm(l.finished_at)}{l.by === 'schedule' ? ' (по расписанию)' : ''}:</span> {reportLine(l.report)}
        {:else}
          <span class="error-text">Копия не снята ({dateTime(l.finished_at)}): {l.error ?? 'причина не известна'}</span>
        {/if}
      </p>
    {/if}
    {#if actionMsg}<p class="small" role="status">{actionMsg}</p>{/if}
    {#if actionErr}<p class="error" role="alert">{actionErr}</p>{/if}

    <h3>Готовые копии</h3>
    {#if !st.items.length}
      <p class="muted">Готовых копий в каталоге копий нет.</p>
    {:else if phone}
      <!-- телефон: строки таблицы — карточками (§5.6 п. 2) -->
      <ul class="cards" aria-label="Готовые копии">
        {#each st.items as c (c.name)}
          <li class="card" data-backup={c.name}>
            <div class="card-head"><b>{dateTime(c.created_at)}</b> <span class="muted">{bytesText(c.bytes)}</span></div>
            <div class="small">баз {c.dbs} · блобов {c.blobs} · версия {c.server_version}</div>
            <div class="small">расхождения снимков: {driftText(c)}</div>
            <div class="small muted"><code>{c.name}</code></div>
            <button type="button" class="danger" disabled={sending} onclick={() => (confirmDel = c)} aria-label={`Удалить копию ${c.name}`}>
              Удалить
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr><th>Дата</th><th>Размер</th><th>Баз</th><th>Блобов</th><th>Версия</th><th>Расхождения</th><th><span class="sr">Действия</span></th></tr>
          </thead>
          <tbody>
            {#each st.items as c (c.name)}
              <tr data-backup={c.name}>
                <td>{dateTime(c.created_at)}<div class="muted small"><code>{c.name}</code></div></td>
                <td class="num">{bytesText(c.bytes)}</td>
                <td class="num">{c.dbs}</td>
                <td class="num">{c.blobs}</td>
                <td>{c.server_version}</td>
                <td class="drift">{driftText(c)}</td>
                <td>
                  <button type="button" class="danger" disabled={sending} onclick={() => (confirmDel = c)} aria-label={`Удалить копию ${c.name}`}>
                    Удалить
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <h3>Восстановление</h3>
    <p class="small">
      Из WebUI копию не восстановить и не скачать: восстановление идёт только на остановленном сервере, а в копии —
      хэши токенов и шифротексты Vault. Перенос на другую машину — архивом <code>zam-server backup --zip</code> на
      машине сервера.
    </p>
    <ol class="small steps">
      <li>
        Мастер-ключ Vault в копию не входит: сохраните его отдельно (<code>master.key</code> каталога данных или
        <code>ZAM_MASTER_KEY</code>) и положите на место <b>до</b> <code>restore</code> — иначе секреты Vault не раскрыть.
      </li>
      <li>Остановите сервер.</li>
      <li>
        На машине сервера: <code>zam-server restore &lt;каталог копии&gt; --config &lt;конфиг&gt;</code>. Каталог данных
        не пуст — с <code>--replace</code>: прежние данные уйдут в <code>backups/pre-restore-…</code>.
      </li>
      <li>Запустите сервер и войдите заново: вход в WebUI после восстановления сбрасывается, токены агентов прежние.</li>
    </ol>
    <p class="small">
      Порядок целиком — руководство <code>README-install.md</code> комплекта сервера, раздел «Перенос на другую машину»
      (<a href={GUIDE_URL} target="_blank" rel="noopener noreferrer">на GitHub</a>).
    </p>
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

<Dialog.Root
  open={confirmRun}
  onOpenChange={(o) => {
    if (!o && !sending) confirmRun = false;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog">
      <Dialog.Title>Снять копию сейчас</Dialog.Title>
      <Dialog.Description>
        Сервер снимет полную копию каталога данных в каталог копий, не останавливаясь: запись и поиск работают. На
        большой базе это займёт минуты и место на диске размером с базы и новые вложения.
        {#if st?.schedule === 'daily'}Лишние копии сверх {st.keep} будут удалены, начиная с самых старых.{/if}
      </Dialog.Description>
      <div class="row" style="margin-top: 12px;">
        <button type="button" class="primary" disabled={sending} onclick={run}>Снять копию</button>
        <Dialog.Close disabled={sending}>Отмена</Dialog.Close>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<Dialog.Root
  open={confirmDel !== null}
  onOpenChange={(o) => {
    if (!o && !sending) confirmDel = null;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog">
      <Dialog.Title>Удалить копию</Dialog.Title>
      <Dialog.Description class="warn">
        Копия {confirmDel ? dateTime(confirmDel.created_at) : ''} (<code>{confirmDel?.name}</code>) будет удалена с диска
        сервера без возможности вернуть.
      </Dialog.Description>
      <div class="row" style="margin-top: 12px;">
        <button type="button" class="danger-fill" disabled={sending} onclick={remove}>Удалить</button>
        <Dialog.Close disabled={sending}>Отмена</Dialog.Close>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  h3 {
    margin: 20px 0 8px;
    font-size: 15px;
    font-weight: 600;
  }
  .small {
    font-size: 13px;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px 24px;
    margin: 0 0 12px;
  }
  .facts dt {
    font-size: 12px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .facts dd {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
  }
  .path {
    overflow-wrap: anywhere;
  }
  .alert {
    margin: 12px 0;
    padding: 10px 12px;
    border: 1px solid var(--callout-warning);
    border-left-width: 4px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .alert p {
    margin: 4px 0;
  }
  .alert-title {
    font-weight: 600;
  }
  /* флаги `--zip`, `--dir` — без переноса на дефисе */
  .alert code {
    white-space: nowrap;
  }
  .actions {
    margin: 12px 0 8px;
  }
  .last {
    overflow-wrap: anywhere;
  }
  .ok {
    color: var(--callout-tip);
  }
  .error-text {
    color: var(--danger);
  }
  .num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .drift {
    overflow-wrap: anywhere;
    min-width: 120px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  .card {
    display: grid;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow-wrap: anywhere;
  }
  .card-head {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .card button {
    justify-self: start;
    margin-top: 4px;
  }
  .steps {
    margin: 4px 0 8px;
    padding-left: 20px;
  }
  .steps li {
    margin: 4px 0;
  }
</style>
