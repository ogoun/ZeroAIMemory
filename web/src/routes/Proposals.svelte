<script lang="ts">
  // «Кандидаты в Rules» (#/proposals[/<o:id>]; M7.3, спецификация §6.9 п. 3, §6.7 п. 4; ленивый
  // чанк): правила, которые агент или подключённая модель предлагают в Rules, — решает только
  // человек. Список и карточка — `JournalSplit` (§5.1 п. 5): рядом в широкой области, карточка
  // вместо списка в узкой; карточка — путь, текст, пометка команд shell, довод, основание с
  // исходными текстами (раскрываются по запросу), автор (сессия агента или имя модели), ряд
  // решения под шапкой (правило Г §6.14). «Принять» — окно формы (`FormDialog`, правило Б) с правкой
  // пути и текста, суммой Rules в дайджесте и приростом; «Отклонить» — окно с причиной по желанию.
  // После «Принять» — строка со ссылкой на созданную заметку, карточка — «принят» (§6.14 п. 6).
  // Числа «кандидаты: N» дерева и шапки перечитываются сами (`changesCounts`).
  import FormDialog from '../components/FormDialog.svelte';
  import JournalSplit from '../components/JournalSplit.svelte';
  import { ApiError, errorText, getEntry, getNode, type Me } from '../lib/api';
  import { isDirty } from '../lib/formDraft';
  import { go, href } from '../lib/router';
  import { localTime } from '../lib/time';
  import {
    acceptProposal,
    declineProposal,
    getProposal,
    listProposals,
    refRoute,
    type Proposal,
    type ProposalCard,
  } from '../lib/worksApi';
  import { PROPOSAL_STATUS_TEXT, proposalAuthorText, rulesTokensText } from '../lib/worksText';

  let { me, proposal, onexpired }: { me: Me; proposal: string | null; onexpired: () => void } = $props();

  let items = $state<Proposal[]>([]);
  let loaded = $state(false);
  let error = $state('');
  let filter = $state<'' | Proposal['status']>('proposed');
  let card = $state<ProposalCard | null>(null);
  let cardError = $state('');
  /** Итог решения в карточке; у принятия — ссылка на созданную заметку. */
  let decided = $state<{ text: string; note?: { id: string; path: string } } | null>(null);
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
    error = '';
    try {
      const r = await listProposals(filter || undefined);
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
    void filter;
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
    } catch (e) {
      if (my !== cseq) return;
      if (expired(e)) return;
      card = null;
      cardError = e instanceof ApiError && e.status === 404 ? `Кандидата ${id} нет.` : errorText(e);
    }
  }

  $effect(() => {
    const id = proposal;
    void loadCard(id);
  });

  const select = (id: string) => go({ name: 'proposals', proposal: proposal === id ? null : id });

  // ---- основание: исходные тексты по запросу ----
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

  // ---- «Принять» (окно формы с правкой пути и текста) ----
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
    decided = { text: 'Кандидат отклонён: его источник больше не предлагается.' };
    await Promise.all([loadCard(c.id, true), loadList()]);
  }

  let contentEdited = $derived(!!card && aContent.trim() !== card.content.trim());
  const noteHref = (id: string) => href({ name: 'node', user: me.user_id, space: 'rules', node: id, anchor: null });
  const refId = (r: string | null) => (r ?? '').replace(/^n:/, '');
</script>

<section class="panel proposals-page" aria-labelledby="proposals-h">
  <h2 id="proposals-h">Кандидаты в Rules</h2>
  <p class="muted small">
    Правила, которые агент или подключённая модель предлагают в Rules — поведение для всех проектов агента. Rules
    приходят агенту в каждой сессии, поэтому кандидата принимает или отклоняет только человек; заметка Rules
    создаётся от вашего имени, происхождение — в её истории.
  </p>
  {#if error}<p class="error" role="alert">Не удалось получить кандидатов: {error}</p>{/if}

  <JournalSplit
    open={card !== null || cardError !== ''}
    back={{ href: href({ name: 'proposals', proposal: null }), label: 'Кандидаты в Rules' }}
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
          <select bind:value={filter} aria-label="Статус кандидатов">
            <option value="proposed">ждут решения</option>
            <option value="accepted">принятые</option>
            <option value="declined">отклонённые</option>
            <option value="">все</option>
          </select>
        </label>
      </div>
      {#if loaded && !items.length}
        <p class="muted">{filter === 'proposed' ? 'Кандидатов, ждущих решения, нет.' : 'Кандидатов нет.'}</p>
      {:else if !loaded && !error}
        <p class="muted">Загрузка…</p>
      {/if}
      <ul class="plist" aria-label="Кандидаты">
        {#each items as p (p.id)}
          <li class:selected={p.id === proposal} data-entry={p.id} data-proposal={p.id}>
            <button type="button" class="title" onclick={() => select(p.id)}>rules:/{p.path}</button>
            <div class="muted small preview">{p.content.split('\n').find((l) => l.trim()) ?? ''}</div>
            <div class="muted small">
              {PROPOSAL_STATUS_TEXT[p.status] ?? p.status} · {proposalAuthorText(p.author_kind, p.author, p.session)} · {localTime(p.created_at)}
              {#if p.shell}<span class="chip shell">команды shell</span>{/if}
            </div>
          </li>
        {/each}
      </ul>
    {/snippet}
    {#snippet detail()}
      {#if cardError}<p class="error" role="alert">{cardError}</p>{/if}
      {#if card}
        {@const c = card}
        <article class="card panel" aria-label="Карточка кандидата">
          <header>
            <h2>rules:/{c.path}</h2>
            <div class="row meta">
              <span class="chip status" data-testid="proposal-status">{PROPOSAL_STATUS_TEXT[c.status] ?? c.status}</span>
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
              {PROPOSAL_STATUS_TEXT[c.status] ?? c.status} {localTime(c.decided_at)}{c.decided_by ? ` (${c.decided_by})` : ''}{c.decision_note ? `: ${c.decision_note}` : ''}
              {#if c.result_ref}· заметка <a href={noteHref(refId(c.result_ref))}>{c.result_ref}</a>{/if}
            </p>
          {/if}
        </article>
      {/if}
    {/snippet}
  </JournalSplit>
</section>

<FormDialog
  open={acceptOpen}
  title="Принять кандидата"
  subject={card ? `rules:/${card.path}` : undefined}
  description="Заметка Rules создаётся от вашего имени, в её истории — «из предложения». Путь и текст можно поправить."
  submit="Принять"
  wide
  refocus={() => document.querySelector('[data-testid="proposal-decided"]')}
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
  open={declineOpen}
  title="Отклонить кандидата"
  subject={card ? `rules:/${card.path}` : undefined}
  description="Отклонённый кандидат остаётся в списке «отклонённые»; источник внутренней работы больше не предлагается ни в какой ревизии."
  submit="Отклонить"
  danger
  refocus={() => document.querySelector('[data-testid="proposal-decided"]')}
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
  .chip.shell {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .card header h2 {
    margin: 0 0 6px;
    overflow-wrap: anywhere;
  }
  .meta {
    gap: 6px 10px;
  }
  .actions {
    margin: 10px 0 4px;
  }
  .decided:empty {
    margin: 0;
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
  .source {
    margin-top: 6px;
    max-height: 240px;
    overflow: auto;
  }
  .reason {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .sources {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 6px;
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
