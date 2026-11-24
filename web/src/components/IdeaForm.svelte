<script lang="ts" module>
  /** Правка в карточке: фокус на первом поле при открытии (у окна фокус ставит `FormDialog`). */
  function focusInline(node: HTMLElement) {
    if (!node.closest('[data-form-dialog]')) node.focus();
  }
</script>

<script lang="ts">
  // Форма карточки идеи журнала «Идеи» (M5.7 §10): создание, правка и решения с полями — «Начать»
  // идеи Knowledge (выбор решения с предупреждением об общем), «Отложить» (дата UTC),
  // «Отказаться», «Вернуть». Ленивый чанк (M6 §16, задача 16): нужен только после нажатия кнопки
  // карточки, поэтому грузится при первом открытии формы и в чанк журнала не входит. Запросы к
  // серверу делает журнал (`onsubmit`), здесь — поля и проверка заполнения. Форма создаётся
  // заново на каждое открытие — начальные значения берутся из свойств один раз. Несохранённое —
  // `dirty()`: поля против значений при открытии (финальное ревью M7.2, I1).
  //
  // M7.3 (§6.14, правила Б и В): создание и решения — окно формы (`FormDialog`: вопрос о
  // несохранённом задаёт окно, на телефоне — во весь экран), правка — в карточке на месте (вопрос
  // перед закрытием задаёт журнал). 409 — `rebase(fresh)`: поля, которые человек не трогал, — из
  // свежей идеи, набранное остаётся; возвращает поля-расхождения (п. 5).
  import { untrack } from 'svelte';
  import FormDialog from './FormDialog.svelte';
  import { spaceRef, type Entry, type Space } from '../lib/api';
  import { rebase as rebaseFields } from '../lib/formDraft';
  import { IDEA_FORMS, decisionOf, priorityOf, tagsOf, type IdeaSubmit } from '../lib/ideaButtons';
  import { deferDefault, deferError, deferMin, sharedWarning, type IdeaForm } from '../lib/ideaForms';
  import { PRIORITIES, PRIORITY_TEXT, dataStr as str, type IdeaPriority } from '../lib/journals';

  interface Props {
    m: IdeaForm;
    /** Окно открыто (у окна — `mode` журнала; правка в карточке — всегда). */
    open?: boolean;
    /** Идея карточки (правка и решения); у создания — `null`. */
    idea: Entry | null;
    /** Решения, куда можно поставить задачу идеи Knowledge. */
    targets: Space[];
    /** Пространство задачи по умолчанию (`knowledge` или ссылка решения). */
    space0: string;
    busy: boolean;
    error: string;
    onsubmit: (s: IdeaSubmit) => void | Promise<void>;
    onerror: (message: string) => void;
    oncancel: () => void;
    /** Окно: куда перевести фокус при закрытии (`FormDialog`). */
    refocus?: () => Element | null | undefined;
  }

  let { m, open = true, idea, targets, space0, busy, error, onsubmit, onerror, oncancel, refocus }: Props = $props();

  const f = untrack(() => IDEA_FORMS[m]);
  /** Правка — в карточке; создание и решения — в окне. */
  const inDialog = untrack(() => m !== 'edit');
  /** Правимая идея (после 409 — свежая): у создания и решений поля пустые. */
  let src = $state(untrack(() => (m === 'edit' ? idea : null)));
  /** `horizon` идеи до правки: неизменённая (в том числе наступившая) дата не ограничивается min. */
  let horizonWas = $derived(src ? str(src.data, 'horizon') : '');

  let title = $state(untrack(() => src?.title ?? ''));
  let body = $state(untrack(() => src?.body ?? ''));
  let priority = $state<IdeaPriority>(untrack(() => (src ? priorityOf(src) : 'normal')));
  /** «Не раньше» создания и правки, «отложить до» решения. */
  let horizon = $state(untrack(() => (m === 'defer' ? deferDefault() : src ? str(src.data, 'horizon') : '')));
  let tags = $state(untrack(() => src?.tags.join(', ') ?? ''));
  let reason = $state('');
  let taskSpace = $state(untrack(() => space0));
  let chosen = $derived(targets.find((s) => spaceRef(s) === taskSpace) ?? null);

  /** Поля формы (все виды: у каждого меняется только своё). */
  const fields = () => ({ title, body, priority, horizon, tags, reason, taskSpace });
  type Fields = ReturnType<typeof fields>;
  /** Поля при открытии формы (после 409 — свежей идеи). */
  let base: Fields = fields();
  /** Поля, изменённые и здесь, и в другом месте (409): помечены в форме. */
  let clash = $state<string[]>([]);
  const clashed = (k: keyof Fields) => clash.includes(k);

  /** В форме есть несохранённое (для вопроса «Закрыть без сохранения?»). */
  export function dirty(): boolean {
    const now = fields();
    return (Object.keys(base) as (keyof Fields)[]).some((k) => now[k] !== base[k]);
  }

  /** Подписи полей в тексте 409. */
  const LABEL: Record<keyof Fields, string> = {
    title: 'заголовок',
    body: 'замысел',
    priority: 'приоритет',
    horizon: 'дата',
    tags: 'теги',
    reason: 'причина',
    taskSpace: 'пространство задачи',
  };

  /** 409: идею изменили в другом месте — черновик поверх свежей (`rebase`): нетронутое — из неё,
   * набранное остаётся. Возвращает подписи полей, изменённых и здесь, и там. */
  export function rebase(fresh: Entry): string[] {
    if (m !== 'edit') return [];
    const next: Fields = {
      ...base,
      title: fresh.title,
      body: fresh.body,
      priority: priorityOf(fresh),
      horizon: str(fresh.data, 'horizon'),
      tags: fresh.tags.join(', '),
    };
    const r = rebaseFields(fields(), base, next);
    title = r.fields.title;
    body = r.fields.body;
    priority = r.fields.priority;
    horizon = r.fields.horizon;
    tags = r.fields.tags;
    base = next;
    src = fresh;
    clash = r.clash.map(String);
    return r.clash.map((k) => LABEL[k]);
  }

  /** Заполненная форма — журналу; ошибка заполнения — `onerror`. */
  function send(): void | Promise<void> {
    if (m === 'create' || m === 'edit') {
      const b = { title: title.trim(), body, tags: tagsOf(tags) };
      if (!b.title) return onerror('Укажите заголовок.');
      if (m === 'create') return onsubmit({ create: { ...b, data: { priority, ...(horizon ? { horizon } : {}) } } });
      // в data — только изменённое: прежний horizon мог наступить, и сервер отверг бы его
      const data: Record<string, unknown> = {};
      if (src && priority !== priorityOf(src)) data.priority = priority;
      if (horizon !== horizonWas) data.horizon = horizon || null;
      return onsubmit({ patch: { ...b, ...(Object.keys(data).length ? { data } : {}) } });
    }
    const d = decisionOf(m, { horizon, reason, space: taskSpace });
    const err = 'error' in d ? d.error : m === 'defer' ? deferError(horizon) : '';
    if ('error' in d || err) return onerror(err);
    return onsubmit(d);
  }

  function submitInline(e: SubmitEvent) {
    e.preventDefault();
    void send();
  }
</script>

{#if inDialog}
  <FormDialog
    {open}
    title={f.title}
    subject={idea?.title}
    submit={f.submit}
    danger={m === 'decline'}
    {busy}
    {error}
    {dirty}
    {refocus}
    onsubmit={send}
    onclose={oncancel}
  >
    {@render fieldsOf()}
  </FormDialog>
{:else}
  <form class="form" onsubmit={submitInline} aria-label={f.aria}>
    <h3>{f.title}</h3>
    {@render fieldsOf()}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="row end">
      <button type="submit" class="primary" disabled={busy}>{f.submit}</button>
      <button type="button" onclick={oncancel}>Отмена</button>
    </div>
  </form>
{/if}

{#snippet fieldsOf()}
  {#if m === 'create' || m === 'edit'}
    <label class:clash={clashed('title')}>Заголовок {@render note('title')}<input bind:value={title} required maxlength="500" data-autofocus use:focusInline /></label>
    <label class:clash={clashed('body')}>Замысел {@render note('body')}<textarea bind:value={body} rows="4"></textarea></label>
    <div class="row pair">
      <label class:clash={clashed('priority')}>
        Приоритет {@render note('priority')}
        <select bind:value={priority}>
          {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
        </select>
      </label>
      {@render date('Не раньше', false)}
    </div>
    <label class:clash={clashed('tags')}>Теги (через запятую) {@render note('tags')}<input bind:value={tags} aria-label="Теги идеи" /></label>
  {:else if m === 'start'}
    <label>
      Пространство задачи
      <select bind:value={taskSpace} data-autofocus>
        <option value="knowledge">Knowledge</option>
        {#each targets as s (s.id)}<option value={spaceRef(s)}>{s.name}{s.shared ? ' (общее)' : ''}</option>{/each}
      </select>
    </label>
    {#if chosen?.shared}<p class="warn" data-testid="shared-warning">{sharedWarning(chosen.name)}</p>{/if}
  {:else if m === 'defer'}
    {@render date('Отложить до', true)}
  {:else}
    <label
      >Причина {#if m === 'decline'}<span class="muted small">(необязательно)</span>{/if}
      <textarea bind:value={reason} rows="3" maxlength="2000" required={m === 'reopen'} data-autofocus></textarea></label
    >
  {/if}
{/snippet}

<!-- пометка поля-расхождения 409 в подписи -->
{#snippet note(k: keyof Fields)}
  {#if clashed(k)}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
{/snippet}

{#snippet date(label: string, required: boolean)}
  <!-- видимая подпись — та же, что имя поля (WCAG 2.5.3): «Не раньше» или «Отложить до» -->
  <label class:clash={clashed('horizon')}>
    {label} {@render note('horizon')}
    <span class="row">
      <input
        type="date"
        bind:value={horizon}
        min={horizon && horizon === horizonWas ? undefined : deferMin()}
        {required}
        aria-label={label}
        data-autofocus={m === 'defer' ? '' : undefined}
      />
      <span class="muted small" title="Даты хранятся в UTC">UTC</span>
    </span>
  </label>
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
  .form textarea {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .pair {
    align-items: start;
  }
  .pair > :global(label) {
    flex: 1 1 160px;
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 12px;
  }
</style>
