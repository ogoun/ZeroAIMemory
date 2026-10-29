<script lang="ts">
  // «Подключение агента» (§15.5): готовый текст для CLAUDE.md / AGENTS.md и фрагменты
  // настроек по `GET /agent-setup`. Администратор выбирает любого пользователя, пользователь —
  // только себя. Токен можно выпустить тут же: его открытый текст попадает только в команды
  // установки и `zam init` (в блоке и `.mcp.json` — `${ZAM_TOKEN}`) и только в браузере —
  // вместо заглушки сервера `'<токен>'`, серверу в адресе он не уходит. Страница открывается
  // сразу после создания пользователя или выпуска токена (lib/agentSetup.ts). Здесь же —
  // типы подсказок хуков пользователя на сервере (`hooks.<тип>`, §10.3, R5).
  import { onMount } from 'svelte';
  import TokenShown from '../components/TokenShown.svelte';
  import {
    AGENT_KINDS,
    ApiError,
    HOOK_KINDS,
    agentSetup,
    errorText,
    getUserSettings,
    isAdminSession,
    issueOwnToken,
    issueToken,
    listSolutions,
    setUserHooks,
    type Access,
    type AgentKind,
    type AgentSetup,
    type HookFlags,
    type HookKind,
    type IssuedToken,
    type Lang,
    type Me,
    type SolutionItem,
    type User,
    withToken,
  } from '../lib/api';
  import { takeIssued } from '../lib/agentSetup';
  import { copyText } from '../lib/clipboard';

  let { me, users, onexpired }: { me: Me; users: User[]; onexpired: () => void } = $props();

  let admin = $derived(isAdminSession(me));
  const handoff = takeIssued();

  const AGENT_TITLE: Record<AgentKind, string> = {
    claude: 'Claude Code (CLAUDE.md)',
    agents: 'AGENTS.md (Codex, Cursor и др.)',
    codex: 'Codex (AGENTS.md и хуки Codex)',
  };
  const HOOK_TITLE: Record<HookKind, string> = {
    session_start: 'Дайджест при старте сессии (SessionStart)',
    prompt: 'Подсказки к запросу пользователя (UserPromptSubmit)',
    pre_tool: 'Подсказки перед правкой файла и командой (PreToolUse)',
    post_tool: 'Подсказки к ошибке инструмента (PostToolUseFailure)',
    checkpoint: 'Напоминание записать итоги (Stop, PreCompact)',
  };

  /** Выбранный пользователь; пусто — свой. */
  let picked = $state(handoff?.user ?? '');
  let userId = $derived(picked || me.user_id);
  /** Токен для команд установки и `zam init` (только что выпущенный). */
  let issued = $state<IssuedToken | null>(handoff?.token ?? null);
  /** Окно с новым токеном (показывается один раз). */
  let shown = $state<IssuedToken | null>(handoff?.token ?? null);

  let agent = $state<AgentKind>('claude');
  let solMode = $state<'auto' | 'new' | 'existing'>('auto');
  let existing = $state('');
  /** GUID нового решения: выдаёт сервер при первом запросе `solution=new`, дальше — он же. */
  let newGuid = $state('');
  let name = $state('');
  let mcp = $state(true);
  let hooks = $state(true);
  let rulesMode = $state<'server' | 'on' | 'off'>('server');
  let lang = $state<Lang>('ru');

  let setup = $state<AgentSetup | null>(null);
  let error = $state('');
  let solutions = $state<SolutionItem[]>([]);
  let tokenName = $state('');
  let tokenAccess = $state<Access>('rw');
  let tokError = $state('');
  let flags = $state<HookFlags | null>(null);
  let flagsError = $state('');
  let copied = $state('');
  let copyHint = $state('');

  let self = $derived(userId === me.user_id);
  let userName = $derived(users.find((u) => u.id === userId)?.name ?? me.name);
  /** Решения выбранного пользователя: администратору — его собственные, пользователю — все свои. */
  let userSolutions = $derived(
    admin ? solutions.filter((s) => s.owner.id === userId) : solutions,
  );

  function fail(e: unknown): string {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return '';
    }
    return errorText(e);
  }

  /** `X-Zam-User` администратора (пользователь действует только за себя). */
  const asUser = () => (admin ? userId : undefined);

  let seq = 0;
  async function load() {
    const n = ++seq;
    const solution =
      solMode === 'auto' ? 'auto' : solMode === 'new' ? newGuid || 'new' : existing || 'auto';
    try {
      const r = await agentSetup(
        {
          agent,
          solution,
          name: solMode === 'auto' ? undefined : name.trim() || undefined,
          mcp,
          hooks: agent !== 'agents' && hooks,
          rulesWrite: rulesMode === 'server' ? undefined : rulesMode === 'on',
          lang,
          tokenPlaceholder: issued !== null,
        },
        asUser(),
      );
      if (n !== seq) return;
      if (solMode === 'new' && !newGuid && r.solution) newGuid = r.solution.agent_solution_id;
      setup = r;
      error = '';
    } catch (e) {
      if (n === seq) error = fail(e);
    }
  }

  // любые опции — новый фрагмент (с небольшой задержкой при наборе имени)
  $effect(() => {
    void [agent, solMode, existing, name, mcp, hooks, rulesMode, lang, userId, issued];
    const t = setTimeout(load, 150);
    return () => clearTimeout(t);
  });

  async function loadSolutions() {
    try {
      solutions = await listSolutions();
    } catch (e) {
      error = fail(e);
    }
  }

  async function loadFlags() {
    flagsError = '';
    flags = null;
    try {
      flags = (await getUserSettings(admin ? userId : null)).hooks;
    } catch (e) {
      flagsError = fail(e);
    }
  }

  $effect(() => {
    void userId;
    loadFlags();
  });

  function pickUser(e: Event) {
    const v = (e.currentTarget as HTMLSelectElement).value;
    if (v === userId) return;
    picked = v;
    // токен другого пользователя к новому не подходит; решения — его собственные
    issued = null;
    existing = '';
    newGuid = '';
    if (solMode === 'existing') solMode = 'auto';
  }

  async function setFlag(k: HookKind, on: boolean) {
    flagsError = '';
    try {
      flags = (await setUserHooks(admin ? userId : null, { [k]: on })).hooks;
    } catch (e) {
      flagsError = fail(e);
      await loadFlags();
    }
  }

  async function issue(e: SubmitEvent) {
    e.preventDefault();
    tokError = '';
    try {
      const t =
        admin && !self
          ? await issueToken(userId, tokenName, tokenAccess)
          : await issueOwnToken(tokenName, tokenAccess);
      tokenName = '';
      issued = t;
      shown = t;
    } catch (err) {
      tokError = fail(err);
    }
  }

  const json = (v: unknown) => `${JSON.stringify(v, null, 2)}\n`;

  async function copy(key: string, text: string) {
    copyHint = '';
    copied = '';
    if (await copyText(text)) {
      copied = key;
      return;
    }
    copyHint = key;
  }

  /** Фрагменты по порядку §15.5. */
  let blocks = $derived.by(() => {
    const s = setup && issued ? withToken(setup, issued.plaintext) : setup;
    if (!s) return [];
    const out: { key: string; title: string; file?: string; text: string }[] = [
      {
        key: 'block',
        title: 'Текст для ' + (s.agent === 'claude' ? 'CLAUDE.md' : 'AGENTS.md'),
        file: s.agent === 'claude' ? 'CLAUDE.md' : 'AGENTS.md',
        text: s.block,
      },
    ];
    if (s.solution_toml)
      out.push({ key: 'toml', title: 'solution.toml', file: '.zeroaimemory/solution.toml', text: s.solution_toml });
    if (s.mcp_json) out.push({ key: 'mcp', title: 'Фрагмент .mcp.json', file: '.mcp.json', text: json(s.mcp_json) });
    if (s.claude_settings)
      out.push({ key: 'hooks', title: 'Хуки Claude Code', file: '.claude/settings.json', text: json(s.claude_settings) });
    if (s.codex_hooks)
      out.push({ key: 'hooks', title: 'Хуки Codex', file: '.codex/hooks.json', text: json(s.codex_hooks) });
    out.push(
      { key: 'install-win', title: 'Установка zam — Windows (PowerShell)', text: s.install.windows },
      { key: 'install-linux', title: 'Установка zam — Linux', text: s.install.linux },
      { key: 'init-linux', title: 'zam init — Linux, macOS (sh)', text: s.init_cmd.linux },
      { key: 'init-win', title: 'zam init — Windows (PowerShell)', text: s.init_cmd.windows },
    );
    return out;
  });

  onMount(() => {
    loadSolutions();
  });
</script>

<section class="panel setup">
  <h2>Подключение агента</h2>
  <p class="muted small">
    Готовый блок инструкции и фрагменты настроек. Токена в блоке и в <code>.mcp.json</code> нет: агент
    берёт его из <code>ZAM_TOKEN</code> или из <code>~/.zeroaimemory/config.toml</code> (пишет
    <code>zam init --token</code>). Однострочник <code>zam init</code> делает всё сам.
  </p>

  <div class="form" role="group" aria-label="Параметры подключения">
    {#if admin}
      <label>
        Пользователь
        <select value={userId} onchange={pickUser} aria-label="Пользователь">
          {#each users as u (u.id)}
            <option value={u.id}>{u.name}{u.id === me.user_id ? ' (вы)' : ''}</option>
          {/each}
        </select>
      </label>
    {:else}
      <p class="muted small">Пользователь <b>{me.name}</b>.</p>
    {/if}

    <fieldset>
      <legend>Агент</legend>
      {#each AGENT_KINDS as a (a)}
        <label class="opt"><input type="radio" name="agent" value={a} bind:group={agent} /> {AGENT_TITLE[a]}</label>
      {/each}
    </fieldset>

    <fieldset>
      <legend>Решение</legend>
      <label class="opt"><input type="radio" name="sol" value="auto" bind:group={solMode} /> Агент создаст сам (<code>zam init</code>)</label>
      <label class="opt"><input type="radio" name="sol" value="new" bind:group={solMode} /> Новое (GUID выдаст сервер)</label>
      <label class="opt">
        <input type="radio" name="sol" value="existing" bind:group={solMode} disabled={!userSolutions.length} /> Существующее
      </label>
      {#if solMode === 'existing'}
        <select bind:value={existing} aria-label="Решение">
          <option value="" disabled>— выберите —</option>
          {#each userSolutions as s (s.agent_solution_id)}
            <option value={s.agent_solution_id}>{s.name} · {s.agent_solution_id}{s.role === 'member' ? ' (участник)' : ''}</option>
          {/each}
        </select>
      {/if}
      {#if solMode !== 'auto'}
        <input placeholder={solMode === 'new' ? 'Имя решения' : 'Имя (по умолчанию — из реестра)'} bind:value={name} maxlength="128" aria-label="Имя решения" />
      {/if}
    </fieldset>

    <fieldset>
      <legend>Опции</legend>
      <label class="opt"><input type="checkbox" bind:checked={mcp} /> MCP (<code>.mcp.json</code>)</label>
      <label class="opt">
        <input type="checkbox" bind:checked={hooks} disabled={agent === 'agents'} /> Хуки
        {#if agent === 'agents'}<span class="muted small">— у AGENTS.md без Codex хуков нет</span>{/if}
      </label>
      <label>
        Запись в Rules
        <select bind:value={rulesMode} aria-label="Запись в Rules">
          <option value="server">по настройке сервера</option>
          <option value="on">разрешена</option>
          <option value="off">запрещена</option>
        </select>
      </label>
      <label>
        Язык инструкции
        <select bind:value={lang} aria-label="Язык инструкции">
          <option value="ru">русский</option>
          <option value="en">English</option>
        </select>
      </label>
    </fieldset>

    <fieldset>
      <legend>Токен</legend>
      {#if issued}
        <p class="small">
          Новый токен <code>zam_{issued.info.prefix}…</code> ({issued.info.access}) подставлен в команды установки и
          <code>zam init</code>.
        </p>
      {:else}
        <p class="muted small">
          Без нового токена в командах — заглушка <code>&lt;токен&gt;</code>: подставьте свой токен
          {userName}. Выпустить новый:
        </p>
      {/if}
      <form class="row" onsubmit={issue}>
        <input placeholder="Имя токена" bind:value={tokenName} required maxlength="64" aria-label="Имя токена" />
        <select bind:value={tokenAccess} aria-label="Доступ">
          <option value="rw">rw (чтение и запись)</option>
          <option value="ro">ro (только чтение)</option>
        </select>
        <button class="primary">Выпустить токен</button>
      </form>
      {#if tokError}<p class="error" role="alert">{tokError}</p>{/if}
    </fieldset>
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if setup}
    <p class="muted small" data-testid="setup-meta">
      Версия блока v{setup.version}{setup.template_custom ? ' (свой шаблон)' : ''} · {setup.tokens} токенов{setup.tokens_exact ? '' : ' (оценка)'}
      · адрес <code>{setup.url}</code>
      {#if setup.solution} · решение <code>{setup.solution.agent_solution_id}</code>{/if}
    </p>
    {#each setup.warnings as w (w)}<p class="warn" role="alert">{w}</p>{/each}
    {#each blocks as b (b.key)}
      <section class="frag" aria-label={b.title}>
        <header class="row">
          <h3>{b.title}</h3>
          {#if b.file}<code class="muted small">{b.file}</code>{/if}
          <button type="button" class="copy" onclick={() => copy(b.key, b.text)}>{copied === b.key ? 'Скопировано' : 'Копировать'}</button>
        </header>
        <pre>{b.text}</pre>
        {#if copied === b.key}<p class="muted small" role="status">Скопировано в буфер обмена.</p>{/if}
        {#if copyHint === b.key}<p class="error small" role="alert">Не удалось скопировать автоматически: выделите текст и нажмите Ctrl+C.</p>{/if}
      </section>
    {/each}
    <p class="small">
      Бинарники <code>zam</code> раздаёт сервер:
      <a href="downloads/zam/windows-x64" download>Windows x64</a> ·
      <a href="downloads/zam/linux-x64" download>Linux x64</a>.
    </p>
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

<section class="panel" style="margin-top: 16px;">
  <h2>Подсказки хуков{admin && !self ? ` — ${userName}` : ''}</h2>
  <p class="muted small">
    Какие подсказки хуки показывают агенту. Настройка хранится на сервере и приходит хукам при старте
    сессии; выключить тип можно и в <code>[hooks]</code> файла <code>config.toml</code> клиента — тип
    выключен, если выключен хотя бы где-то. Выключатели касаются только хуков: явный вызов MCP
    <code>memory_recall</code> с <code>event=recovery</code> или <code>event=brief</code> они не гасят
    (<code>hooks.recovery</code> не действует на <code>memory_recall event=recovery</code>).
  </p>
  {#if flagsError}<p class="error" role="alert">{flagsError}</p>{/if}
  {#if flags}
    <div class="flags" role="group" aria-label="Типы подсказок">
      {#each HOOK_KINDS as k (k)}
        <label class="opt">
          <input type="checkbox" checked={flags[k]} onchange={(e) => setFlag(k, e.currentTarget.checked)} />
          {HOOK_TITLE[k]} <code class="muted small">hooks.{k}</code>
        </label>
      {/each}
    </div>
  {:else if !flagsError}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

<TokenShown bind:shown />

<style>
  .setup h3 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .form {
    display: grid;
    gap: 12px;
    margin: 12px 0;
  }
  fieldset {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 8px 12px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  legend {
    padding: 0 4px;
    font-weight: 600;
  }
  label {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }
  .opt input {
    margin: 0;
  }
  .frag {
    margin-top: 14px;
  }
  .frag header {
    margin-bottom: 4px;
  }
  .copy {
    margin-left: auto;
    padding: 2px 10px;
  }
  pre {
    margin: 0;
    padding: 10px 12px;
    overflow: auto;
    max-height: 360px;
    background: var(--code-bg, var(--hover));
    border: 1px solid var(--border);
    border-radius: 6px;
    font: 12.5px/1.45 ui-monospace, Consolas, monospace;
    white-space: pre;
  }
  .flags {
    display: grid;
    gap: 6px;
  }
  .small {
    font-size: 12px;
  }
</style>
