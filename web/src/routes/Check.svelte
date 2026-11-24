<script lang="ts">
  // «Проверка памяти» (#/check; M7.3, спецификация §6.9 п. 2, §6.4–§6.5; ленивый чанк): вердикт
  // последнего прогона — «в норме», «ухудшение: …», «ухудшение после обновления до …», «накопление
  // базы 3/5», «изменилось: другая модель …», «поиск без векторов» (`checkVerdict.ts`) — с
  // метриками к медиане базы и советами; проваленные пробы — вопрос, ожидаемая цель (ссылка),
  // где нашлась, совет; число проб по видам; «Проверить сейчас» — сразу (правило Д §6.14, пока
  // прогон идёт — опрос раз в 3 с, скрытая вкладка не опрашивает); при открытой регрессии —
  // «Принять как норму» (окно-подтверждение: последний законченный ночной прогон эпизода, §6.5
  // п. 5; принимает только человек). Тренд, ручные пробы и блок администратора — M7.4 (§6.13).
  import { onMount } from 'svelte';
  import FormDialog from '../components/FormDialog.svelte';
  import Notice from '../components/Notice.svelte';
  import { ApiError, errorText, type Me } from '../lib/api';
  import { attention } from '../lib/attention.svelte';
  import {
    PROBE_KIND_TEXT,
    acceptRun,
    foundText,
    metricRows,
    openRegressionNote,
    regressionOpen,
    runStatusText,
    shownVerdict,
    triggerText,
    verdictNote,
    verdictText,
    type CheckRun,
    type CheckStatus,
  } from '../lib/checkVerdict';
  import { go } from '../lib/router';
  import { localTime } from '../lib/time';
  import { viewport } from '../lib/viewport.svelte';
  import { acceptCheck, checkStatus, lastNightlyRun, refRoute, runCheck } from '../lib/worksApi';

  let { me, onexpired }: { me: Me; onexpired: () => void } = $props();

  let st = $state<CheckStatus | null>(null);
  /** Последний законченный ночной прогон: открыта ли регрессия и что можно принять. */
  let nightly = $state<CheckRun | null>(null);
  let error = $state('');
  let notice = $state('');
  let actionError = $state('');
  let starting = $state(false);
  let acceptOpen = $state(false);
  /** Итог действия (`Notice`): после «Принять как норму» кнопка уходит — фокус сюда. */
  let noticeBox = $state<HTMLElement | null>(null);
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = true;
  const POLL_MS = 3000;

  let phone = $derived(viewport.current === 'phone');

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return true;
    }
    return false;
  }

  function plan() {
    clearTimeout(timer);
    timer = undefined;
    if (alive && st?.running && !document.hidden) timer = setTimeout(load, POLL_MS);
  }

  async function load() {
    const my = ++seq;
    try {
      // последний законченный ночной прогон — фильтром сервера (`?trigger=nightly&status=done`)
      const [r, n] = await Promise.all([checkStatus(), lastNightlyRun()]);
      if (my !== seq) return;
      st = r;
      nightly = n;
      error = '';
      attention.regression = regressionOpen(nightly);
    } catch (e) {
      if (my !== seq) return;
      if (expired(e)) return;
      error = errorText(e);
    }
    plan();
  }

  onMount(() => {
    void load();
    const onvis = () => {
      if (!document.hidden && st?.running) void load();
    };
    document.addEventListener('visibilitychange', onvis);
    return () => {
      alive = false;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onvis);
    };
  });

  /** «Проверить сейчас» — сразу (Д): прогон идёт в процессе сервера, страница опрашивает статус. */
  async function start() {
    starting = true;
    notice = '';
    actionError = '';
    try {
      await runCheck();
      notice = 'Прогон начат: проверка идёт в фоне, вердикт появится здесь.';
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.problem?.type === '/problems/check-running') notice = 'Прогон уже идёт.';
      else actionError = errorText(e);
    } finally {
      starting = false;
    }
    await load();
  }

  let target = $derived(acceptRun(nightly));

  /** «Принять как норму»: последний законченный ночной прогон эпизода; ошибка — в окне. */
  async function accept() {
    const run = target;
    if (!run) throw new Error('Принимать нечего: регрессии нет.');
    try {
      await acceptCheck(run);
    } catch (e) {
      if (expired(e)) return;
      throw new Error(
        e instanceof ApiError && e.problem?.type === '/problems/check-accept-not-latest'
          ? 'Принять можно только последний законченный ночной прогон эпизода: статус изменился — перечитайте страницу.'
          : errorText(e),
      );
    }
    acceptOpen = false;
    notice = 'Норма принята: база начинается с этого прогона заново, строка в дайджесте агента снята.';
    await load();
  }

  async function openRef(ref: string) {
    actionError = '';
    try {
      const r = await refRoute(me.user_id, ref);
      if (r) go(r);
      else actionError = `Запись ${ref} недоступна: удалена или вне ваших пространств.`;
    } catch (e) {
      if (expired(e)) return;
      actionError = `Запись ${ref}: ${errorText(e)}`;
    }
  }

  let last = $derived(st?.last ?? null);
  let verdict = $derived(shownVerdict(last, nightly));
  /** Строка у «Принять как норму»: последний прогон — не этот ночной (ревью r1 M6, r2 N3). */
  let openNote = $derived(openRegressionNote(last, nightly));
  let rows = $derived(last ? metricRows(last.metrics, last.compared_with ?? last.verdict?.compared_with ?? null) : []);
  const kindClass = (k: string | undefined) => (k ? `v-${k}` : '');
  /** «i:01KQ…E2EI1» — ссылка записи коротко (id — 26 знаков). */
  const shortRef = (r: string) => (r.length > 16 ? `${r.slice(0, 6)}…${r.slice(-5)}` : r);
  /** «Активных проб: 31 · вопросы исполнителя: 24 · посторонние: 7». */
  let probesLine = $derived(
    st
      ? [`Активных проб: ${st.probes.active}`, ...Object.entries(st.probes.by_kind).map(([k, n]) => `${PROBE_KIND_TEXT[k] ?? k}: ${n}`)].join(' · ')
      : '',
  );
</script>

<section class="panel check-page" aria-labelledby="check-h">
  <h2 id="check-h">Проверка памяти</h2>
  <p class="muted small">
    Ночью сервер задаёт поиску и подсказкам пробные вопросы о ваших записях — тем же путём, что у агента, без следов в
    учёте показов, — и сравнивает итог с базой пяти прошлых ночей. Знания при этом не меняются.
  </p>
  {#if error}<p class="error" role="alert">Не удалось получить статус: {error}</p>{/if}

  {#if st}
    <div class="verdict {kindClass(verdict?.kind)}" data-testid="check-verdict">
      {#if last}
        {#if verdict}
          <p class="verdict-text"><span class="muted">Вердикт:</span> <b>{verdictText(verdict)}</b></p>
          {#if verdictNote(verdict)}<p class="muted small">{verdictNote(verdict)}</p>{/if}
        {:else}
          <p class="verdict-text"><span class="muted">Прогон:</span> <b>{runStatusText(last)}</b></p>
        {/if}
        <p class="muted small">
          {triggerText(last.trigger)} прогон{last.night ? ` ночи ${last.night}` : ''} · {localTime(last.finished_at ?? last.started_at)}
          {#if last.flags.includes('backup_overlap')} · шла копия — задержки не сравниваются{/if}
        </p>
        {#if last.accepted_at}
          <p class="small" data-testid="check-accepted">Норма принята {localTime(last.accepted_at)}{last.accepted_by ? ` (${last.accepted_by})` : ''}.</p>
        {/if}
      {:else}
        <p class="verdict-text"><b>Прогонов ещё не было.</b></p>
        <p class="muted small">Первый ночной прогон — в окне ночи (страница «Внутренние работы»); ручной — кнопкой ниже.</p>
      {/if}
    </div>
    {#if st.running}<p class="warn small" role="status" data-testid="check-running">Идёт прогон проверки…</p>{/if}
    <div class="row actions">
      <button type="button" disabled={starting || !!st.running} onclick={start}>Проверить сейчас</button>
      {#if target}
        <button type="button" class="primary" onclick={() => (acceptOpen = true)}>Принять как норму</button>
      {/if}
    </div>
    {#if openNote}
      <!-- последний прогон — не этот ночной (ручной поверх эпизода): принимается ночной -->
      <p class="warn small" data-testid="check-open-regression">{openNote}</p>
    {/if}
    <div bind:this={noticeBox}><Notice text={notice} /></div>
    {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
    <p class="muted small">
      Ночная проверка: на сервере — {st.check.server === 'nightly' ? 'включена' : 'выключена'}, вашей памяти — {st.check.user === 'on' ? 'включена' : 'выключена'}
      (выключатель — «Внутренние работы»). Ручной прогон от этого не зависит.
    </p>
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

<!-- цель пробы: ссылка короткой (буква и края id), полная — в имени кнопки и подсказке -->
{#snippet refLink(ref: string)}
  <button type="button" class="link ref" title={ref} aria-label="Открыть {ref}" onclick={() => openRef(ref)}>{shortRef(ref)}</button>
{/snippet}

{#if st && last}
  {#if rows.length}
    <section class="panel block" aria-labelledby="check-metrics-h">
      <h2 id="check-metrics-h">Метрики к медиане базы</h2>
      <div class="table-wrap">
        <table aria-label="Метрики к медиане базы">
          <thead><tr><th>Метрика</th><th>Этот прогон</th><th>Медиана базы</th></tr></thead>
          <tbody>
            {#each rows as r (r.label)}
              <tr><td>{r.label}</td><td class="num">{r.value}</td><td class="num">{r.median}</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if st.advice.length}
        <h3>Что чинить</h3>
        <ul class="small advice">{#each st.advice as a (a.code)}<li>{a.text}</li>{/each}</ul>
      {/if}
    </section>
  {/if}

  <section class="panel block" aria-labelledby="check-failing-h">
    <h2 id="check-failing-h">Проваленные пробы</h2>
    {#if !st.failing.length}
      <p class="muted">Проваленных проб нет.</p>
    {:else if phone}
      <ul class="cards" aria-label="Проваленные пробы">
        {#each st.failing as f (f.probe)}
          <li class="card" data-probe={f.probe}>
            <div class="question">{f.question}</div>
            {#if f.target}
              <div class="small">Цель: {@render refLink(f.target)}</div>
            {/if}
            <div class="small">{foundText(f)}</div>
            <div class="small muted">Совет: {f.advice_text ?? f.advice}</div>
          </li>
        {/each}
      </ul>
    {:else}
      <div class="table-wrap">
        <table aria-label="Проваленные пробы">
          <thead><tr><th>Вопрос</th><th>Ожидаемая цель</th><th>Где нашлась</th><th>Совет</th></tr></thead>
          <tbody>
            {#each st.failing as f (f.probe)}
              <tr data-probe={f.probe}>
                <td class="question">{f.question}<div class="muted small">{PROBE_KIND_TEXT[f.kind] ?? f.kind} · {f.split}</div></td>
                <td>
                  {#if f.target}{@render refLink(f.target)}{:else}—{/if}
                </td>
                <td>{foundText(f)}</td>
                <td>{f.advice_text ?? f.advice}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
{/if}

{#if st}
  <section class="panel block" aria-labelledby="check-probes-h">
    <h2 id="check-probes-h">Пробы</h2>
    <p data-testid="check-probes">{probesLine}</p>
    <p class="muted small">
      Вопросы исполнителя приходят из внутренних работ (вид «Пробы ночной проверки»), из кэша выжимок — каждую ночь сами,
      посторонние вопросы — общий набор сервера. Ввод и снятие проб вручную появятся в следующих версиях.
    </p>
  </section>
{/if}

<FormDialog
  open={acceptOpen}
  confirm
  title="Принять как норму?"
  description="Ухудшение ночного прогона {nightly?.night ?? ''} станет новой нормой: база начнётся с этого прогона заново (накопление 1/5), строка в дайджесте агента снимется. Делайте так, только если ухудшение ожидаемо (сознательная правка умолчаний, меньший бюджет). Агент норму принять не может."
  submit="Принять как норму"
  refocus={() => noticeBox?.querySelector('[role="status"]')}
  onsubmit={accept}
  onclose={() => (acceptOpen = false)}
/>

<style>
  .block {
    margin-top: 16px;
  }
  .small {
    font-size: 13px;
  }
  h3 {
    margin: 16px 0 6px;
    font-size: 14px;
    font-weight: 600;
  }
  .verdict {
    margin: 12px 0 8px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-left-width: 4px;
    border-radius: 6px;
  }
  .verdict p {
    margin: 2px 0;
  }
  .verdict-text {
    font-size: 16px;
  }
  .v-ok {
    border-left-color: var(--callout-tip);
  }
  .v-regression {
    border-left-color: var(--danger);
  }
  .v-changed {
    border-left-color: var(--callout-important);
  }
  .v-degraded {
    border-left-color: var(--callout-warning);
  }
  .actions {
    margin: 12px 0 8px;
  }
  .num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .question {
    overflow-wrap: anywhere;
    min-width: 200px;
  }
  .advice {
    margin: 4px 0;
    padding-left: 20px;
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
    overflow-wrap: anywhere;
  }
  .ref {
    font-family: ui-monospace, Consolas, monospace;
    white-space: nowrap;
  }
  .cards {
    list-style: none;
    margin: 8px 0 0;
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
  .card .question {
    min-width: 0;
  }
  td {
    vertical-align: top;
  }
</style>
