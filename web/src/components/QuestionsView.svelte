<script lang="ts">
  // «Вопросы» (M4.5 §4): вопросы агентов человеку. Открытые — первыми; варианты — кнопками
  // (рекомендация агента отмечена), ответ в один клик или текстом. Ответ идёт с
  // `channel: webui` и создаёт решение (insight `decision`, `decided_by: human`) — после
  // ответа карточка ссылается на него. Отвеченные и неактуальные — ниже, с ответом.
  import { tick, untrack } from 'svelte';
  import {
    ApiError,
    answerQuestion,
    answeredDecision,
    errorText,
    listEntries,
    type Entry,
  } from '../lib/api';
  import { AUTHOR_TEXT, STATUS_TEXT, fmtTime } from '../lib/journals';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

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

  async function answer(q: Entry, a: { text?: string; option?: string }) {
    if (busy) return;
    busy = q.id;
    errors[q.id] = '';
    try {
      const r = await answerQuestion(user, q.id, a);
      created[q.id] = r.decision_id;
      texts[q.id] = '';
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

        {#if q.status === 'open'}
          {#if opts.length}
            <div class="row opts" role="group" aria-label="Варианты ответа">
              {#each opts as o (o.key)}
                <button
                  type="button"
                  class:primary={o.key === recommended(q)}
                  disabled={busy !== ''}
                  title={o.key === recommended(q) ? 'Рекомендация агента' : ''}
                  onclick={() => answer(q, { option: o.key })}
                >
                  {o.text}{#if o.key === recommended(q)}<span class="rec"> · рекомендуется</span>{/if}
                </button>
              {/each}
            </div>
          {/if}
          <form class="row" onsubmit={(e) => submitText(e, q)} aria-label="Ответ текстом">
            <input
              bind:value={texts[q.id]}
              placeholder={opts.length ? 'или свой ответ' : 'ответ'}
              aria-label="Ответ на вопрос «{q.title}»"
              maxlength="4000"
            />
            <button type="submit" disabled={busy !== ''}>Ответить</button>
          </form>
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
