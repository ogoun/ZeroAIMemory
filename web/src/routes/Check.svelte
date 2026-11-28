<script lang="ts">
  // «Проверка памяти» (#/check; M7.3, спецификация §6.9 п. 2, §6.4–§6.5; ленивый чанк): вердикт
  // последнего прогона — «в норме», «ухудшение: …», «ухудшение после обновления до …», «накопление
  // базы 3/5», «изменилось: другая модель …», «поиск без векторов» (`checkVerdict.ts`) — с
  // метриками к медиане базы и советами; проваленные пробы — вопрос, ожидаемая цель (ссылка),
  // где нашлась, совет; число проб по видам; «Проверить сейчас» — сразу (правило Д §6.14, пока
  // прогон идёт — опрос раз в 3 с, скрытая вкладка не опрашивает); при открытой регрессии —
  // «Принять как норму» (окно-подтверждение: последний законченный ночной прогон эпизода, §6.5
  // п. 5; принимает только человек).
  // M7.4 (§7.7 п. 1): тренд — свой SVG по `trend` статуса (`trend.ts`): доли `hit_search` и
  // `hit_prompt` (test) и задержка p95 — двумя графиками со своими осями, прогоны с ухудшением —
  // цветом и знаком ▼, пропуски — разрыв линии; подпись для чтения с экрана и скрытая таблица тех же
  // чисел. Набор проб — список `GET /check/probes` с фильтрами сервера (вид, состояние, часть);
  // «Добавить пробу» — окно формы (вопрос, цели-ссылки с проверкой формы `<буква>:<id>`, ошибка —
  // под полем); «Снять» — опасное подтверждение с причиной (снимает только человек, аудит
  // `check.probe.retire`; посторонние вопросы набора сервера не снимаются).
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
  import { focusNow } from '../lib/focus';
  import { PROBE_AUTHOR_TEXT, PROBE_QUESTION_MAX, RETIRE_REASON_MAX, parseProbeTargets, questionError, reasonError } from '../lib/probeForm';
  import { go } from '../lib/router';
  import { localTime } from '../lib/time';
  import { TREND_TITLE, segmentPath, trendChart, trendSummary, trendText, type TrendBox, type TrendChart, type TrendKey } from '../lib/trend';
  import { viewport } from '../lib/viewport.svelte';
  import {
    PROBES_PAGE,
    acceptCheck,
    addProbe,
    checkStatus,
    lastNightlyRun,
    listProbes,
    refRoute,
    retireProbe,
    runCheck,
    type Probe,
    type ProbeFilter,
  } from '../lib/worksApi';

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
    void loadProbes();
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

  // ---- тренд (свой SVG, `trend.ts`) ----

  /** Ширина графиков (по ширине области); высота — постоянная. */
  let shareW = $state(0);
  let msW = $state(0);
  const CHART_H = 150;
  const boxOf = (width: number, left: number): TrendBox => ({ width, height: CHART_H, left, right: 14, top: 10, bottom: 26 });
  let trendRows = $derived(st?.trend ?? []);
  let shareChart = $derived(trendChart(trendRows, ['hit_search', 'hit_prompt'], 'share', boxOf(shareW, 44)));
  let msChart = $derived(trendChart(trendRows, ['p95'], 'ms', boxOf(msW, 58)));
  let summary = $derived(trendSummary(trendRows));
  /** Деление оси: доли — «50 %», миллисекунды — «250 мс». */
  const tickText = (unit: 'share' | 'ms', v: number) => (unit === 'share' ? `${Math.round(v * 100)} %` : trendText('p95', v));
  /** Знак «ухудшение» — треугольник остриём вниз (▼) с центром в точке. */
  const worseMark = (x: number, y: number) => `M${x - 5} ${y - 4}L${x + 5} ${y - 4}L${x} ${y + 5}Z`;
  /** Подпись даты прогона по оси X. */
  const runDate = (c: TrendChart, i: number) => c.runs[i]?.date ?? '';

  // ---- набор проб (M7.4 §7.7 п. 1) ----

  let probes = $state<Probe[] | null>(null);
  let probesError = $state('');
  let pFilter = $state<Required<ProbeFilter>>({ kind: '', status: 'active', split: '' });
  let probeNotice = $state('');
  /** Итог добавления и снятия: окно закрывается — фокус сюда. */
  let probeBox = $state<HTMLElement | null>(null);
  let pseq = 0;

  async function loadProbes() {
    const my = ++pseq;
    probesError = '';
    try {
      const r = await listProbes(pFilter);
      if (my !== pseq) return;
      probes = r.items;
    } catch (e) {
      if (my !== pseq) return;
      if (expired(e)) return;
      probesError = errorText(e);
    }
  }

  /** После добавления и снятия: итог — до закрытия окна (фокус уходит на него), окно закрывается,
   * список и числа проб статуса — заново. */
  async function probesChanged(text: string, close: () => void) {
    probeNotice = text;
    close();
    await Promise.all([loadProbes(), load()]);
  }

  // «Добавить пробу» (окно формы, правило Б): вопрос и цели; ошибки формы — под полями
  let addOpen = $state(false);
  let aQuestion = $state('');
  let aTargets = $state('');
  let aQuestionError = $state('');
  let aTargetsError = $state('');
  let aQuestionEl = $state<HTMLTextAreaElement | null>(null);
  let aTargetsEl = $state<HTMLInputElement | null>(null);

  function openAdd() {
    aQuestion = '';
    aTargets = '';
    aQuestionError = '';
    aTargetsError = '';
    probeNotice = '';
    addOpen = true;
  }

  async function submitAdd() {
    aQuestionError = questionError(aQuestion);
    const t = parseProbeTargets(aTargets);
    aTargetsError = 'error' in t ? t.error : '';
    if (aQuestionError || 'error' in t) {
      // окно открыто, набранное цело: фокус — на первом поле с ошибкой
      focusNow(aQuestionError ? aQuestionEl : aTargetsEl);
      return;
    }
    let r;
    try {
      r = await addProbe(aQuestion.trim(), t.refs);
    } catch (e) {
      if (expired(e)) return;
      const type = e instanceof ApiError ? e.problem?.type : undefined;
      throw new Error(
        type === '/problems/probe-duplicate'
          ? 'Такой вопрос уже есть у активной пробы этого пространства (без учёта регистра и пробелов).'
          : errorText(e),
      );
    }
    await probesChanged(`Проба ${r.id} добавлена: ночная проверка задаст её со следующего прогона.`, () => (addOpen = false));
  }

  // «Снять» (опасное подтверждение с причиной, правило Б; снимает только человек)
  let retireOf = $state<Probe | null>(null);
  let rReason = $state('');
  let rReasonError = $state('');
  let rReasonEl = $state<HTMLTextAreaElement | null>(null);

  function openRetire(p: Probe) {
    rReason = '';
    rReasonError = '';
    probeNotice = '';
    retireOf = p;
  }

  async function submitRetire() {
    const p = retireOf;
    if (!p) return;
    rReasonError = reasonError(rReason);
    if (rReasonError) {
      focusNow(rReasonEl);
      return;
    }
    try {
      await retireProbe(p.id, rReason.trim());
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.status === 404) {
        void loadProbes();
        throw new Error('Проба уже снята или удалена — список перечитан.');
      }
      throw new Error(errorText(e));
    }
    await probesChanged(`Проба ${p.id} снята: ночная проверка её больше не задаёт.`, () => (retireOf = null));
  }

  /** Автор: «агент», «человек», «модель qwen3» (у модели — её имя; id токена агента человеку ничего
   * не говорит — не показывается). */
  const authorText = (p: Probe) =>
    p.author_kind === 'model' && p.author ? `модель ${p.author.replace(/^model:/, '')}` : (PROBE_AUTHOR_TEXT[p.author_kind] ?? p.author_kind);
  /** Вопрос коротко — в имени кнопки «Снять» (полный — в строке). */
  const shortQ = (q: string) => (q.length > 60 ? `${q.slice(0, 59)}…` : q);
  /** Снять можно активную пробу, кроме постороннего вопроса набора сервера (сервер — 409). */
  const retirable = (p: Probe) => p.status === 'active' && p.kind !== 'negative';
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
{/if}

<!-- график тренда: оси, деления, линии по отрезкам между пропусками, точки (ухудшение — ▼) -->
{#snippet chart(c: TrendChart, unit: 'share' | 'ms', title: string, id: string)}
  {@const b = c.box}
  {#if b.width > 0}
    <svg class="chart" width={b.width} height={b.height} role="img" aria-labelledby="{id}-t" aria-describedby="check-trend-sum">
      <title id="{id}-t">{title}</title>
      {#each c.ticks as t (t.value)}
        <line class="grid" x1={b.left} x2={b.width - b.right} y1={t.y} y2={t.y} />
        <text class="tick" x={b.left - 6} y={t.y} text-anchor="end" dominant-baseline="middle">{tickText(unit, t.value)}</text>
      {/each}
      {#if c.xs.length}
        <text class="tick" x={c.xs[0]} y={b.height - 6} text-anchor={c.xs.length > 1 ? 'start' : 'middle'}>{runDate(c, 0)}</text>
        {#if c.xs.length > 1}
          <text class="tick" x={c.xs[c.xs.length - 1]} y={b.height - 6} text-anchor="end">{runDate(c, c.xs.length - 1)}</text>
        {/if}
      {/if}
      {#each c.lines as l (l.key)}
        {#each l.segments as seg, si (si)}
          <path class="line l-{l.key}" d={segmentPath(seg)} />
        {/each}
        {#each l.points as p (p.run)}
          {#if p.worse}
            <path class="worse" d={worseMark(p.x, p.y)}><title>{runDate(c, p.i)}: {TREND_TITLE[l.key]} — {trendText(l.key, p.value)}, ухудшение</title></path>
          {:else}
            <circle class="dot l-{l.key}" cx={p.x} cy={p.y} r="3.5"><title>{runDate(c, p.i)}: {TREND_TITLE[l.key]} — {trendText(l.key, p.value)}</title></circle>
          {/if}
        {/each}
      {/each}
    </svg>
  {/if}
{/snippet}

{#snippet legend(keys: TrendKey[])}
  <ul class="legend small" aria-hidden="true">
    {#each keys as k (k)}<li><span class="sw l-{k}"></span>{TREND_TITLE[k]}</li>{/each}
    <li><span class="sw-worse">▼</span>ухудшение</li>
  </ul>
{/snippet}

{#if st && st.trend.length}
  <section class="panel block trend" aria-labelledby="check-trend-h" data-testid="check-trend">
    <h2 id="check-trend-h">Тренд</h2>
    <p class="muted small">
      Последние прогоны, старые слева. ▼ — прогон с ухудшением; разрыв линии — пропуск (прогон без метрик: переиндексация,
      сбой, предел времени).
    </p>
    <p class="sr-only" id="check-trend-sum">{summary}</p>
    <div class="charts">
      <figure bind:clientWidth={shareW}>
        <figcaption class="small">Доли проб части test</figcaption>
        {@render chart(shareChart, 'share', 'Найдено поиском и среди подсказок к запросу (test), %', 'trend-share')}
        {@render legend(['hit_search', 'hit_prompt'])}
      </figure>
      <figure bind:clientWidth={msW}>
        <figcaption class="small">Задержка поиска</figcaption>
        {@render chart(msChart, 'ms', 'Задержка поиска p95, мс', 'trend-p95')}
        {@render legend(['p95'])}
      </figure>
    </div>
    <!-- те же числа таблицей — для чтения с экрана (график — картинка с подписью). Скрывает обёртка:
         таблица не бывает уже своего содержимого, и скрытая сама она раздвигала бы страницу
         (на телефоне — смена ширины раскладки по кругу) -->
    <div class="sr-only">
      <table aria-label="Тренд по прогонам">
        <thead>
          <tr><th>Дата</th><th>Прогон</th><th>Состояние</th><th>{TREND_TITLE.hit_search}</th><th>{TREND_TITLE.hit_prompt}</th><th>{TREND_TITLE.p95}</th></tr>
        </thead>
        <tbody>
          {#each [...st.trend].reverse() as r (r.run)}
            <tr>
              <td>{r.date}</td>
              <td>{triggerText(r.trigger)}</td>
              <td>{r.status === 'done' ? (r.verdict === 'regression' ? 'ухудшение' : 'готово') : runStatusText({ status: r.status, error: null })}</td>
              <td>{r.status === 'done' ? trendText('hit_search', r.hit_search) : '—'}</td>
              <td>{r.status === 'done' ? trendText('hit_prompt', r.hit_prompt) : '—'}</td>
              <td>{r.status === 'done' ? trendText('p95', r.p95) : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
{/if}

{#if st && last}
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
      посторонние вопросы — общий набор сервера. Свою пробу можно добавить: вопрос и записи, которые он должен находить.
      Ошибочную — снять с причиной: снятие записывается в аудит, агент пробы не снимает.
    </p>
    <div class="row probe-bar">
      <label>
        Вид
        <select bind:value={pFilter.kind} onchange={loadProbes}>
          <option value="">все</option>
          {#each Object.entries(PROBE_KIND_TEXT) as [k, t] (k)}<option value={k}>{t}</option>{/each}
        </select>
      </label>
      <label>
        Состояние
        <select bind:value={pFilter.status} onchange={loadProbes}>
          <option value="active">активные</option>
          <option value="retired">снятые</option>
          <option value="">все</option>
        </select>
      </label>
      <label>
        Часть
        <select bind:value={pFilter.split} onchange={loadProbes}>
          <option value="">все</option>
          <option value="dev">dev</option>
          <option value="test">test</option>
        </select>
      </label>
      <button type="button" class="primary" onclick={openAdd}>Добавить пробу</button>
    </div>
    <div bind:this={probeBox}><Notice text={probeNotice} /></div>
    {#if probesError}<p class="error" role="alert">Не удалось получить пробы: {probesError}</p>{/if}
    {#if !probes && !probesError}
      <p class="muted">Загрузка…</p>
    {:else if probes && !probes.length}
      <p class="muted">Проб по этим фильтрам нет.</p>
    {:else if probes && phone}
      <ul class="cards" aria-label="Набор проб">
        {#each probes as p (p.id)}
          <li class="card" data-probe-set={p.id}>
            <div class="question">{p.question}</div>
            <div class="small muted">{PROBE_KIND_TEXT[p.kind] ?? p.kind} · {p.split} · {authorText(p)} · {localTime(p.created_at)}</div>
            {#if p.targets.length}
              <div class="small">Цели: {#each p.targets as t, i (t.ref)}{i ? ', ' : ''}{@render refLink(t.ref)}{/each}</div>
            {/if}
            {#if p.status === 'retired'}
              <div class="small">Снята {p.retired_at ? localTime(p.retired_at) : ''}{p.retired_reason ? `: ${p.retired_reason}` : ''}</div>
            {/if}
            {#if retirable(p)}
              <div class="row"><button type="button" class="danger" aria-label="Снять пробу «{shortQ(p.question)}»" onclick={() => openRetire(p)}>Снять</button></div>
            {/if}
          </li>
        {/each}
      </ul>
    {:else if probes}
      <div class="table-wrap">
        <table aria-label="Набор проб">
          <thead><tr><th>Вопрос</th><th>Цели</th><th>Автор</th><th>Состояние</th><th><span class="sr-only">Действие</span></th></tr></thead>
          <tbody>
            {#each probes as p (p.id)}
              <tr data-probe-set={p.id}>
                <td class="question">{p.question}<div class="muted small">{PROBE_KIND_TEXT[p.kind] ?? p.kind} · {p.split} · <code>{p.id}</code></div></td>
                <td>
                  {#each p.targets as t (t.ref)}<div>{@render refLink(t.ref)}</div>{:else}—{/each}
                </td>
                <td>{authorText(p)}<div class="muted small nowrap">{localTime(p.created_at)}</div></td>
                <td>
                  {#if p.status === 'retired'}
                    снята{#if p.retired_at}<div class="muted small nowrap">{localTime(p.retired_at)}</div>{/if}
                    {#if p.retired_reason}<div class="small">{p.retired_reason}</div>{/if}
                  {:else}
                    активна
                  {/if}
                </td>
                <td>
                  {#if retirable(p)}
                    <button type="button" class="danger small-btn" aria-label="Снять пробу «{shortQ(p.question)}»" onclick={() => openRetire(p)}>Снять</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
    {#if probes && probes.length >= PROBES_PAGE}
      <p class="muted small">Показаны {PROBES_PAGE} последних проб — уточните фильтры.</p>
    {/if}
  </section>
{/if}

<FormDialog
  open={addOpen}
  title="Добавить пробу"
  description="Ночная проверка задаёт вопрос поиску и подсказкам к запросу тем же путём, что у агента, и ждёт среди найденного записи-цели. Цели — записи одного пространства (Knowledge, Rules или решения)."
  submit="Добавить"
  dirty={() => addOpen && (aQuestion.trim() !== '' || aTargets.trim() !== '')}
  refocus={() => probeBox?.querySelector('[role="status"]')}
  onsubmit={submitAdd}
  onclose={() => (addOpen = false)}
>
  <label>
    Вопрос
    <textarea
      rows="3"
      bind:this={aQuestionEl}
      bind:value={aQuestion}
      maxlength={PROBE_QUESTION_MAX}
      required
      data-autofocus
      aria-invalid={aQuestionError ? 'true' : undefined}
      aria-describedby={aQuestionError ? 'probe-q-err' : undefined}
      oninput={() => (aQuestionError = '')}
    ></textarea>
  </label>
  {#if aQuestionError}<p class="error field-error" id="probe-q-err" role="alert">{aQuestionError}</p>{/if}
  <label>
    Цели
    <input
      bind:this={aTargetsEl}
      bind:value={aTargets}
      spellcheck="false"
      autocomplete="off"
      placeholder="i:01KQ… n:01KQ…"
      aria-required="true"
      aria-invalid={aTargetsError ? 'true' : undefined}
      aria-describedby={aTargetsError ? 'probe-t-err probe-t-hint' : 'probe-t-hint'}
      oninput={() => (aTargetsError = '')}
    />
  </label>
  {#if aTargetsError}<p class="error field-error" id="probe-t-err" role="alert">{aTargetsError}</p>{/if}
  <p class="muted small" id="probe-t-hint">
    Ссылки на записи через пробел или запятую — до пяти: i:&lt;id&gt; insight, n:&lt;id&gt; заметки, k:&lt;id&gt; задачи… Ссылку
    записи можно скопировать из её карточки.
  </p>
</FormDialog>

<FormDialog
  open={!!retireOf}
  title="Снять пробу?"
  subject={retireOf?.question}
  description="Снятая проба больше не задаётся ночной проверкой; снятие с причиной записывается в аудит. Вернуть её нельзя — только добавить заново."
  submit="Снять"
  danger
  dirty={() => !!retireOf && rReason.trim() !== ''}
  refocus={() => probeBox?.querySelector('[role="status"]')}
  onsubmit={submitRetire}
  onclose={() => (retireOf = null)}
>
  <label>
    Причина снятия
    <textarea
      rows="3"
      bind:this={rReasonEl}
      bind:value={rReason}
      maxlength={RETIRE_REASON_MAX}
      required
      data-autofocus
      aria-invalid={rReasonError ? 'true' : undefined}
      aria-describedby={rReasonError ? 'probe-r-err' : undefined}
      oninput={() => (rReasonError = '')}
    ></textarea>
  </label>
  {#if rReasonError}<p class="error field-error" id="probe-r-err" role="alert">{rReasonError}</p>{/if}
</FormDialog>

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
  .nowrap {
    white-space: nowrap;
  }
  /* скрыто с экрана, читается программой чтения с экрана; блок-контейнер — `.trend`
     (`position: relative`): абсолютный потомок не раздвигает страницу */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .trend {
    position: relative;
  }
  /* два графика рядом на широкой области, друг под другом — на узкой */
  .charts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 300px), 1fr));
    gap: 12px 24px;
    margin-top: 8px;
  }
  figure {
    margin: 0;
    min-width: 0;
  }
  figcaption {
    font-weight: 600;
    margin-bottom: 2px;
  }
  .chart {
    display: block;
    overflow: visible;
  }
  .chart .grid {
    stroke: var(--border);
    stroke-width: 1;
  }
  .chart .tick {
    fill: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .chart .line {
    fill: none;
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .chart .dot {
    stroke: var(--panel);
    stroke-width: 1.5;
  }
  /* ряды различаются и цветом, и начертанием (подсказки — пунктир): не только цветом */
  .chart .line.l-hit_search,
  .sw.l-hit_search {
    stroke: var(--accent);
    border-color: var(--accent);
  }
  .chart .dot.l-hit_search {
    fill: var(--accent);
  }
  .chart .line.l-hit_prompt {
    stroke: var(--callout-important);
    stroke-dasharray: 5 4;
  }
  .sw.l-hit_prompt {
    border-color: var(--callout-important);
    border-top-style: dashed;
  }
  .chart .dot.l-hit_prompt {
    fill: var(--callout-important);
  }
  .chart .line.l-p95,
  .sw.l-p95 {
    stroke: var(--callout-warning);
    border-color: var(--callout-warning);
  }
  .chart .dot.l-p95 {
    fill: var(--callout-warning);
  }
  .chart .worse {
    fill: var(--danger);
    stroke: var(--panel);
    stroke-width: 1;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 14px;
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
    color: var(--muted);
  }
  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .sw {
    display: inline-block;
    width: 18px;
    border-top: 2px solid;
  }
  .sw-worse {
    color: var(--danger);
    font-size: 11px;
  }
  .probe-bar {
    gap: 8px 12px;
    margin: 8px 0;
  }
  .probe-bar label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    min-width: 0;
  }
  .probe-bar select {
    min-width: 0;
    max-width: 100%;
  }
  .field-error {
    margin: -4px 0 8px;
    font-size: 13px;
  }
</style>
