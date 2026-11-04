<script lang="ts">
  // «Вопросы» (M4.5 §4): вопросы агентов человеку. Открытые — первыми; варианты — кнопками
  // (рекомендация агента отмечена), ответ в один клик или текстом. Ответ идёт с
  // `channel: webui` и создаёт решение (insight `decision`, `decided_by: human`) — после
  // ответа карточка ссылается на него. Отвеченные и неактуальные — ниже, с ответом.
  // Вопрос-предложение идеи (`data.idea`, M5.7 §5.3) — со ссылкой «Идея w:<id>»; ответ — только
  // вариантом, у «отложить» — дата «не раньше» (UTC, не раньше завтрашней); «начать» идеи
  // Knowledge с задачей в общем решении (`data.task_space`) — после предупреждения (§5.2, п. 1).

  import { tick, untrack } from 'svelte';
  import {
    ApiError,
    answerQuestion,
    answeredDecision,
    errorText,
    listEntries,
    type Entry,
    type Space,
  } from '../lib/api';
  import { AUTHOR_TEXT, STATUS_TEXT, entryRef, fmtTime, utcDate } from '../lib/journals';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    /** Пространства пользователя: общее ли решение задачи вопроса-предложения. */
    spaces?: Space[];
    onexpired: () => void;
  }

  let { user, space, entry, spaces = [], onexpired }: Props = $props();

  const PAGE = 200;
  const CHANNEL_TEXT: Record<string, string> = { webui: 'WebUI', chat: 'чат', cli: 'CLI' };

  interface Opt {
    key: string;
    text: string;
  }

  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let seq = 0;
  let busy = $state('');
  /** Ошибка и текст ответа по вопросу. */
  let errors = $state<Record<string, string>>({});
  let texts = $state<Record<string, string>>({});
  /** Решение, созданное ответом только что (до перечитывания списка). */
  let created = $state<Record<string, string>>({});
  /** Вопрос-предложение, у которого открыт выбор даты «отложить», и сама дата. */
  let deferFor = $state('');
  let deferDate = $state('');

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => load(u, s, true));
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await listEntries(u, s, { kind: 'question', cursor: reset ? null : next, limit: PAGE });
      if (my !== seq) return;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
      if (reset && entry) {
        await tick();
        document.querySelector(`[data-question="${CSS.escape(entry)}"]`)?.scrollIntoView({ block: 'nearest' });
      }
    } catch (e) {
      if (my !== seq) return;
      if (reset) items = [];
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  const optionsOf = (e: Entry): Opt[] =>
    Array.isArray(e.data.options)
      ? (e.data.options as unknown[]).filter(
          (o): o is Opt => !!o && typeof o === 'object' && typeof (o as Opt).key === 'string' && typeof (o as Opt).text === 'string',
        )
      : [];
  /** Идея вопроса-предложения (`data.idea`) или пустая строка. */
  const ideaOf = (e: Entry): string => (typeof e.data.idea === 'string' ? e.data.idea : '');
  const ideaHref = (id: string) => href({ name: 'journal', user, space, journal: 'ideas', entry: id });

  /** Вопрос-предложение, у которого ждёт подтверждения «начать» в общем решении. */
  let confirmFor = $state('');

  /** Общее решение, где «начать» поставит задачу идеи Knowledge (иначе `null`). */
  function sharedTarget(q: Entry): Space | null {
    const here = spaces.find((s) => s.kind === 'knowledge');
    if (space !== 'knowledge' && q.space_id !== here?.id) return null;
    const ts = typeof q.data.task_space === 'string' ? q.data.task_space : '';
    const sp = spaces.find((s) => s.id === ts);
    return sp && sp.kind === 'solution' && sp.shared ? sp : null;
  }

  function pick(q: Entry, key: string) {
    if (ideaOf(q) && key === 'accept' && sharedTarget(q) && confirmFor !== q.id) {
      confirmFor = q.id;
      deferFor = '';
      errors[q.id] = '';
      return;
    }
    if (ideaOf(q) && key === 'defer') {
      confirmFor = '';
      deferFor = q.id;
      deferDate = utcDate(30);
      errors[q.id] = '';
      return;
    }
    answer(q, { option: key });
  }

  function submitDefer(e: SubmitEvent, q: Entry) {
    e.preventDefault();
    if (!deferDate || deferDate < utcDate(1)) {
      errors[q.id] = `Дата — не раньше ${utcDate(1)} (UTC).`;
      return;
    }
    answer(q, { option: 'defer', horizon: deferDate });
  }

  const recommended = (e: Entry): string =>
    typeof e.data.recommendation === 'string' ? e.data.recommendation : '';

  interface Ans {
    text?: string;
    option?: string;
    by: string;
    channel: string;
    answered_at: string;
    decision_id: string;
  }
  function answerOf(e: Entry): Ans | null {
    const a = e.data.answer;
    return a && typeof a === 'object' ? (a as Ans) : null;
  }
  const answerText = (e: Entry, a: Ans) => {
    const opt = optionsOf(e).find((o) => o.key === a.option);
    return [opt ? opt.text : a.option, a.text].filter(Boolean).join(' — ');
  };

  /** Открытые первыми (новые вопросы сверху внутри группы — как отдаёт сервер). */
  let ordered = $derived([...items.filter((q) => q.status === 'open'), ...items.filter((q) => q.status !== 'open')]);

  async function answer(q: Entry, a: { text?: string; option?: string; horizon?: string }) {
    if (busy) return;
    busy = q.id;
    errors[q.id] = '';
    try {
      const r = await answerQuestion(user, q.id, a);
      created[q.id] = r.decision_id;
      texts[q.id] = '';
      if (deferFor === q.id) deferFor = '';
      if (confirmFor === q.id) confirmFor = '';
      await load(user, space, true);
    } catch (e) {
      const d = answeredDecision(e);
      if (d !== null) {
        // уже отвечен в другом месте: показать итог
        if (d) created[q.id] = d;
        await load(user, space, true);
      }
      fail(e, (m) => (errors[q.id] = m));
    } finally {
      busy = '';
    }
  }

  function submitText(e: SubmitEvent, q: Entry) {
    e.preventDefault();
    const text = (texts[q.id] ?? '').trim();
    if (!text) {
      errors[q.id] = 'Введите ответ или выберите вариант.';
      return;
    }
    answer(q, { text });
  }

  const decisionHref = (id: string) =>
    href({ name: 'journal', user, space, journal: 'insights', entry: id });
</script>

<section class="journal questions">
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading && items.length === 0}<p class="muted">Загрузка…</p>{/if}
  {#if !loading && items.length === 0 && !error}<p class="muted">Вопросов нет.</p>{/if}

  <div class="list" role="feed" aria-label="Вопросы">
    {#each ordered as q (q.id)}
      {@const a = answerOf(q)}
      {@const opts = optionsOf(q)}
      <article class="q panel" class:selected={q.id === entry} class:done={q.status !== 'open'} data-question={q.id}>
        <header class="row">
          <h2>{q.title}</h2>
          <span class="chip status">{q.status ? STATUS_TEXT[q.status] : ''}</span>
          <span class="muted small">{AUTHOR_TEXT[q.author.kind] ?? q.author.kind} · {fmtTime(q.created_at)}</span>
        </header>
        {#if q.body}<p class="body">{q.body}</p>{/if}
        {#if ideaOf(q)}
          <p><a href={ideaHref(ideaOf(q))} data-testid="idea-link">Идея {entryRef('idea', ideaOf(q))}</a></p>
        {/if}

        {#if q.status === 'open'}
          {#if opts.length}
            <div class="row opts" role="group" aria-label="Варианты ответа">
              {#each opts as o (o.key)}
                <button
                  type="button"
                  class:primary={o.key === recommended(q)}
                  disabled={busy !== '' || (confirmFor === q.id && o.key === 'accept')}
                  title={o.key === recommended(q) ? 'Рекомендация агента' : ''}
                  onclick={() => pick(q, o.key)}
                >
                  {o.text}{#if o.key === recommended(q)}<span class="rec"> · рекомендуется</span>{/if}
                </button>
              {/each}
            </div>
          {/if}
          {#if confirmFor === q.id}
            {@const sp = sharedTarget(q)}
            <div class="row" role="group" aria-label="Подтверждение «начать»">
              <p class="warn" data-testid="shared-warning">Задача появится в решении «{sp?.name ?? ''}», её увидят участники.</p>
              <button type="button" class="primary" disabled={busy !== ''} onclick={() => pick(q, 'accept')}>Начать</button>
              <button type="button" onclick={() => (confirmFor = '')}>Отмена</button>
            </div>
          {/if}
          {#if deferFor === q.id}
            <form class="row" onsubmit={(e) => submitDefer(e, q)} aria-label="Отложить идею">
              <label class="row">
                Не раньше
                <input type="date" bind:value={deferDate} min={utcDate(1)} required aria-label="Отложить до" />
                <span class="muted small" title="Даты хранятся в UTC">UTC</span>
              </label>
              <button type="submit" disabled={busy !== ''}>Отложить</button>
              <button type="button" onclick={() => (deferFor = '')}>Отмена</button>
            </form>
          {/if}
          {#if !ideaOf(q)}
            <form class="row" onsubmit={(e) => submitText(e, q)} aria-label="Ответ текстом">
              <input
                bind:value={texts[q.id]}
                placeholder={opts.length ? 'или свой ответ' : 'ответ'}
                aria-label="Ответ на вопрос «{q.title}»"
                maxlength="4000"
              />
              <button type="submit" disabled={busy !== ''}>Ответить</button>
            </form>
          {/if}
          {#if errors[q.id]}<p class="error" role="alert">{errors[q.id]}</p>{/if}
        {:else if a}
          <p class="answer">
            Ответ: <strong>{answerText(q, a)}</strong>
            <span class="muted small">
              · {a.by === 'human' ? 'человек' : 'агент'} · {CHANNEL_TEXT[a.channel] ?? a.channel} · {fmtTime(a.answered_at)}
            </span>
          </p>
          {#if a.decision_id}
            <p><a href={decisionHref(a.decision_id)} data-testid="decision-link">Созданное решение</a></p>
          {/if}
        {:else if q.status === 'obsolete'}
          <p class="muted">Вопрос неактуален.</p>
        {/if}
        {#if created[q.id] && !a}
          <p><a href={decisionHref(created[q.id])} data-testid="decision-link">Созданное решение</a></p>
        {/if}
      </article>
    {/each}
  </div>
  {#if next && !loading}
    <button type="button" onclick={() => load(user, space, false)}>Показать ещё</button>
  {/if}
</section>

<style>
  .list {
    display: grid;
    gap: 10px;
  }
  .q.selected {
    border-color: var(--accent);
  }
  .q.done {
    opacity: 0.85;
  }
  .q h2 {
    margin: 0;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .opts {
    margin: 8px 0;
  }
  .rec {
    font-size: 12px;
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .small {
    font-size: 12px;
  }
  form input {
    flex: 1;
    min-width: 220px;
  }
</style>
