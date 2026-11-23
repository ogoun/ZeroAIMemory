<script lang="ts">
  // Общая форма правки записи журнала (M7.2, спецификация §5.9 п. 1): заголовок, текст (простым
  // текстом, как у задач и идей), теги, область; по виду: changelog — вид (`data.kind`); release —
  // заголовок и текст (версия и коммиты — как есть); задача — приоритет, кто ведёт, оценка, ссылки
  // (`data` задачи, M6.6 §9); открытый вопрос — текст. `PATCH` с `If-Match` ревизии — только
  // изменённое (`lib/entryEdit.ts`): нетронутое поле чужую правку не откатит. 409 — запись
  // перечитывается: нетронутые поля — из неё, набранное остаётся, ошибка видна, поля, изменённые и
  // здесь, и в другом месте, названы; журнал получает перечитанную запись (`onreread`). Несохранённое
  // — `dirty()` (панель доски спрашивает перед закрытием). Ленивый чанк: грузится при первом
  // «Изменить», в основной чанк не входит.
  //
  // Уход с несохранённым (ревью задачи 10, круг 2): пока форма открыта, переход по адресу (запись
  // из Ctrl+K, пункт журнала, «назад») сначала спрашивает `LEAVE_EDIT` (проверка ухода роутера, как у
  // редактора заметок; закрытие вкладки — `beforeunload`); журнал закрывает форму на переходе, а
  // действия, которые закрыли бы её на месте (фильтр, «Корзина», другая запись), спрашивают через
  // `leave()`. `release()` — журнал уже спросил сам (вопрос панели доски) или закрывает форму.
  import { onMount, untrack } from 'svelte';
  import { ApiError, errorText, getEntry, isStaleEntry, patchEntry, type Entry, type EntryKind } from '../lib/api';
  import { ESTIMATE_MAX, FIELD_TEXT, fieldsOf, isDirty, patchOf, rebase } from '../lib/entryEdit';
  import { LEAVE_EDIT } from '../lib/leaveEdit';
  import { CHANGELOG_KINDS, PRIORITIES, PRIORITY_TEXT } from '../lib/journals';
  import { setGuard } from '../lib/router';

  interface Props {
    user: string;
    /** Правимая запись (ревизия — для `If-Match`); поля берутся один раз, при открытии. */
    entry: Entry;
    /** Сохранено: ответ сервера — новая ревизия записи. */
    onsaved: (e: Entry) => void;
    /** «Отмена» или «Сохранить» без изменений. */
    oncancel: () => void;
    /** 409: запись перечитана — журнал показывает её новую ревизию. */
    onreread?: (e: Entry) => void;
    /** 401: сессия кончилась. */
    onexpired?: () => void;
  }

  let { user, entry, onsaved, oncancel, onreread, onexpired }: Props = $props();

  const TITLE: Partial<Record<EntryKind, string>> = {
    changelog: 'Правка записи',
    release: 'Правка релиза',
    task: 'Правка задачи',
    question: 'Правка вопроса',
  };
  const OWNER_TEXT: Record<string, string> = { agent: 'агент', human: 'человек' };

  /** Запись, против которой считается правка: на открытии, после 409 — перечитанная. */
  let cur = $state(untrack(() => entry));
  /** Поля записи `cur` — с ними сравнивается черновик. */
  let base = $state(untrack(() => fieldsOf(entry)));
  let f = $state(untrack(() => fieldsOf(entry)));
  let busy = $state(false);
  let error = $state('');
  const title = untrack(() => TITLE[entry.kind] ?? 'Правка записи');

  /** В форме есть несохранённое (для вопроса «Закрыть без сохранения?» панели журнала). */
  export function dirty(): boolean {
    return isDirty(f, base);
  }

  /** Журнал уже решил закрыть форму (спросил сам или несохранённого нет): переход не спрашивает. */
  let released = false;

  /** Можно ли закрыть форму: несохранённого нет или человек согласился его оставить. */
  export function leave(): boolean {
    return released || !dirty() || window.confirm(LEAVE_EDIT);
  }

  /** Форма закрывается по решению журнала: проверка ухода больше не спрашивает. */
  export function release(): void {
    released = true;
  }

  onMount(() => {
    const off = setGuard(() => leave());
    const onbeforeunload = (e: BeforeUnloadEvent) => {
      if (!released && dirty()) e.preventDefault();
    };
    window.addEventListener('beforeunload', onbeforeunload);
    return () => {
      off();
      window.removeEventListener('beforeunload', onbeforeunload);
    };
  });

  /** Фокус на первом поле при открытии формы. */
  function focusOnOpen(node: HTMLElement) {
    node.focus();
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    const r = patchOf(cur, f);
    if ('error' in r) {
      error = r.error;
      return;
    }
    if ('none' in r) return oncancel();
    busy = true;
    error = '';
    try {
      const saved = await patchEntry(user, cur.id, cur.rev, r.patch);
      released = true;
      onsaved(saved);
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired?.();
      if (isStaleEntry(err)) await reread(errorText(err));
      else error = errorText(err);
    } finally {
      busy = false;
    }
  }

  /** 409: перечитать запись и перенести на неё черновик (`rebase`); `stale` — текст сервера. */
  async function reread(stale: string) {
    let fresh: Entry;
    try {
      fresh = await getEntry(user, cur.id);
    } catch (err) {
      error = `${stale} ${errorText(err)}`;
      return;
    }
    const next = fieldsOf(fresh);
    const r = rebase(f, base, next);
    f = r.fields;
    base = next;
    cur = fresh;
    const clash = r.clash.length
      ? ` Изменено и здесь, и в другом месте: ${r.clash.map((k) => FIELD_TEXT[k]).join(', ')} — сверьте перед сохранением.`
      : '';
    error = `Запись изменили в другом месте (сейчас ревизия ${fresh.rev}): форма обновлена, ваши правки остались — проверьте и сохраните снова.${clash}`;
    onreread?.(fresh);
  }
</script>

<form class="form" onsubmit={submit} aria-label={title}>
  <h3>{title}</h3>
  <label>Заголовок <input bind:value={f.title} required maxlength="500" use:focusOnOpen /></label>
  <label>Текст <textarea bind:value={f.body} rows="4"></textarea></label>
  {#if cur.kind === 'changelog'}
    <label>
      Вид
      <select bind:value={f.kind}>
        {#each CHANGELOG_KINDS as k (k)}<option value={k}>{k}</option>{/each}
      </select>
    </label>
  {:else if cur.kind === 'task'}
    <div class="row">
      <label>
        Приоритет
        <select bind:value={f.priority}>
          <option value="">без приоритета</option>
          {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
        </select>
      </label>
      <label>
        Кто ведёт
        <select bind:value={f.owner}>
          <option value="">не задано</option>
          {#each Object.entries(OWNER_TEXT) as [k, t] (k)}<option value={k}>{t}</option>{/each}
        </select>
      </label>
      <label>
        Оценка, минут
        <input bind:value={f.estimate} inputmode="numeric" pattern="[0-9]*" maxlength="6" title="от 1 до {ESTIMATE_MAX}" />
      </label>
    </div>
    <label>Ссылки <span class="muted small">(по одной в строке)</span> <textarea bind:value={f.links} rows="2"></textarea></label>
  {/if}
  <div class="row">
    <label class="grow">Теги <span class="muted small">(через запятую)</span> <input bind:value={f.tags} /></label>
    <label class="grow">Область <input bind:value={f.area} maxlength="200" /></label>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>Сохранить</button>
    <button type="button" onclick={() => ((released = true), oncancel())}>Отмена</button>
  </div>
</form>

<style>
  .form {
    display: grid;
    gap: 8px;
    margin-top: 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .form h3 {
    margin: 0;
    font-size: 15px;
  }
  .form label {
    display: grid;
    gap: 4px;
  }
  .form .row {
    align-items: start;
  }
  .grow {
    flex: 1 1 180px;
    min-width: 0;
  }
  .form textarea {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 12px;
  }
</style>
