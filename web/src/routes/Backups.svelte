<script lang="ts">
  // «Администрирование → Копии» (#/admin/backups; M7.2, спецификация M7 §5.6 п. 2; ленивый чанк):
  // последняя копия (когда и «N дн. назад»), расписание автокопий с местным временем срока или
  // «выключено» с подсказкой ключа, `no_recent_backup` текстом; «Снять копию сейчас» — копия в
  // процессе сервера (`POST /admin/backups`, подтверждение; пока идёт — «идёт с ЧЧ:ММ», опрос
  // `GET /admin/backups` раз в 3 с, скрытая вкладка не опрашивает; итог — строкой отчёта); список
  // готовых копий каталога копий (дата, размер, баз, блобов, версия сервера, расхождения снимков)
  // с «Удалить» (подтверждение); на телефоне — карточками строк. Блок «Восстановление» — порядок
  // из руководства: восстановления и скачивания копии из WebUI нет (решение координатора 24).
  // Подтверждения «Снять копию сейчас» и «Удалить копию» — окна на общем каркасе `FormDialog`
  // (M7.3, §6.14 п. 1): порядок кнопок, телефон, фокус («Удалить» — `danger`, фокус на «Отмена»),
  // ошибка сервера — в окне, окно открыто; «копия уже идёт» (409) и «копии уже нет» (404) — итог
  // строкой на странице, окно закрыто.
  import { onMount } from 'svelte';
  import FormDialog from '../components/FormDialog.svelte';
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

  let st = $state<Backups | null>(null);
  let error = $state('');
  /** Время ответа: давность считается от него, а не от часов отрисовки. */
  let now = $state(Date.now());
  let confirmRun = $state(false);
  let confirmDel = $state<BackupCopy | null>(null);
  let sending = $state(false);
  let actionMsg = $state('');
  /** Итог «Снять копию сейчас» — до конца копии (дальше — строка последнего прогона). */
  const STARTED = 'Копия запущена: её ход — ниже.';
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
      if (!r.running && actionMsg === STARTED) actionMsg = '';
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

  /** «Снять копию»: ошибка — в окне (исключение), окно открыто; копия запущена или уже идёт (по
   * расписанию, из другой вкладки) — итог строкой и ход копии перечитаны до закрытия окна: фокус
   * при закрытии — на строку итога, а не на кнопку, которую идущая копия выключает. */
  async function run() {
    sending = true;
    actionMsg = '';
    try {
      await startBackup();
      actionMsg = STARTED;
    } catch (e) {
      if (expired(e)) return;
      if (!(e instanceof ApiError && e.status === 409)) throw new Error(errorText(e));
      actionMsg = e.message;
    } finally {
      sending = false;
    }
    await load();
    confirmRun = false;
  }

  /** «Удалить» копию: ошибка — в окне, окно открыто; копии уже нет — окно закрыто, итог строкой. */
  async function remove() {
    const c = confirmDel;
    if (!c) return;
    sending = true;
    actionMsg = '';
    try {
      await deleteBackup(c.name);
      actionMsg = `Копия ${c.name} удалена.`;
    } catch (e) {
      if (expired(e)) return;
      if (!(e instanceof ApiError && e.status === 404)) throw new Error(errorText(e));
      actionMsg = `Копии ${c.name} уже нет в каталоге копий.`;
    } finally {
      sending = false;
    }
    confirmDel = null;
    await load();
  }

  /** Итог действия на странице — фокус после закрытия окна (кнопка ушла или выключена). */
  const actionStatus = () => document.querySelector('[data-testid="backup-action"]') ?? document.querySelector('[data-testid="backup-running"]');

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
    {#if actionMsg}<p class="small" role="status" tabindex="-1" data-testid="backup-action">{actionMsg}</p>{/if}

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
      Порядок целиком — руководство <code>README-install.md</code> комплекта сервера (в репозитории —
      <code>deploy/README-install.md</code>), раздел «Перенос на другую машину».
    </p>
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

<!-- подтверждения — окна на общем каркасе (§6.14 п. 1): на телефоне — во всю ширину, не во весь экран;
     при автокопиях снятие удаляет старшие копии сверх `keep` — необратимо: `danger`, фокус на «Отмена» -->
<FormDialog
  open={confirmRun}
  confirm
  danger={st?.schedule === 'daily'}
  title="Снять копию сейчас?"
  description={'Сервер снимет полную копию каталога данных в каталог копий, не останавливаясь: запись и поиск работают. ' +
    'На большой базе это займёт минуты и место на диске размером с базы и новые вложения.' +
    (st?.schedule === 'daily' ? ` Лишние копии сверх ${st.keep} будут удалены, начиная с самых старых.` : '')}
  submit="Снять копию"
  busy={sending}
  refocus={actionStatus}
  onsubmit={run}
  onclose={() => (confirmRun = false)}
/>

<FormDialog
  open={confirmDel !== null}
  confirm
  danger
  title="Удалить копию?"
  submit="Удалить"
  busy={sending}
  refocus={actionStatus}
  onsubmit={remove}
  onclose={() => (confirmDel = null)}
>
  <p class="warn">
    Копия {confirmDel ? dateTime(confirmDel.created_at) : ''} (<code>{confirmDel?.name}</code>) будет удалена с диска
    сервера без возможности вернуть.
  </p>
</FormDialog>

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
