<script lang="ts">
  // «Предложения» (#/proposals[/<o:id>]; ленивый чанк): предложения человеку — решает только он.
  // Вкладки «Правила» и «Уборка» (M7.5, спецификация §8.8 п. 1: фильтр по типу, число открытых у
  // каждой — кэш `proposals/counts`; вкладка по умолчанию — «Правила», если там есть открытые или в
  // уборке их нет). Список и карточка — `JournalSplit` (§5.1 п. 5): рядом в широкой области,
  // карточка вместо списка в узкой; ряд решения — под шапкой (правило Г §6.14).
  // - «Правила» (M7.3, §6.9 п. 3, §6.7 п. 4) — кандидаты в Rules: путь, текст, пометка команд shell,
  //   довод, основание с исходными текстами (раскрываются по запросу), автор; «Принять» — окно
  //   (`FormDialog`, правило Б) с правкой пути и текста, суммой Rules в дайджесте и приростом, после —
  //   строка со ссылкой на созданную заметку (§6.14 п. 6); «Отклонить» — окно с причиной по желанию.
  // - «Уборка» (M7.5, §8.7–§8.8) — кандидаты уборки: тип кандидата, сигналы, тексты записей (L1),
  //   операция и довод (`lib/tidyCard.ts`); у `merge` — дифф (`DiffView`: `keep` против `text` или
  //   `drop`, полные тела записей); «решение человека» у `axiom`; «Применить» — окно (поле текста у
  //   `merge` и `append`, у прочих — подтверждение; отзыв — `danger`): операция выполняется от имени
  //   человека на текущей ревизии записи. Запись изменилась после предложения (`stale`) — кнопка
  //   недоступна с объяснением; 409 при отправке — объяснение в окне, набранное цело, кнопка окна
  //   выключена, после закрытия карточка перечитывается. «Отклонить» — окно с причиной по желанию.
  // Числа «Предложения N» шапки и «кандидаты: N» дерева перечитываются сами (`changesCounts`).
  import { tick } from 'svelte';
  import CertaintyBadge from '../components/CertaintyBadge.svelte';
  import DiffView from '../components/DiffView.svelte';
  import FormDialog from '../components/FormDialog.svelte';
  import JournalSplit from '../components/JournalSplit.svelte';
  import StatusTabs from '../components/StatusTabs.svelte';
  import { ApiError, errorText, getEntry, getNode, type Certainty, type Me } from '../lib/api';
  import { proposalCounts } from '../lib/counts.svelte';
  import { focusNow } from '../lib/focus';
  import { isDirty } from '../lib/formDraft';
  import { go, href } from '../lib/router';
  import { localTime } from '../lib/time';
  import {
    CANDIDATE_NOTE,
    applyBody,
    applyForm,
    appliedText,
    conflictText,
    decideState,
    humanDecisionText,
    mergeDiff,
    opShort,
    opText,
    signalText,
    tidyTitle,
    type TidyFields,
  } from '../lib/tidyCard';
  import {
    acceptProposal,
    applyTidy,
    declineProposal,
    getProposal,
    listProposals,
    refRoute,
    type Proposal,
    type ProposalCard,
  } from '../lib/worksApi';
  import { PROPOSAL_STATUS_TEXT, proposalAuthorText, rulesTokensText } from '../lib/worksText';

  let { me, proposal, onexpired }: { me: Me; proposal: string | null; onexpired: () => void } = $props();

  type Tab = Proposal['type'];
  type Item = Proposal & Partial<TidyFields>;
  type Card = ProposalCard & Partial<TidyFields>;

  /** Поля уборки карточки или строки (у `tidy` сервер отдаёт их всегда). */
  const tidyOf = (x: Item): TidyFields => ({
    status: x.status,
    candidate: x.candidate ?? '',
    signals: x.signals ?? [],
    records: x.records ?? [],
    ops: x.ops ?? [],
    op: x.op ?? null,
    stale: x.stale ?? false,
  });

  // ---- вкладки «Правила» и «Уборка» ----
  /** Открытые предложения по типам (кэш шапки и дерева). */
  let pc = $derived(proposalCounts.get(me.user_id, 'rules'));
  /** Выбранная вкладка; `null` — по числам открытых, когда они придут. */
  let picked = $state<Tab | null>(null);
  let tab = $derived<Tab>(picked ?? (pc && !pc.rules && pc.tidy ? 'tidy' : 'rule'));
  /** Существительное числа во вкладке: «Уборка, 3 открытых». */
  const OPEN_FORMS = ['открытое', 'открытых', 'открытых'] as const;
  let tabs = $derived([
    { id: 'rule', title: 'Правила', count: pc?.rules },
    { id: 'tidy', title: 'Уборка', count: pc ? (pc.tidy ?? 0) : undefined },
  ]);

  function selectTab(t: Tab) {
    picked = t;
    // карточка другого типа закрывается: список и карточка — одной вкладки
    if (card && card.type !== t) go({ name: 'proposals', proposal: null });
  }

  let items = $state<Item[]>([]);
  let loaded = $state(false);
  let error = $state('');
  let filters = $state<Record<Tab, '' | Proposal['status']>>({ rule: 'proposed', tidy: 'proposed' });
  let card = $state<Card | null>(null);
  let cardError = $state('');
  /** Итог решения в карточке; у принятия кандидата в Rules — ссылка на созданную заметку, у уборки —
   * ссылка на результат операции. */
  let decided = $state<{ text: string; note?: { id: string; path: string }; ref?: string } | null>(null);
  let actionError = $state('');

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return true;
    }
    return false;
  }

  let lseq = 0;
  async function loadList() {
    const my = ++lseq;
    const t = tab;
    error = '';
    try {
      const r = await listProposals(t, filters[t] || undefined);
      if (my !== lseq) return;
      items = r.proposals;
      loaded = true;
    } catch (e) {
      if (my !== lseq) return;
      if (expired(e)) return;
      error = errorText(e);
    }
  }

  $effect(() => {
    void tab;
    void filters[tab];
    loaded = false;
    void loadList();
  });

  let cseq = 0;
  async function loadCard(id: string | null, keepNote = false) {
    const my = ++cseq;
    cardError = '';
    actionError = '';
    if (!keepNote) decided = null;
    sources = {};
    if (!id) {
      card = null;
      return;
    }
    try {
      const c = await getProposal(id);
      if (my !== cseq) return;
      card = c;
      // открытая по адресу карточка — вкладка её типа
      if (c.type !== tab) picked = c.type;
      void loadBodies(c);
    } catch (e) {
      if (my !== cseq) return;
      if (expired(e)) return;
      card = null;
      cardError = e instanceof ApiError && e.status === 404 ? `Предложения ${id} нет.` : errorText(e);
    }
  }

  $effect(() => {
    const id = proposal;
    void loadCard(id);
  });

  const select = (id: string) => go({ name: 'proposals', proposal: proposal === id ? null : id });

  // ---- основание кандидата в Rules: исходные тексты по запросу ----
  let sources = $state<Record<string, { text?: string; error?: string }>>({});
  async function loadSource(ref: string) {
    if (sources[ref]) return;
    sources[ref] = {};
    try {
      const [letter, id] = ref.split(':');
      const text = letter === 'n' ? ((await getNode(me.user_id, id)).content ?? '') : (await getEntry(me.user_id, id)).body;
      sources[ref] = { text: text || '(текста нет)' };
    } catch (e) {
      if (expired(e)) return;
      sources[ref] = { error: errorText(e) };
    }
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

  // ---- уборка: полные тексты записей `merge` для диффа (не загрузились — L1 предложения) ----
  let bodies = $state<Record<string, string | undefined>>({});
  let bseq = 0;
  async function loadBodies(c: Card) {
    const my = ++bseq;
    bodies = {};
    if (c.type !== 'tidy' || c.op?.op !== 'merge') return;
    const refs = [c.op.keep, c.op.drop].filter((r): r is string => !!r);
    await Promise.all(
      refs.map(async (ref) => {
        try {
          const e = await getEntry(me.user_id, ref.replace(/^[a-z]:/, ''));
          if (my === bseq) bodies[ref] = e.body;
        } catch (e) {
          expired(e);
        }
      }),
    );
  }

  let tidy = $derived(card?.type === 'tidy' ? tidyOf(card) : null);
  let decision = $derived(tidy ? decideState(tidy) : null);
  let human = $derived(tidy ? humanDecisionText(tidy) : '');
  let diff = $derived(tidy ? mergeDiff(tidy, bodies) : null);

  // ---- «Принять» кандидата в Rules (окно формы с правкой пути и текста) ----
  let acceptOpen = $state(false);
  let aPath = $state('');
  let aContent = $state('');
  let aBase: Record<string, unknown> = {};
  const aDirty = () => acceptOpen && isDirty({ path: aPath, content: aContent }, aBase);

  function openAccept() {
    const c = card;
    if (!c) return;
    aPath = c.path;
    aContent = c.content;
    aBase = { path: aPath, content: aContent };
    acceptOpen = true;
  }

  async function accept() {
    const c = card;
    if (!c) return;
    const path = aPath.trim().replace(/^rules:\/*/, '');
    const content = aContent.trim();
    if (!path) throw new Error('Укажите путь заметки в Rules.');
    if (!content) throw new Error('Текст правила пуст.');
    const edit: { path?: string; content?: string } = {};
    if (path !== c.path) edit.path = path;
    if (content !== c.content.trim()) edit.content = content;
    let r;
    try {
      r = await acceptProposal(c.id, edit);
    } catch (e) {
      if (expired(e)) return;
      throw new Error(errorText(e));
    }
    acceptOpen = false;
    decided = { text: 'Принято: заметка Rules создана — ', note: { id: r.note.id, path: r.note.path ?? path } };
    await Promise.all([loadCard(c.id, true), loadList()]);
  }

  // ---- «Применить» предложение уборки ----
  let applyOpen = $state(false);
  let tText = $state('');
  let form = $derived(tidy ? applyForm(tidy) : null);
  /** Отказ 409 (`stale`, `not_open`): отправить снова нельзя, набранное цело; карточка
   * перечитывается после закрытия окна (кнопка, открывшая его, уходит вместе с рядом решения). */
  let blocked = $state(false);
  let dialogDiff = $derived(tidy && form?.kind === 'text' ? mergeDiff(tidy, bodies, tText) : null);

  function openApply() {
    if (!form) return;
    tText = form.initial;
    blocked = false;
    applyOpen = true;
  }

  async function apply() {
    const c = card;
    if (!c || !form || !tidy) return;
    const body = applyBody(form, tText);
    let r;
    try {
      r = await applyTidy(c.id, body);
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.status === 409) {
        const t = conflictText(e.problem?.code, e.problem?.status);
        if (t) {
          blocked = true;
          throw new Error(t);
        }
      }
      throw new Error(errorText(e));
    }
    applyOpen = false;
    decided = { text: appliedText(tidy.op, body.text !== undefined, r.warnings ?? []), ref: r.result };
    await Promise.all([loadCard(c.id, true), loadList()]);
  }

  async function closeApply() {
    applyOpen = false;
    if (!blocked || !card) return;
    blocked = false;
    // предложение уже устарело или решено: карточка — с объяснением на месте ряда решения
    await Promise.all([loadCard(card.id), loadList()]);
    await tick();
    const why = document.querySelector<HTMLElement>('[data-testid="tidy-stale"]') ?? document.querySelector<HTMLElement>('[data-testid="proposal-decided"]');
    if (why) focusNow(why);
  }

  // ---- «Отклонить» (окно с причиной по желанию) ----
  let declineOpen = $state(false);
  let dNote = $state('');

  function openDecline() {
    dNote = '';
    declineOpen = true;
  }

  async function decline() {
    const c = card;
    if (!c) return;
    try {
      await declineProposal(c.id, dNote.trim() || undefined);
    } catch (e) {
      if (expired(e)) return;
      throw new Error(errorText(e));
    }
    declineOpen = false;
    decided = {
      text: c.type === 'tidy' ? 'Предложение отклонено: кандидат по этим записям больше не предлагается.' : 'Кандидат отклонён: его источник больше не предлагается.',
    };
    await Promise.all([loadCard(c.id, true), loadList()]);
  }

  let contentEdited = $derived(!!card && aContent.trim() !== card.content.trim());
  const noteHref = (id: string) => href({ name: 'node', user: me.user_id, space: 'rules', node: id, anchor: null });
  const refId = (r: string | null) => (r ?? '').replace(/^n:/, '');
  const statusText = (s: string) => PROPOSAL_STATUS_TEXT[s] ?? s;
  const refocusDecided = () => document.querySelector('[data-testid="proposal-decided"]');
  /** Вид записи кандидата: «insight · gotcha», «термин». */
  const kindText = (r: { kind: string; insight_kind?: string | null }) =>
    r.kind === 'term' ? 'термин' : r.insight_kind ? `${r.kind} · ${r.insight_kind}` : r.kind;
  const RECORD_STATUS: Record<string, string> = { revisit: 'на пересмотре', superseded: 'замещена', retracted: 'отозвана' };
</script>

<section class="panel proposals-page" aria-labelledby="proposals-h">
  <h2 id="proposals-h">Предложения</h2>
  <StatusTabs label="Тип предложений" tabs={[...tabs]} selected={tab} onselect={(id) => selectTab(id as Tab)} noun={OPEN_FORMS}>
    {#if tab === 'rule'}
      <p class="muted small intro">
        Кандидаты в Rules — правила, которые агент или подключённая модель предлагают в Rules: поведение для всех проектов
        агента. Rules приходят агенту в каждой сессии, поэтому кандидата принимает или отклоняет только человек; заметка Rules
        создаётся от вашего имени, происхождение — в её истории.
      </p>
    {:else}
      <p class="muted small intro">
        Уборка памяти: ночью сервер находит в ваших решениях и Knowledge почти-дубли, противоречия, устаревшие и шумные записи
        и записи, которых не находят пробы, а агент или подключённая модель предлагает для каждой одну операцию. Само ничего не
        меняется: «Применить» выполняет операцию от вашего имени на текущей ревизии записи, история остаётся, удаления нет.
      </p>
    {/if}
    {#if error}<p class="error" role="alert">Не удалось получить предложения: {error}</p>{/if}

    <JournalSplit
      open={card !== null || cardError !== ''}
      back={{ href: href({ name: 'proposals', proposal: null }), label: 'Предложения' }}
      entry={card?.id ?? (cardError ? proposal : null)}
      onclose={() => {
        card = null;
        cardError = '';
      }}
    >
      {#snippet list()}
        <div class="row filters">
          <label class="row">
            Показать
            <select bind:value={filters[tab]} aria-label={tab === 'rule' ? 'Статус кандидатов' : 'Статус предложений уборки'}>
              <option value="proposed">ждут решения</option>
              <option value="accepted">{tab === 'rule' ? 'принятые' : 'применённые'}</option>
              <option value="declined">отклонённые</option>
              {#if tab === 'tidy'}<option value="stale">устаревшие</option>{/if}
              <option value="">все</option>
            </select>
          </label>
        </div>
        {#if loaded && !items.length}
          <p class="muted">
            {#if tab === 'rule'}{filters.rule === 'proposed' ? 'Кандидатов, ждущих решения, нет.' : 'Кандидатов нет.'}
            {:else}{filters.tidy === 'proposed' ? 'Предложений уборки, ждущих решения, нет.' : 'Предложений уборки нет.'}{/if}
          </p>
        {:else if !loaded && !error}
          <p class="muted">Загрузка…</p>
        {/if}
        <ul class="plist" aria-label={tab === 'rule' ? 'Кандидаты' : 'Предложения уборки'}>
          {#each items as p (p.id)}
            <li class:selected={p.id === proposal} data-entry={p.id} data-proposal={p.id}>
              {#if p.type === 'tidy'}
                {@const t = tidyOf(p)}
                <button type="button" class="title" onclick={() => select(p.id)}>{tidyTitle(t)}</button>
                <div class="muted small preview">{opShort(t)}</div>
                <div class="muted small">
                  {statusText(p.status)} · {proposalAuthorText(p.author_kind, p.author, p.session)} · {localTime(p.created_at)}
                  {#if t.stale && p.status === 'proposed'}<span class="chip changed">запись изменилась</span>{/if}
                  {#if (t.records ?? []).some((r) => r.human_decision)}<span class="chip human">решение человека</span>{/if}
                </div>
              {:else}
                <button type="button" class="title" onclick={() => select(p.id)}>rules:/{p.path}</button>
                <div class="muted small preview">{p.content.split('\n').find((l) => l.trim()) ?? ''}</div>
                <div class="muted small">
                  {statusText(p.status)} · {proposalAuthorText(p.author_kind, p.author, p.session)} · {localTime(p.created_at)}
                  {#if p.shell}<span class="chip shell">команды shell</span>{/if}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/snippet}
      {#snippet detail()}
        {#if cardError}<p class="error" role="alert">{cardError}</p>{/if}
        {#if card && tidy && decision}
          {@const c = card}
          <article class="card panel" aria-label="Карточка предложения уборки">
            <header>
              <h2>{tidyTitle(tidy)}</h2>
              <div class="row meta">
                <span class="chip status" data-testid="proposal-status">{statusText(c.status)}</span>
                <span class="muted small" title={c.work ? `внутренняя работа ${c.work}` : undefined}
                  >{proposalAuthorText(c.author_kind, c.author, c.session)} · {localTime(c.created_at)}{c.work ? ' · из внутренней работы' : ''}</span
                >
              </div>
              {#if CANDIDATE_NOTE[tidy.candidate]}<p class="muted small note">{CANDIDATE_NOTE[tidy.candidate]}</p>{/if}
            </header>
            {#if decision.show}
              <!-- решение — сразу под шапкой, до операции, диффа и записей (правило Г) -->
              <div class="row actions" role="group" aria-label="Решение по предложению">
                <button
                  type="button"
                  class="primary"
                  disabled={!decision.apply}
                  aria-describedby={decision.apply ? undefined : 'tidy-stale-why'}
                  onclick={openApply}>Применить</button
                >
                <button type="button" class="danger" onclick={openDecline}>Отклонить</button>
              </div>
            {/if}
            {#if decision.why}
              <p class="warn small" id="tidy-stale-why" role="note" tabindex="-1" data-testid="tidy-stale">{decision.why}</p>
            {/if}
            <p class="muted small decided" role="status" tabindex="-1" data-testid="proposal-decided">
              {#if decided}{decided.text}{#if decided.ref}<button type="button" class="link" onclick={() => openRef(decided!.ref!)}
                    >{decided.ref}</button
                  >.{/if}{/if}
            </p>
            {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
            {#if human}<p class="warn small" role="note" data-testid="tidy-human">{human}</p>{/if}
            <h3>Операция</h3>
            <p class="op" data-testid="tidy-op">{opText(tidy)}</p>
            {#if tidy.op?.op === 'append' && tidy.op.text}<pre class="rule" data-testid="tidy-op-text">{tidy.op.text}</pre>{/if}
            {#if diff}
              <h3>Различия</h3>
              <p class="muted small" data-testid="tidy-diff-caption">{diff.caption}</p>
              <div class="diff-wrap" data-testid="tidy-diff"><DiffView diff={diff.diff} /></div>
            {/if}
            {#if c.reason}
              <h3>Довод</h3>
              <p class="reason">{c.reason}</p>
            {/if}
            {#if tidy.signals?.length}
              <h3>Сигналы</h3>
              <ul class="signals" data-testid="tidy-signals">
                {#each tidy.signals as s, i (i)}<li>{signalText(s)}</li>{/each}
              </ul>
            {/if}
            <h3>Записи</h3>
            <ul class="records">
              {#each tidy.records ?? [] as r (r.ref)}
                <li data-record={r.ref}>
                  <div class="row rec-head">
                    <b>{r.title}</b>
                    <button type="button" class="link small" onclick={() => openRef(r.ref)} aria-label="Открыть {r.ref} «{r.title}»">{r.ref}</button>
                    <span class="muted small">{kindText(r)} · ревизия {r.rev}{r.status && RECORD_STATUS[r.status] ? ` · ${RECORD_STATUS[r.status]}` : ''}</span>
                    <CertaintyBadge value={(r.certainty ?? null) as Certainty | null} />
                    {#if r.human_decision}<span class="chip human">решение человека</span>{/if}
                  </div>
                  <pre class="source">{r.l1}</pre>
                </li>
              {/each}
            </ul>
            {#if c.decided_at}
              <p class="small">
                {statusText(c.status)}
                {localTime(c.decided_at)}{c.decided_by ? ` (${c.decided_by})` : ''}{c.decision_note ? `: ${c.decision_note}` : ''}
                {#if c.result_ref && c.status === 'accepted'}· результат <button type="button" class="link" onclick={() => openRef(c.result_ref!)}
                    >{c.result_ref}</button
                  >{/if}
              </p>
            {/if}
          </article>
        {:else if card}
          {@const c = card}
          <article class="card panel" aria-label="Карточка кандидата">
            <header>
              <h2>rules:/{c.path}</h2>
              <div class="row meta">
                <span class="chip status" data-testid="proposal-status">{statusText(c.status)}</span>
                <span class="muted small" title={c.work ? `внутренняя работа ${c.work}` : undefined}
                  >{proposalAuthorText(c.author_kind, c.author, c.session)} · {localTime(c.created_at)}{c.work ? ' · из внутренней работы' : ''}</span
                >
              </div>
            </header>
            {#if c.status === 'proposed'}
              <!-- решение — сразу под шапкой, до текста и основания (правило Г) -->
              <div class="row actions" role="group" aria-label="Решение по кандидату">
                <button type="button" class="primary" onclick={openAccept}>Принять</button>
                <button type="button" class="danger" onclick={openDecline}>Отклонить</button>
              </div>
            {/if}
            <p class="muted small decided" role="status" tabindex="-1" data-testid="proposal-decided">
              {#if decided}{decided.text}{#if decided.note}<a href={noteHref(decided.note.id)}>rules:/{decided.note.path}</a>.{/if}{/if}
            </p>
            {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
            {#if c.shell}
              <p class="warn small" role="note">В тексте есть команды shell — проверьте их, прежде чем принять: правило агент читает в каждой сессии.</p>
            {/if}
            <h3>Текст правила</h3>
            <pre class="rule" data-testid="proposal-content">{c.content}</pre>
            {#if c.status === 'proposed'}<p class="muted small">{rulesTokensText(c.rules_tokens)}</p>{/if}
            {#if c.reason}
              <h3>Довод</h3>
              <p class="reason">{c.reason}</p>
            {/if}
            {#if c.sources.length}
              <h3>Основание</h3>
              <ul class="sources">
                {#each c.sources as s (s.ref)}
                  <li>
                    {#if s.live}
                      <details ontoggle={(e) => (e.currentTarget as HTMLDetailsElement).open && loadSource(s.ref)}>
                        <summary>{s.title} <code class="muted">{s.ref}</code>{#if s.rev != null && s.current_rev != null && s.rev !== s.current_rev}<span class="muted small">{` — изменилось с ревизии ${s.rev}`}</span>{/if}</summary>
                        {#if sources[s.ref]?.text !== undefined}<pre class="source">{sources[s.ref].text}</pre>
                        {:else if sources[s.ref]?.error}<p class="error small" role="alert">{sources[s.ref].error}</p>
                        {:else}<p class="muted small">Загрузка…</p>{/if}
                        <button type="button" class="link small" onclick={() => openRef(s.ref)}>Открыть {s.ref}</button>
                      </details>
                    {:else}
                      <code>{s.ref}</code> <span class="muted small">— недоступно или удалено</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
            {#if c.decided_at}
              <p class="small">
                {statusText(c.status)} {localTime(c.decided_at)}{c.decided_by ? ` (${c.decided_by})` : ''}{c.decision_note ? `: ${c.decision_note}` : ''}
                {#if c.result_ref}· заметка <a href={noteHref(refId(c.result_ref))}>{c.result_ref}</a>{/if}
              </p>
            {/if}
          </article>
        {/if}
      {/snippet}
    </JournalSplit>
  </StatusTabs>
</section>

<FormDialog
  open={acceptOpen}
  title="Принять кандидата"
  subject={card ? `rules:/${card.path}` : undefined}
  description="Заметка Rules создаётся от вашего имени, в её истории — «из предложения». Путь и текст можно поправить."
  submit="Принять"
  wide
  refocus={refocusDecided}
  dirty={aDirty}
  onsubmit={accept}
  onclose={() => (acceptOpen = false)}
>
  <label>
    Путь в Rules
    <!-- префикс пространства — видимый, как в заголовке карточки («rules:/…»); в поле — путь -->
    <span class="row path-row"><span class="prefix" aria-hidden="true">rules:/</span><input
        bind:value={aPath}
        required
        data-autofocus
        spellcheck="false"
      /></span>
  </label>
  <label>
    Текст правила
    <textarea rows="8" bind:value={aContent} required></textarea>
  </label>
  {#if card}
    <p class="muted small" data-testid="proposal-tokens">
      {rulesTokensText(card.rules_tokens)}{contentEdited ? ' — по исходному тексту; правка пересчитается при принятии' : ''}
    </p>
    {#if card.shell}<p class="warn small" role="note">В тексте есть команды shell — проверьте их.</p>{/if}
  {/if}
</FormDialog>

<FormDialog
  open={applyOpen}
  title="Применить предложение"
  subject={tidy ? tidyTitle(tidy) : undefined}
  description={tidy ? opText(tidy) : undefined}
  submit="Применить"
  wide={form?.kind === 'text'}
  confirm={form?.kind === 'confirm'}
  danger={form?.danger}
  cautious={form?.kind === 'confirm' && !form.danger && !!human}
  canSubmit={!blocked}
  refocus={refocusDecided}
  dirty={() => applyOpen && form?.kind === 'text' && tText !== form.initial}
  onsubmit={apply}
  onclose={closeApply}
>
  {#if human}<p class="warn small" role="note">{human}</p>{/if}
  {#if form?.kind === 'text'}
    <label>
      {form.label}
      <textarea rows="8" bind:value={tText} required={form.required} data-autofocus></textarea>
    </label>
    <p class="muted small">{form.hint}</p>
    {#if dialogDiff}
      <p class="muted small">{dialogDiff.caption}</p>
      <DiffView diff={dialogDiff.diff} />
    {/if}
  {:else}
    <p class="small">Операция выполнится от вашего имени на текущей ревизии записи; в истории записи — «из предложения». Удаления нет.</p>
  {/if}
</FormDialog>

<FormDialog
  open={declineOpen}
  title={card?.type === 'tidy' ? 'Отклонить предложение' : 'Отклонить кандидата'}
  subject={card ? (tidy ? tidyTitle(tidy) : `rules:/${card.path}`) : undefined}
  description={card?.type === 'tidy'
    ? 'Предложение уходит в «отклонённые», записи не меняются; кандидат по этим записям больше не предлагается ни в какой ревизии.'
    : 'Отклонённый кандидат остаётся в списке «отклонённые»; источник внутренней работы больше не предлагается ни в какой ревизии.'}
  submit="Отклонить"
  danger
  refocus={refocusDecided}
  dirty={() => declineOpen && dNote.trim() !== ''}
  onsubmit={decline}
  onclose={() => (declineOpen = false)}
>
  <label>
    Причина (необязательно)
    <textarea rows="3" bind:value={dNote} maxlength="2000" data-autofocus></textarea>
  </label>
</FormDialog>

<style>
  .small {
    font-size: 13px;
  }
  h3 {
    margin: 14px 0 4px;
    font-size: 14px;
    font-weight: 600;
  }
  .intro {
    margin: 0 0 8px;
  }
  .filters {
    margin: 4px 0 10px;
  }
  .plist {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .plist li {
    padding: 8px 10px;
    border-radius: 6px;
    overflow-wrap: anywhere;
  }
  .plist li.selected {
    background: var(--selected);
  }
  .plist .title {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    font-weight: 600;
    text-align: left;
  }
  .preview {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .chip.shell,
  .chip.changed {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .chip.human {
    border: 1px solid var(--callout-important);
    color: var(--callout-important);
    background: transparent;
  }
  .card header h2 {
    margin: 0 0 6px;
    overflow-wrap: anywhere;
  }
  .meta {
    gap: 6px 10px;
  }
  .note {
    margin: 6px 0 0;
  }
  .actions {
    margin: 10px 0 4px;
  }
  .decided:empty {
    margin: 0;
  }
  .op {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .rule,
  .source {
    margin: 0;
    padding: 8px 10px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: 14px/1.5 ui-monospace, Consolas, monospace;
    background: var(--code-bg);
    border-radius: 6px;
  }
  .rule {
    margin-top: 6px;
  }
  .source {
    margin-top: 6px;
    max-height: 240px;
    overflow: auto;
  }
  .diff-wrap {
    min-width: 0;
  }
  .reason {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .sources,
  .signals {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 6px;
  }
  .records {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }
  .rec-head {
    flex-wrap: wrap;
    gap: 4px 8px;
    overflow-wrap: anywhere;
  }
  .sources summary {
    cursor: pointer;
    overflow-wrap: anywhere;
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    overflow-wrap: anywhere;
  }
  .path-row {
    gap: 4px;
  }
  .prefix {
    flex: none;
    font: 14px ui-monospace, Consolas, monospace;
    color: var(--muted);
  }
</style>
