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
  // из Ctrl+K, пункт журнала, «назад») сначала спрашивает `LEAVE_EDIT` (общий помощник `guardLeave`:
  // проверка ухода роутера, как у редактора заметок; закрытие вкладки — `beforeunload`); журнал
  // закрывает форму на переходе, а действия, которые закрыли бы её на месте (фильтр, «Корзина»,
  // другая запись), спрашивают через `leave()`. `release()` — журнал уже спросил сам (вопрос панели
  // доски) или закрывает форму.
  //
  // Вариант «в окне» (`bare`, M7.3 §6.14 п. 3 — ленты без карточки): только поля и ошибка, кнопки и
  // вопрос о несохранённом — у окна `FormDialog` (`EditPanel` с `dialog`), отправка — `submit()`;
  // `leave()` не спрашивает и своей проверки ухода форма не ставит — их задаёт окно.
  import { onMount, untrack } from 'svelte';
  import { ApiError, errorText, getEntry, isStaleEntry, patchEntry, type Entry } from '../lib/api';
  import { ESTIMATE_MAX, FIELD_TEXT, fieldsOf, isDirty, patchOf, rebase, type EditFields } from '../lib/entryEdit';
  import { rebaseText } from '../lib/formDraft';
  import { LEAVE_EDIT, guardLeave } from '../lib/leaveEdit';
  import { CHANGELOG_KINDS, EDIT_TITLE, PRIORITIES, PRIORITY_TEXT } from '../lib/journals';

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
    /** Поля — в окне формы (`EditPanel` с `dialog`): без своих кнопок, заголовка и вопросов. */
    bare?: boolean;
  }

  let { user, entry, onsaved, oncancel, onreread, onexpired, bare = false }: Props = $props();

  const OWNER_TEXT: Record<string, string> = { agent: 'агент', human: 'человек' };

  /** Запись, против которой считается правка: на открытии, после 409 — перечитанная. */
  let cur = $state(untrack(() => entry));
  /** Поля записи `cur` — с ними сравнивается черновик. */
  let base = $state(untrack(() => fieldsOf(entry)));
  let f = $state(untrack(() => fieldsOf(entry)));
  let busy = $state(false);
  let error = $state('');
  /** Поля, изменённые и здесь, и в другом месте (409): помечены в форме (§6.14 п. 5). */
  let clash = $state<(keyof EditFields)[]>([]);
  const title = untrack(() => EDIT_TITLE[entry.kind] ?? 'Правка записи');
  const inDialog = untrack(() => bare);

  /** Журнал уже решил закрыть форму (спросил сам, несохранённого нет или сохранено): переход не
   * спрашивает. */
  let released = false;

  /** В форме есть несохранённое (для вопроса «Закрыть без сохранения?» панели журнала или окна);
   * отпущенная форма (закрывается, сохранена) — нет. */
  export function dirty(): boolean {
    return !released && isDirty(f, base);
  }

  /** Можно ли закрыть форму: несохранённого нет или человек согласился его оставить. В окне — не
   * спрашивает (вопрос задаёт окно). */
  export function leave(): boolean {
    return released || inDialog || !dirty() || window.confirm(LEAVE_EDIT);
  }

  /** Форма закрывается по решению журнала: проверка ухода больше не спрашивает. */
  export function release(): void {
    released = true;
  }

  // в окне проверку ухода ставит окно (`FormDialog`)
  onMount(() => (inDialog ? undefined : guardLeave(dirty, release)));

  /** Фокус на первом поле при открытии формы. В окне фокус ставит окно (`[data-autofocus]`), но
   * чанк формы приходит позже открытия окна (`Lazy`): если фокус ещё не в поле этого окна (на его
   * кнопке, на странице под окном) — первое поле (§6.14 п. 1). */
  function focusOnOpen(node: HTMLElement) {
    if (!inDialog) return node.focus();
    const a = document.activeElement;
    const dlg = node.closest('[data-form-dialog]');
    if (!(a && dlg?.contains(a) && a.matches('input, select, textarea'))) node.focus();
  }

  function onsubmitForm(e: SubmitEvent) {
    e.preventDefault();
    void submit();
  }

  /** Сохранить: `PATCH` только изменённого; 409 — черновик поверх свежей записи, ошибка видна. */
  export async function submit(): Promise<void> {
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
    clash = r.clash;
    // один текст 409 у всех форм с ревизией (`formDraft.rebaseText`)
    error = rebaseText(
      r.clash.map((k) => FIELD_TEXT[k]),
      fresh.rev,
    );
    onreread?.(fresh);
  }
</script>

{#if inDialog}
  {@render fields()}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
{:else}
  <form class="form" onsubmit={onsubmitForm} aria-label={title}>
    <h3>{title}</h3>
    {@render fields()}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="row end">
      <button type="submit" class="primary" disabled={busy}>Сохранить</button>
      <button type="button" onclick={() => ((released = true), oncancel())}>Отмена</button>
    </div>
  </form>
{/if}

<!-- пометка поля-расхождения 409 в подписи (её читает и программа чтения с экрана) -->
{#snippet note(k: keyof EditFields)}
  {#if clash.includes(k)}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
{/snippet}

{#snippet fields()}
  <label class:clash={clash.includes('title')}>Заголовок {@render note('title')}<input bind:value={f.title} required maxlength="500" use:focusOnOpen data-autofocus /></label>
  <label class:clash={clash.includes('body')}>Текст {@render note('body')}<textarea bind:value={f.body} rows="4"></textarea></label>
  {#if cur.kind === 'changelog'}
    <label class:clash={clash.includes('kind')}>
      Вид {@render note('kind')}
      <select bind:value={f.kind}>
        {#each CHANGELOG_KINDS as k (k)}<option value={k}>{k}</option>{/each}
      </select>
    </label>
  {:else if cur.kind === 'task'}
    <div class="row">
      <label class:clash={clash.includes('priority')}>
        Приоритет {@render note('priority')}
        <select bind:value={f.priority}>
          <option value="">без приоритета</option>
          {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
        </select>
      </label>
      <label class:clash={clash.includes('owner')}>
        Кто ведёт {@render note('owner')}
        <select bind:value={f.owner}>
          <option value="">не задано</option>
          {#each Object.entries(OWNER_TEXT) as [k, t] (k)}<option value={k}>{t}</option>{/each}
        </select>
      </label>
      <label class:clash={clash.includes('estimate')}>
        Оценка, минут {@render note('estimate')}
        <input bind:value={f.estimate} inputmode="numeric" pattern="[0-9]*" maxlength="6" title="от 1 до {ESTIMATE_MAX}" />
      </label>
    </div>
    <label class:clash={clash.includes('links')}
      >Ссылки <span class="muted small">(по одной в строке)</span> {@render note('links')}<textarea bind:value={f.links} rows="2"></textarea></label
    >
  {/if}
  <div class="row">
    <label class="grow" class:clash={clash.includes('tags')}
      >Теги <span class="muted small">(через запятую)</span> {@render note('tags')}<input bind:value={f.tags} /></label
    >
    <label class="grow" class:clash={clash.includes('area')}>Область {@render note('area')}<input bind:value={f.area} maxlength="200" /></label>
  </div>
{/snippet}

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
  .form .row,
  .row:has(> .grow) {
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
