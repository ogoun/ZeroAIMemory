<script lang="ts">
  // Окно «Новый insight» (M7.3, спецификация §6.14 п. 7, решение 87): человек пишет и в Insights.
  // Поля — «Вид» (`gotcha`, `failure`, `discovery`, `decision`, `experiment`, `pattern`),
  // «Заголовок», «Текст», «Область», «Надёжность» (по умолчанию не указана: аксиому человек ставит
  // сам). Запрос — `POST /spaces/{space}/entries {kind: "insight", …}`, как у агента, автор —
  // человек. Дубль ловит сервер: 409 `/problems/possible-duplicate` — окно показывает кандидатов
  // ссылками (переход — роутером, как у ссылок окна) и даёт «Записать всё равно» (`force: true`)
  // или «Отмена»; правка полей снимает кандидатов (дубль проверяется заново). Триггеры и поля ADR
  // задаются после создания — окнами карточки. Ленивый чанк: грузится при первом «Новый insight».
  import FormDialog from './FormDialog.svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import {
    ApiError,
    createInsight,
    duplicateCandidates,
    errorText,
    type Certainty,
    type DuplicateCandidate,
    type Entry,
  } from '../lib/api';
  import { CERTAINTIES, CERTAINTY_TEXT, INSIGHT_KINDS, STATUS_TEXT } from '../lib/journals';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    /** Записано: окно закрывается, журнал открывает карточку созданной. */
    oncreated: (e: Entry) => void;
    onclose: () => void;
    onexpired?: () => void;
  }

  let { user, space, oncreated, onclose, onexpired }: Props = $props();

  let kind = $state('discovery');
  let title = $state('');
  let body = $state('');
  let area = $state('');
  let certainty = $state<Certainty | ''>('');
  let error = $state('');
  /** Кандидаты-дубли последней отправки; `null` — дубля не было. */
  let dups = $state<DuplicateCandidate[] | null>(null);
  /** Поля, для которых найдены дубли: правка полей снимает кандидатов. */
  let dupsFor = '';
  /** Записано: окно закрывается, переход к карточке не спрашивает о набранном. */
  let done = $state(false);
  /** Идёт запрос (и у «Записать всё равно» — кнопки окна выключены). */
  let sending = $state(false);

  const fields = () => JSON.stringify([kind, title.trim(), body, area.trim(), certainty]);
  const dirty = () => !done && !!(title.trim() || body.trim() || area.trim() || certainty || kind !== 'discovery');

  // правка полей после 409 — кандидаты больше не о них
  $effect(() => {
    const f = fields();
    if (dups && f !== dupsFor) dups = null;
  });

  async function send(force = false) {
    const t = title.trim();
    if (!t) {
      error = 'Укажите заголовок.';
      return;
    }
    if (sending) return;
    error = '';
    sending = true;
    let e: Entry;
    try {
      e = await createInsight(user, space, {
        title: t,
        body,
        kind,
        area: area.trim() || undefined,
        certainty: certainty || undefined,
        force,
      });
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired?.();
      const c = duplicateCandidates(err);
      if (c) {
        dups = c;
        dupsFor = fields();
        error = '';
        return;
      }
      error = errorText(err);
      return;
    } finally {
      sending = false;
    }
    done = true;
    oncreated(e);
  }

  const entryHref = (id: string) => href({ name: 'journal', user, space, journal: 'insights', entry: id });
</script>

<FormDialog
  open={!done}
  title="Новый insight"
  description="Знание для агента и для вас: ловушка, сбой, находка, решение, эксперимент или приём. Триггеры и поля ADR — после записи, в карточке."
  submit="Записать"
  wide
  {error}
  {dirty}
  busy={sending}
  extra={dups ? force : undefined}
  onsubmit={() => send(false)}
  {onclose}
>
  <!-- порядок §6.14 п. 7: «Вид», «Заголовок», «Текст», «Область», «Надёжность» -->
  <label>
    Вид
    <select bind:value={kind}>
      {#each INSIGHT_KINDS as k (k)}<option value={k}>{k}</option>{/each}
    </select>
  </label>
  <label>Заголовок <input bind:value={title} required maxlength="500" data-autofocus /></label>
  <label>Текст <textarea bind:value={body} rows="6"></textarea></label>
  <div class="row pair">
    <label>Область <span class="muted small">(необязательно)</span> <input bind:value={area} maxlength="200" placeholder="например, web" /></label>
    <label>
      Надёжность
      <select bind:value={certainty} title={certainty ? CERTAINTY_TEXT[certainty] : undefined}>
        <option value="">не указана</option>
        {#each CERTAINTIES as c (c)}<option value={c}>{c}</option>{/each}
      </select>
    </label>
  </div>
  {#if dups}
    <section class="dups warn" aria-labelledby="insight-dups-title" data-testid="insight-duplicates">
      <h3 id="insight-dups-title">Похоже на уже записанное</h3>
      {#if dups.length}
        <ul>
          {#each dups as d (d.id)}
            <li>
              <CertaintyBadge value={d.certainty} />
              <a href={entryHref(d.id)}>{d.title}</a>
              <span class="small">{STATUS_TEXT[d.status] ?? d.status}{d.reason === 'title' ? ' · тот же заголовок' : ' · близкий текст'}</span>
            </li>
          {/each}
        </ul>
      {/if}
      <p class="small">Откройте похожую запись и допишите её — или запишите новую всё равно.</p>
    </section>
  {/if}
</FormDialog>

{#snippet force()}
  <button type="button" onclick={() => send(true)} disabled={sending}>Записать всё равно</button>
{/snippet}

<style>
  .pair {
    align-items: start;
  }
  .pair > label {
    flex: 1 1 180px;
  }
  .dups h3 {
    margin: 0 0 4px;
    font-size: 14px;
  }
  .dups ul {
    margin: 0 0 4px;
    padding-left: 0;
    list-style: none;
    display: grid;
    gap: 4px;
  }
  .dups li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .dups p {
    margin: 0;
  }
  .small {
    font-size: 12px;
  }
</style>
