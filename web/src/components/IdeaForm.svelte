<script lang="ts">
  // Форма карточки идеи журнала «Идеи» (M5.7 §10): создание, правка и решения с полями — «Начать»
  // идеи Knowledge (выбор решения с предупреждением об общем), «Отложить» (дата UTC),
  // «Отказаться», «Вернуть». Ленивый чанк (M6 §16, задача 16): нужен только после нажатия кнопки
  // карточки, поэтому грузится при первом открытии формы и в чанк журнала не входит. Запросы к
  // серверу делает журнал (`onsubmit`), здесь — поля и проверка заполнения. Форма создаётся
  // заново на каждое открытие — начальные значения берутся из свойств один раз.
  import { untrack } from 'svelte';
  import { spaceRef, type Entry, type Space } from '../lib/api';
  import { IDEA_FORMS, decisionOf, priorityOf, tagsOf, type IdeaSubmit } from '../lib/ideaButtons';
  import { deferDefault, deferError, deferMin, sharedWarning, type IdeaForm } from '../lib/ideaForms';
  import { PRIORITIES, PRIORITY_TEXT, dataStr as str, type IdeaPriority } from '../lib/journals';

  interface Props {
    m: IdeaForm;
    /** Идея карточки (правка и решения); у создания — `null`. */
    idea: Entry | null;
    /** Решения, куда можно поставить задачу идеи Knowledge. */
    targets: Space[];
    /** Пространство задачи по умолчанию (`knowledge` или ссылка решения). */
    space0: string;
    busy: boolean;
    error: string;
    onsubmit: (s: IdeaSubmit) => void;
    onerror: (message: string) => void;
    oncancel: () => void;
  }

  let { m, idea, targets, space0, busy, error, onsubmit, onerror, oncancel }: Props = $props();

  const f = untrack(() => IDEA_FORMS[m]);
  /** Правимая идея: у создания и решений поля пустые. */
  const src = untrack(() => (m === 'edit' ? idea : null));
  /** `horizon` идеи до правки: неизменённая (в том числе наступившая) дата не ограничивается min. */
  const horizonWas = src ? str(src.data, 'horizon') : '';

  let title = $state(src?.title ?? '');
  let body = $state(src?.body ?? '');
  let priority = $state<IdeaPriority>(src ? priorityOf(src) : 'normal');
  /** «Не раньше» создания и правки, «отложить до» решения. */
  let horizon = $state(untrack(() => (m === 'defer' ? deferDefault() : horizonWas)));
  let tags = $state(src?.tags.join(', ') ?? '');
  let reason = $state('');
  let taskSpace = $state(untrack(() => space0));
  let chosen = $derived(targets.find((s) => spaceRef(s) === taskSpace) ?? null);

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (m === 'create' || m === 'edit') {
      const base = { title: title.trim(), body, tags: tagsOf(tags) };
      if (!base.title) return onerror('Укажите заголовок.');
      if (m === 'create') return onsubmit({ create: { ...base, data: { priority, ...(horizon ? { horizon } : {}) } } });
      // в data — только изменённое: прежний horizon мог наступить, и сервер отверг бы его
      const data: Record<string, unknown> = {};
      if (src && priority !== priorityOf(src)) data.priority = priority;
      if (horizon !== horizonWas) data.horizon = horizon || null;
      return onsubmit({ patch: { ...base, ...(Object.keys(data).length ? { data } : {}) } });
    }
    const d = decisionOf(m, { horizon, reason, space: taskSpace });
    const err = 'error' in d ? d.error : m === 'defer' ? deferError(horizon) : '';
    if ('error' in d || err) onerror(err);
    else onsubmit(d);
  }
</script>

<form class="form" class:panel={m === 'create'} onsubmit={submit} aria-label={f.aria}>
  <h3>{f.title}</h3>
  {#if m === 'create' || m === 'edit'}
    <label>Заголовок <input bind:value={title} required maxlength="500" /></label>
    <label>Замысел <textarea bind:value={body} rows="4"></textarea></label>
    <div class="row">
      <label>
        Приоритет
        <select bind:value={priority}>
          {#each PRIORITIES as p (p)}<option value={p}>{PRIORITY_TEXT[p]}</option>{/each}
        </select>
      </label>
      {@render date('Не раньше', false)}
    </div>
    <label>Теги (через запятую) <input bind:value={tags} aria-label="Теги идеи" /></label>
  {:else if m === 'start'}
    <label>
      Пространство задачи
      <select bind:value={taskSpace}>
        <option value="knowledge">Knowledge</option>
        {#each targets as s (s.id)}<option value={spaceRef(s)}>{s.name}{s.shared ? ' (общее)' : ''}</option>{/each}
      </select>
    </label>
    {#if chosen?.shared}<p class="warn" data-testid="shared-warning">{sharedWarning(chosen.name)}</p>{/if}
  {:else if m === 'defer'}
    {@render date('Отложить до', true)}
  {:else}
    <label
      >Причина{m === 'decline' ? ' (необязательно)' : ''}
      <textarea bind:value={reason} rows="2" maxlength="2000" required={m === 'reopen'}></textarea></label
    >
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="row end">
    <button type="submit" class="primary" disabled={busy}>{f.submit}</button>
    <button type="button" onclick={oncancel}>Отмена</button>
  </div>
</form>

{#snippet date(label: string, required: boolean)}
  <label>
    Не раньше
    <span class="row">
      <input type="date" bind:value={horizon} min={horizon && horizon === horizonWas ? undefined : deferMin()} {required} aria-label={label} />
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
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 12px;
  }
</style>
