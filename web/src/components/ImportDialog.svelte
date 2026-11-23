<script lang="ts">
  // Импорт архива пространства (M7.2, спецификация M7 §5.7 пп. 2–4; ленивый чанк — открывает
  // `Tree.svelte`): файл и флаги, «Проверить без записи» (`dry_run`) и «Импортировать». Тело
  // уходит `importArchive` (XMLHttpRequest — ход отправки), после 202 задание опрашивается сразу и
  // дальше раз в секунду, пока открыт диалог: этап, счётчики записи, итог. Отказ приёма — текстом по
  // коду ответа. Итог без `dry_run` помечает числа разделов устаревшими в `importJob` (`api.ts`, одна
  // точка), дерево перечитывает `ondone`. Закрытие окна задание не отменяет (оно на сервере, итог
  // живёт там час), опрос останавливается, итог после закрытия не показывается **[решение
  // координатора 26]** — окно предупреждает.
  import { Dialog } from 'bits-ui';
  import { onDestroy } from 'svelte';
  import { ApiError, errorText, importArchive, importJob, type ImportJob, type ImportReport } from '../lib/api';
  import { bytesText } from '../lib/backups';
  import { formatCount } from '../lib/counts';
  import { plural } from '../lib/plural';

  interface Props {
    /** id пользователя пространства (`X-Zam-User` администратора). */
    user: string;
    /** Ссылка пространства (`spaceRef`). */
    space: string;
    spaceName: string;
    /** Имя пользователя — у администратора в чужом пространстве; своё — пусто. */
    userName?: string;
    onclose: () => void;
    /** Итог без `dry_run` (готово или ошибка после начала записи): перечитать дерево. */
    ondone: () => void;
    onexpired: () => void;
  }

  let { user, space, spaceName, userName = '', onclose, ondone, onexpired }: Props = $props();

  /** Опрос задания (§5.7 п. 3). */
  const POLL_MS = 1000;
  /** Конфликтов и предупреждений в итоге — первые 50, дальше — «всего N» (§5.7 п. 3). */
  const SHOW = 50;

  let file = $state<File | null>(null);
  let own = $state(false);
  let onConflict = $state<'skip' | 'revision'>('skip');
  let axioms = $state<'demote' | 'keep'>('demote');
  /** `sending` — тело уходит, `running` — задание на сервере, опрос идёт. */
  let phase = $state<'idle' | 'sending' | 'running'>('idle');
  let sent = $state(0);
  let total = $state(0);
  /** Последний ответ опроса идущего задания. */
  let job = $state<ImportJob | null>(null);
  let result = $state<ImportReport | null>(null);
  let error = $state('');
  /** Сбой одного опроса (повтор через секунду). */
  let note = $state('');

  let busy = $derived(phase !== 'idle');

  // Диалог закрыт — опрос останавливается: таймер снят, ответ запроса в пути отбрасывается.
  let alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => {
    alive = false;
    clearTimeout(timer);
  });
  const pause = (ms: number) => new Promise<void>((r) => (timer = setTimeout(r, ms)));

  /** Отказ приёма — понятным текстом по коду (§5.7 п. 3), прочее — `errorText`. */
  function refusalText(e: unknown): string {
    if (e instanceof ApiError) {
      switch (e.status) {
        case 403:
          return 'импорт — только владельцу пространства';
        case 408:
          return 'отправка слишком медленная';
        case 413:
          return 'архив больше предела сервера (import.max_bytes)';
        case 503:
          return e.retryAfter !== null ? `сервер занят, повторите через ${e.retryAfter} с` : 'сервер занят, повторите позже';
        case 507:
          return 'мало места на сервере';
      }
    }
    return errorText(e);
  }

  /** Ошибка задания (problem+json со `status`). */
  function failureText(p: Record<string, unknown> | null): string {
    const status = typeof p?.status === 'number' ? p.status : 0;
    const detail = typeof p?.detail === 'string' ? p.detail : typeof p?.title === 'string' ? p.title : 'причина не указана';
    if (status === 503) return 'Импорт прерван: сервер останавливается. Повторите импорт после запуска — записанное пропустится.';
    if (status === 422) return `Архив отвергнут, ничего не записано: ${detail}`;
    return `Импорт не выполнен: ${detail}`;
  }

  /** Этап задания (§5.7 п. 3) и счётчики записи. */
  function stageText(j: ImportJob | null): string {
    const n = j?.queue ?? 0;
    const name = !j
      ? 'в очереди'
      : j.stage === 'queued'
        ? n > 0
          ? `в очереди: перед вами ${formatCount(n)} ${plural(n, 'задание', 'задания', 'заданий')}`
          : 'в очереди'
        : j.stage === 'validating'
          ? 'проверка архива'
          : j.stage === 'extracting'
            ? 'вложения'
            : j.stage === 'writing'
              ? 'запись'
              : j.stage === 'receiving'
                ? 'приём архива'
                : j.stage;
    const p = j?.progress;
    return p && p.total > 0 ? `${name} — ${formatCount(p.done)} из ${formatCount(p.total)}` : name;
  }

  const REASON: Record<string, string> = {
    changed: 'уже есть и отличается',
    'path-taken': 'путь занят другой заметкой',
    'report-immutable': 'отчёт уже есть, отчёты не меняются',
    'term-conflict': 'термин уже есть в словаре',
    'release-version': 'релиз этой версии уже есть',
    'report-space': 'отчёты — только в пространство решения',
    invalid: 'не прошла проверку',
  };

  /** Счётчики итога (§5.7 п. 3); у проверки — «будет …». */
  function rows(r: ImportReport): [string, number][] {
    const d = r.dry_run;
    return [
      [d ? 'будет создано' : 'создано', r.created],
      [d ? 'будет новых ревизий' : 'новых ревизий', r.revised],
      [d ? 'будет пропущено' : 'пропущено', r.skipped],
      ['конфликтов', r.conflicts_total],
      [d ? 'будет понижено аксиом' : 'понижено аксиом', r.demoted_axioms],
      [d ? 'будет заменено id' : 'заменено id', r.id_remapped],
      [d ? 'будет пропущено связей' : 'пропущено связей', r.links_skipped],
      ['неразрешённых ссылок', r.unresolved_total],
      ['предупреждений', r.warnings_total],
    ];
  }

  async function start(dryRun: boolean) {
    const f = file;
    if (!f || busy) return;
    error = '';
    note = '';
    result = null;
    job = null;
    sent = 0;
    total = f.size;
    phase = 'sending';
    let id: string;
    try {
      // свой архив — аксиомы как есть: выбор «Аксиомы чужого архива» к нему не относится
      const o = { own, onConflict, axioms: own ? ('keep' as const) : axioms, dryRun };
      id = (
        await importArchive(user, space, f, o, (s, t) => {
          sent = s;
          total = t;
        })
      ).job;
    } catch (e) {
      phase = 'idle';
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = `Архив не принят: ${refusalText(e)}`;
      return;
    }
    if (!alive) return;
    phase = 'running';
    await poll(id);
  }

  async function poll(id: string) {
    for (let first = true; alive; first = false) {
      if (!first) await pause(POLL_MS);
      if (!alive) return;
      let j: ImportJob;
      try {
        j = await importJob(user, space, id);
      } catch (e) {
        if (!alive) return;
        if (e instanceof ApiError && e.status === 401) {
          phase = 'idle';
          return onexpired();
        }
        if (e instanceof ApiError && e.status === 404) {
          phase = 'idle';
          job = null;
          error = 'Задание не найдено: сервер перезапущен или итог старше часа. Повторите импорт — записанное пропустится.';
          return;
        }
        note = `Опрос не удался: ${errorText(e)} — повтор через секунду.`;
        continue;
      }
      if (!alive) return;
      note = '';
      if (j.state === 'running') {
        job = j;
        continue;
      }
      phase = 'idle';
      job = null;
      if (j.state === 'done' && j.report) result = j.report;
      else error = failureText(j.error);
      if (!j.dry_run) ondone();
      return;
    }
  }
</script>

<Dialog.Root
  open
  onOpenChange={(o) => {
    if (!o) onclose();
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog import">
      <Dialog.Title>Импорт архива</Dialog.Title>
      <Dialog.Description class="muted">
        В пространство «{spaceName}»{userName ? ` пользователя ${userName}` : ''}: архив экспорта (.zip) или zip
        папки с заметками .md. Отчёты переносятся только импортом из WebUI.
      </Dialog.Description>
      <form class="form" onsubmit={(e) => e.preventDefault()}>
        <fieldset class="plain" disabled={busy}>
          <label class="field">
            Архив (.zip)
            <input
              type="file"
              accept=".zip"
              onchange={(e) => {
                file = e.currentTarget.files?.[0] ?? null;
                result = null;
                error = '';
              }}
            />
          </label>
          <label class="check"><input type="checkbox" bind:checked={own} /> Это мой архив (перенос с другого сервера)</label>
          <fieldset class="choice">
            <legend>Если запись уже есть</legend>
            <label class="check"><input type="radio" bind:group={onConflict} value="skip" /> пропустить</label>
            <label class="check"><input type="radio" bind:group={onConflict} value="revision" /> новой ревизией</label>
          </fieldset>
          <fieldset class="choice" disabled={own}>
            <legend>Аксиомы чужого архива</legend>
            <label class="check"><input type="radio" bind:group={axioms} value="demote" /> понизить до theorem</label>
            <label class="check"><input type="radio" bind:group={axioms} value="keep" /> оставить</label>
            {#if own}<p class="muted small">Свой архив — аксиомы и id как есть.</p>{/if}
          </fieldset>
        </fieldset>
      </form>
      <p class="warn small">
        Закрытие окна не отменяет импорт: он идёт на сервере, итог после закрытия не показывается.
      </p>

      {#if phase === 'sending'}
        <div class="progress">
          <p>Отправка архива: {bytesText(sent)} из {bytesText(total)}</p>
          <progress max={total || 1} value={sent} aria-label="Отправка архива"></progress>
        </div>
      {:else if phase === 'running'}
        <div class="progress">
          <p data-testid="import-stage">Этап: {stageText(job)}</p>
          {#if job && job.progress.total > 0}
            <progress max={job.progress.total} value={job.progress.done} aria-label="Запись"></progress>
          {/if}
        </div>
      {/if}
      {#if note}<p class="muted small">{note}</p>{/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}

      {#if result}
        {@const r = result}
        <section class="report" aria-label="Итог импорта">
          <div class="head">
            <h3>{r.dry_run ? 'Проверка без записи' : 'Импорт завершён'}</h3>
            {#if r.dry_run}<span class="tag">проверка</span>{/if}
          </div>
          {#if r.dry_run}<p class="muted small">Ничего не записано: ниже — что сделал бы импорт.</p>{/if}
          <ul class="counts">
            {#each rows(r) as [label, n] (label)}
              <li class:zero={n === 0}>{label}: <b>{formatCount(n)}</b></li>
            {/each}
          </ul>
          {#if r.conflicts.length}
            <h4>
              Конфликты{r.conflicts_total > Math.min(SHOW, r.conflicts.length)
                ? ` — первые ${Math.min(SHOW, r.conflicts.length)}, всего ${formatCount(r.conflicts_total)}`
                : ''}
            </h4>
            <ul class="list">
              {#each r.conflicts.slice(0, SHOW) as c, i (i)}
                <li><code>{c.id ?? c.path ?? '—'}</code> — {REASON[c.reason] ?? c.reason}</li>
              {/each}
            </ul>
          {/if}
          {#if r.warnings.length}
            <h4>
              Предупреждения{r.warnings_total > Math.min(SHOW, r.warnings.length)
                ? ` — первые ${Math.min(SHOW, r.warnings.length)}, всего ${formatCount(r.warnings_total)}`
                : ''}
            </h4>
            <ul class="list">
              {#each r.warnings.slice(0, SHOW) as w, i (i)}<li>{w}</li>{/each}
            </ul>
          {/if}
        </section>
      {/if}

      <div class="row actions">
        <button type="button" disabled={busy || !file} onclick={() => start(true)}>Проверить без записи</button>
        <button type="button" class="primary" disabled={busy || !file} onclick={() => start(false)}>Импортировать</button>
        <Dialog.Close type="button">Закрыть</Dialog.Close>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  :global(.dialog.import) {
    /* ширина — переменной: на телефоне окно во всю ширину (app.css); длинный итог — прокрутка */
    --dialog-w: 640px;
    max-height: 90vh;
    max-height: 90dvh;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .form {
    margin-top: 12px;
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  .plain {
    display: grid;
    gap: 10px;
  }
  .field {
    display: grid;
    gap: 4px;
  }
  .field input {
    max-width: 100%;
  }
  .choice {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 16px;
  }
  .choice legend {
    width: 100%;
    padding: 0;
    margin-bottom: 2px;
  }
  .choice .muted {
    width: 100%;
    margin: 0;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .small {
    font-size: 13px;
  }
  .warn {
    margin: 12px 0 0;
  }
  .progress p {
    margin: 12px 0 4px;
  }
  progress {
    width: 100%;
  }
  .report {
    margin-top: 12px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .head h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .tag {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    color: var(--muted);
  }
  .counts {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 2px 16px;
    margin: 8px 0 0;
    padding: 0;
    list-style: none;
  }
  .counts .zero {
    color: var(--muted);
  }
  h4 {
    margin: 12px 0 4px;
    font-size: 14px;
    font-weight: 600;
  }
  .list {
    margin: 0;
    padding-left: 18px;
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .actions {
    margin-top: 16px;
    justify-content: flex-end;
  }
  /* сенсорный ввод (§5.1 п. 2): флажок и варианты — строкой не ниже 44 px */
  @media (pointer: coarse) {
    .check {
      min-height: 44px;
    }
  }
</style>
