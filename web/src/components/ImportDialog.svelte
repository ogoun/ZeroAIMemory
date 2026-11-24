<script lang="ts">
  // Импорт архива пространства (M7.2, спецификация M7 §5.7 пп. 2–4; ленивый чанк — открывает
  // `Tree.svelte`): файл и флаги, «Проверить без записи» (`dry_run`) и «Импортировать». Тело
  // уходит `importArchive` (XMLHttpRequest — ход отправки), после 202 задание опрашивается сразу и
  // дальше раз в секунду, пока открыт диалог: этап, счётчики записи, итог. Отказ приёма — текстом по
  // коду ответа. Итог без `dry_run` помечает числа разделов устаревшими в `importJob` (`api.ts`, одна
  // точка), дерево перечитывает `ondone`. Закрытие окна во время отправки (до 202) — вопрос
  // «Прервать отправку?»: «Прервать» — `abort()` XHR, задания нет, итог — строкой под пространством
  // (`onclose(note)`); после 202 закрытие задание не отменяет (оно на сервере, итог живёт там час),
  // опрос останавливается, итог после закрытия не показывается **[решение координатора 26]** —
  // окно предупреждает по фазе. Ход и итог — живая область `role="status"` (есть в DOM заранее).
  // Каркас окна — общий `FormDialog` (M7.3, §6.14 п. 1, решение 81): шапка с «✕», основное действие
  // «Импортировать», затем «Отмена» справа, «Проверить без записи» — дополнительной кнопкой слева;
  // на телефоне — во весь экран («✕ · Импорт архива · Импортировать», «Назад» закрывает окно).
  // Логика отправки прежняя: вопрос «Прервать отправку?» — свой, внутри окна, вверху тела.
  // Несохранённое — выбранный архив или изменённые параметры до запуска (`dirty`): Esc, «✕»,
  // «Отмена» и «Назад» сначала спрашивают в окне, как у прочих окон; после итога или ошибки
  // запуска — нет (запуск был). Без файла обе кнопки запуска выключены.
  import { onDestroy, tick } from 'svelte';
  import FormDialog from './FormDialog.svelte';
  import {
    ApiError,
    errorText,
    importArchive,
    importJob,
    isImportAborted,
    type ImportJob,
    type ImportReport,
  } from '../lib/api';
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
    /** Окно закрыто; `note` — итог для строки пространства (отправка прервана). */
    onclose: (note?: string) => void;
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
  /** Закрытие во время отправки: вопрос «Прервать отправку?». */
  let askAbort = $state(false);
  let keepBtn = $state<HTMLButtonElement | null>(null);

  let busy = $derived(phase !== 'idle');
  /** Запуск был (итог или ошибка): выбранное «отправлено», вопрос о несохранённом не нужен. */
  let ran = $state(false);

  /** Выбран архив или изменены параметры, а запуска ещё не было. */
  const unsent = (): boolean => phase === 'idle' && !ran && (file !== null || own || onConflict !== 'skip' || axioms !== 'demote');

  /** Отправка тела (до 202): прервать — `abort()`. */
  let sending: AbortController | null = null;

  // Диалог закрыт — опрос останавливается: таймер снят, ответ запроса в пути отбрасывается;
  // окно убрано посреди отправки (выход, смена дерева) — отправка прерывается.
  let alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => {
    alive = false;
    clearTimeout(timer);
    sending?.abort();
  });

  /** Закрыть окно: во время отправки — сначала вопрос, после 202 — сразу (задание идёт дальше). */
  function requestClose() {
    if (phase !== 'sending') return onclose();
    askAbort = true;
  }

  // вопрос показан — на виду (тело окна могло быть прокручено к итогу) и фокус на безопасный выбор
  // «Продолжить отправку»
  $effect(() => {
    if (!askAbort) return;
    void tick().then(() => {
      keepBtn?.scrollIntoView({ block: 'nearest' });
      keepBtn?.focus();
    });
  });

  function abortSending() {
    sending?.abort();
    onclose('Импорт: отправка прервана, задание не создано.');
  }

  /** Предупреждение о закрытии окна — по фазе (§5.7 п. 4, решение координатора 26). */
  const closeWarning = $derived(
    phase === 'sending'
      ? 'Закрытие окна прервёт отправку: архив не дойдёт до сервера, задание не будет создано.'
      : phase === 'running'
        ? 'Закрытие окна не отменяет импорт: он идёт на сервере, итог после закрытия не показывается.'
        : 'Закрытие окна во время отправки прервёт её; после начала задания закрытие импорт не отменяет — он идёт на сервере, итог после закрытия не показывается.',
  );

  /** Итог — коротко для живой области (`role="status"`): что и сколько. */
  function summary(r: ImportReport): string {
    const head = r.dry_run ? 'Проверка без записи готова' : 'Импорт завершён';
    const made = r.dry_run ? 'будет создано' : 'создано';
    return `${head}: ${made} ${formatCount(r.created)}, конфликтов ${formatCount(r.conflicts_total)}.`;
  }
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

  /** Этап задания (§5.7 п. 3). */
  function stageName(j: ImportJob | null): string {
    const n = j?.queue ?? 0;
    return !j
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
  }

  /** Счётчики записи этапа: « — done из total» (сервер заполняет их только при записи). */
  function stageCount(j: ImportJob | null): string {
    const p = j?.progress;
    return p && p.total > 0 ? ` — ${formatCount(p.done)} из ${formatCount(p.total)}` : '';
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
    ran = true;
    phase = 'sending';
    askAbort = false;
    sending = new AbortController();
    let id: string;
    try {
      // свой архив — аксиомы как есть: выбор «Аксиомы чужого архива» к нему не относится
      const o = { own, onConflict, axioms: own ? ('keep' as const) : axioms, dryRun };
      id = (
        await importArchive(
          user,
          space,
          f,
          o,
          (s, t) => {
            sent = s;
            total = t;
          },
          sending.signal,
        )
      ).job;
    } catch (e) {
      phase = 'idle';
      askAbort = false;
      sending = null;
      if (!alive || isImportAborted(e)) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = `Архив не принят: ${refusalText(e)}`;
      return;
    }
    sending = null;
    // 202 пришёл, пока висел вопрос: отправку уже не прервать — вопрос снимается, задание идёт
    askAbort = false;
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

<!-- открыто, пока окно смонтировано: Esc, щелчок мимо, «✕» и «Отмена» идут в `requestClose` (во время
     отправки — вопрос, окно остаётся) -->
<FormDialog
  open
  wide
  title="Импорт архива"
  description={`В пространство «${spaceName}»${userName ? ` пользователя ${userName}` : ''}: архив экспорта (.zip) или zip папки с заметками .md. Отчёты переносятся только импортом из WebUI.`}
  submit="Импортировать"
  {busy}
  canSubmit={!!file}
  dirty={unsent}
  {error}
  onsubmit={() => {
    void start(false);
  }}
  onclose={requestClose}
>
  {#if askAbort && phase === 'sending'}
    <div class="ask warn" role="group" aria-labelledby="import-ask">
      <p id="import-ask"><b>Прервать отправку?</b> Архив ещё не дошёл до сервера: задание не будет создано.</p>
      <div class="row actions">
        <button type="button" class="danger-fill" onclick={abortSending}>Прервать отправку</button>
        <button type="button" bind:this={keepBtn} onclick={() => (askAbort = false)}>Продолжить отправку</button>
      </div>
    </div>
  {/if}
  <div class="form">
    <fieldset class="plain" disabled={busy}>
      <label class="field">
        Архив (.zip)
        <input
          type="file"
          accept=".zip"
          required
          data-autofocus
          onchange={(e) => {
            file = e.currentTarget.files?.[0] ?? null;
            result = null;
            error = '';
            ran = false;
          }}
        />
      </label>
      <label class="check inline"><input type="checkbox" bind:checked={own} /> Это мой архив (перенос с другого сервера)</label>
      <fieldset class="choice">
        <legend>Если запись уже есть</legend>
        <label class="check inline"><input type="radio" bind:group={onConflict} value="skip" /> пропустить</label>
        <label class="check inline"><input type="radio" bind:group={onConflict} value="revision" /> новой ревизией</label>
      </fieldset>
      <fieldset class="choice" disabled={own}>
        <legend>Аксиомы чужого архива</legend>
        <label class="check inline"><input type="radio" bind:group={axioms} value="demote" /> понизить до theorem</label>
        <label class="check inline"><input type="radio" bind:group={axioms} value="keep" /> оставить</label>
        {#if own}<p class="muted small">Свой архив — аксиомы и id как есть.</p>{/if}
      </fieldset>
    </fieldset>
  </div>
  <p class="warn small" data-testid="import-warning">{closeWarning}</p>

  <!-- ход и итог — живая область, есть в DOM заранее: объявляются начало отправки, смена этапа
       и итог; счётчики байт и записей — вне объявления (числа — у `progress`) -->
  <div class="progress" role="status" data-testid="import-status">
    {#if phase === 'sending'}
      <p>Отправка архива<span aria-hidden="true">: {bytesText(sent)} из {bytesText(total)}</span></p>
    {:else if phase === 'running'}
      <p data-testid="import-stage">Этап: {stageName(job)}<span aria-hidden="true">{stageCount(job)}</span></p>
    {:else if result}
      <p class="sr">{summary(result)}</p>
    {/if}
  </div>
  {#if phase === 'sending'}
    <progress max={total || 1} value={sent} aria-label="Отправка архива"></progress>
  {:else if phase === 'running' && job && job.progress.total > 0}
    <progress max={job.progress.total} value={job.progress.done} aria-label="Запись"></progress>
  {/if}
  {#if note}<p class="muted small">{note}</p>{/if}

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

  {#snippet extra()}
    <button type="button" disabled={busy || !file} onclick={() => start(true)}>Проверить без записи</button>
  {/snippet}
</FormDialog>

<style>
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
  /* только для чтения с экрана: итог в живой области (видимый — в разделе «Итог импорта») */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .ask p {
    margin: 0;
  }
  .ask .actions {
    margin-top: 8px;
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
