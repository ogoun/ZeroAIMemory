<script lang="ts">
  // «Подключение агента» (§15.5): готовый текст для CLAUDE.md / AGENTS.md и фрагменты
  // настроек по `GET /agent-setup`. Администратор выбирает любого пользователя, пользователь —
  // только себя. Токен можно выпустить тут же: его открытый текст попадает только в команды
  // установки и `zam init` (в блоке и `.mcp.json` его нет: MCP — stdio `zam mcp`, M6) и только в браузере —
  // вместо заглушки сервера `'<токен>'`, серверу в адресе он не уходит. Страница открывается
  // сразу после создания пользователя или выпуска токена (lib/agentSetup.ts). Здесь же —
  // типы подсказок хуков пользователя на сервере (`hooks.<тип>`, §10.3, R5), проактивность идей
  // (`ideas.proactive`, M5.7 §8: раздел дайджеста об идеях и подсказки идей) и режим идей
  // (`ideas.mode`, M6.5 §7: «Идеи: разбирать сам / предлагать мне»). Галочка «Подключить с
  // переносом данных» (M7.6 §9.1) — задание агенту на перенос проекта от сервера
  // (`migration_prompt`); сервер без признака `agent_setup_migrate` — галочка выключена с пояснением.
  // Выпуск токена (решения координатора 141–142): «Название токена» заполнено «<агент> · <решение> ·
  // <дата>», пока его не правили; выпущенный здесь токен можно тут же отозвать — команды
  // возвращаются к заглушке.
  import { onMount } from 'svelte';
  import Prompt, { type Ask } from '../components/Prompt.svelte';
  import TokenNameField from '../components/TokenNameField.svelte';
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
    revokeOwnToken,
    revokeToken,
    serverFeatures,
    setUserHooks,
    setUserIdeas,
    type Access,
    type AgentKind,
    type AgentSetup,
    type HookFlags,
    type HookKind,
    type IdeasMode,
    type IssuedToken,
    type Lang,
    type Me,
    type SolutionItem,
    type User,
    withToken,
  } from '../lib/api';
  import { takeIssued } from '../lib/agentSetup';
  import { copyText } from '../lib/clipboard';
  import { href } from '../lib/router';
  import { defaultTokenName } from '../lib/tokenName';

  let { me, users, onexpired }: { me: Me; users: User[]; onexpired: () => void } = $props();

  let admin = $derived(isAdminSession(me));
  const handoff = takeIssued();

  const AGENT_TITLE: Record<AgentKind, string> = {
    claude: 'Claude Code (CLAUDE.md)',
    agents: 'AGENTS.md (Codex, Cursor и др.)',
    codex: 'Codex (AGENTS.md и хуки Codex)',
  };
  /** Агент в названии токена по умолчанию. */
  const AGENT_SHORT: Record<AgentKind, string> = { claude: 'Claude Code', agents: 'AGENTS.md', codex: 'Codex' };
  const HOOK_TITLE: Record<HookKind, string> = {
    session_start: 'Дайджест при старте сессии (SessionStart)',
    prompt: 'Подсказки к запросу пользователя (UserPromptSubmit)',
    pre_tool: 'Подсказки перед правкой файла и командой (PreToolUse)',
    post_tool: 'Подсказки к ошибке инструмента (PostToolUseFailure)',
    checkpoint: 'Напоминание записать итоги (Stop, PreCompact)',
    subagent_start: 'Бриф субагенту при его запуске (SubagentStart)',
    file_context: 'Знание о файле перед правкой и после чтения (PreToolUse, PostToolUse)',
    session_journal: 'Журнал сессии и подсказки по нему (PostToolUse)',
    recovery: 'Восстановление после сжатия контекста и возобновления (SessionStart)',
    code_map: 'Карта кода: строка в дайджесте, раздел «Код» брифа, фоновая синхронизация',
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
  /** «Подключить с переносом данных»: задание агенту на перенос проекта. */
  let migrate = $state(false);
  /** Сервер умеет задание на перенос (`agent_setup_migrate`); `null` — ещё не узнали. */
  let migrateOk = $state<boolean | null>(null);

  let setup = $state<AgentSetup | null>(null);
  let error = $state('');
  let solutions = $state<SolutionItem[]>([]);
  let tokenName = $state('');
  /** Название токена правил человек: значение по умолчанию больше не подставляется. */
  let nameEdited = $state(false);
  let nameError = $state('');
  let nameInput = $state<HTMLInputElement>();
  let tokenAccess = $state<Access>('rw');
  let tokError = $state('');
  /** Итог отзыва выпущенного здесь токена. */
  let tokNotice = $state('');
  let ask = $state<Ask | null>(null);
  let flags = $state<HookFlags | null>(null);
  /** `ideas.proactive` пользователя (`null` — ещё не прочитано). */
  let ideasOn = $state<boolean | null>(null);
  /** `ideas.mode` пользователя: `review` — разбирать сам, `propose` — предлагать мне. */
  let ideasMode = $state<IdeasMode>('review');
  let flagsError = $state('');
  let copied = $state('');
  let copyHint = $state('');

  let self = $derived(userId === me.user_id);
  let userName = $derived(users.find((u) => u.id === userId)?.name ?? me.name);
  /** Решения выбранного пользователя: администратору — его собственные, пользователю — все свои. */
  let userSolutions = $derived(
    admin ? solutions.filter((s) => s.owner.id === userId) : solutions,
  );
  /** Решение в названии токена по умолчанию. */
  let solTitle = $derived(
    name.trim() ||
      (solMode === 'existing' ? userSolutions.find((s) => s.agent_solution_id === existing)?.name : undefined) ||
      'новое решение',
  );
  let tokenDefault = $derived(defaultTokenName(AGENT_SHORT[agent], solTitle, new Date()));

  // название по умолчанию следует за агентом и решением, пока человек его не правил
  $effect(() => {
    if (!nameEdited) tokenName = tokenDefault;
  });

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
          migrate: migrate && migrateOk === true,
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
    void [agent, solMode, existing, name, mcp, hooks, rulesMode, lang, userId, issued, migrate, migrateOk];
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
    ideasOn = null;
    try {
      const s = await getUserSettings(admin ? userId : null);
      flags = s.hooks;
      ideasOn = s.ideas?.proactive ?? true;
      ideasMode = s.ideas?.mode === 'propose' ? 'propose' : 'review';
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

  async function setIdeas(on: boolean) {
    flagsError = '';
    try {
      ideasOn = (await setUserIdeas(admin ? userId : null, { proactive: on })).ideas.proactive;
    } catch (e) {
      flagsError = fail(e);
      await loadFlags();
    }
  }

  async function setIdeasMode(m: IdeasMode) {
    flagsError = '';
    try {
      const r = await setUserIdeas(admin ? userId : null, { mode: m });
      ideasMode = r.ideas.mode === 'propose' ? 'propose' : 'review';
    } catch (e) {
      flagsError = fail(e);
      await loadFlags();
    }
  }

  async function issue(e: SubmitEvent) {
    e.preventDefault();
    tokError = '';
    tokNotice = '';
    if (!tokenName.trim()) {
      nameError = 'Укажите название токена';
      nameInput?.focus();
      return;
    }
    try {
      const t =
        admin && !self
          ? await issueToken(userId, tokenName, tokenAccess)
          : await issueOwnToken(tokenName, tokenAccess);
      nameEdited = false;
      tokenName = tokenDefault;
      issued = t;
      shown = t;
    } catch (err) {
      tokError = fail(err);
    }
  }

  /** «Отозвать этот токен» — подтверждение тем же окном, что в «Аккаунте»; ошибка — в окне. После
   * отзыва в командах снова заглушка `<токен>`. */
  function revokeIssued() {
    const t = issued;
    if (!t) return;
    tokError = '';
    tokNotice = '';
    ask = {
      title: 'Отозвать токен?',
      message: `Токен «${t.info.name}» (zam_${t.info.prefix}…) перестанет работать сразу: агенты и CLI с ним потеряют доступ. Вернуть его нельзя — только выпустить новый.`,
      confirm: 'Отозвать',
      danger: true,
      run: async () => {
        try {
          if (admin && t.info.user_id !== me.user_id) await revokeToken(t.info.id);
          else await revokeOwnToken(t.info.id);
        } catch (err) {
          if (err instanceof ApiError && err.status === 401) return onexpired();
          throw new Error(errorText(err));
        }
        if (issued?.info.id === t.info.id) issued = null;
        if (shown?.info.id === t.info.id) shown = null;
        tokNotice = `Токен «${t.info.name}» отозван: в командах снова заглушка <токен>.`;
      },
      // кнопка ушла вместе с отзывом — фокус на итог
      refocus: () => document.querySelector('[data-testid="setup-token-notice"]'),
    };
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

  /** Текст задания на перенос (только при выбранной галочке и ответе сервера с полем). */
  let migration = $derived(migrate && migrateOk ? (setup?.migration_prompt ?? '') : '');

  onMount(() => {
    loadSolutions();
    serverFeatures()
      .then((f) => (migrateOk = f.includes('agent_setup_migrate')))
      .catch(() => (migrateOk = false));
  });
</script>

<section class="panel setup">
  <h2>Подключение агента</h2>
  <p class="muted small">
    Готовый блок инструкции и фрагменты настроек. Токена в блоке и в <code>.mcp.json</code> нет: MCP
    подключается через <code>zam mcp</code>, а он, как <code>zam</code> и хуки, берёт токен из
    <code>~/.zeroaimemory/config.toml</code> (пишет <code>zam init --token</code>) или <code>ZAM_TOKEN</code>.
    Однострочник <code>zam init</code> делает всё сам. Старый <code>.mcp.json</code> с HTTP работает, но без
    учёта сессии — пересоздайте его через <code>zam init</code> или фрагмент ниже.
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

    <div class="migrate-opt">
      <label class="opt">
        <input type="checkbox" bind:checked={migrate} disabled={migrateOk !== true} aria-describedby="migrate-note" />
        Подключить с переносом данных
      </label>
      <p class="muted small" id="migrate-note">
        {#if migrateOk === false}
          Сервер старше 0.0.19: задания на перенос нет — обновите сервер.
        {:else}
          Агенту — ещё задание перенести в память всё, что уже наработано по проекту.
        {/if}
      </p>
    </div>

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
        <div class="row issued">
          <p class="small">
            Новый токен <code>zam_{issued.info.prefix}…</code> ({issued.info.access}) подставлен в команды установки и
            <code>zam init</code>.
          </p>
          <button type="button" class="danger" onclick={revokeIssued}>Отозвать этот токен</button>
        </div>
      {:else}
        <p class="muted small">
          Без нового токена в командах — заглушка <code>&lt;токен&gt;</code>: подставьте свой токен
          {userName}. Выпустить новый:
        </p>
      {/if}
      <p class="small notice" role="status" tabindex="-1" data-testid="setup-token-notice">{tokNotice}</p>
      <form class="token-form" onsubmit={issue} novalidate aria-label="Выпуск токена">
        <TokenNameField
          bind:value={tokenName}
          bind:input={nameInput}
          error={nameError}
          oninput={() => {
            nameEdited = true;
            nameError = '';
          }}
        />
        <div class="row">
          <select bind:value={tokenAccess} aria-label="Доступ">
            <option value="rw">rw (чтение и запись)</option>
            <option value="ro">ro (только чтение)</option>
          </select>
          <button class="primary">Выпустить токен</button>
        </div>
      </form>
      {#if tokError}<p class="error" role="alert">{tokError}</p>{/if}
      <p class="muted small">
        Все токены — <a href={href({ name: 'account' })}>«Аккаунт» → «Мои токены»</a>{#if admin}; токены
          пользователей — <a href={href({ name: 'admin' })}>«Администрирование» → «Пользователи»</a>{/if}.
      </p>
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
    {#each blocks as b, i (b.key)}
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
      {#if i === 0 && migration}
        <!-- M7.6 §9.1: задание на перенос — сразу под текстом подключения -->
        <section class="frag migrate" aria-label="Задание агенту на перенос">
          <header class="row">
            <h3>Задание агенту на перенос</h3>
            <button type="button" class="copy" onclick={() => copy('migrate', migration)}>{copied === 'migrate' ? 'Скопировано' : 'Копировать'}</button>
          </header>
          <p class="muted small">
            Дайте агенту в проекте: он подключит проект и перенесёт в память свою память и историю сессий, карту
            кода, решения, ловушки, задачи и термины. Это займёт время и токены; секреты — только в Vault;
            кандидатов в Rules и аксиомы принимаете вы.
          </p>
          <textarea readonly rows="14" spellcheck="false" aria-label="Текст задания на перенос" value={migration}></textarea>
          {#if copied === 'migrate'}<p class="muted small" role="status">Скопировано в буфер обмена.</p>{/if}
          {#if copyHint === 'migrate'}<p class="error small" role="alert">Не удалось скопировать автоматически: выделите текст и нажмите Ctrl+C.</p>{/if}
        </section>
      {/if}
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
    (<code>hooks.recovery</code> не действует на <code>memory_recall event=recovery</code>), а
    <code>hooks.code_map</code> — на <code>memory_code</code> и <code>zam code</code>.
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
      {#if ideasOn !== null}
        <label class="opt">
          <input type="checkbox" checked={ideasOn} onchange={(e) => setIdeas(e.currentTarget.checked)} />
          Идеи: раздел дайджеста и подсказки (разбор и предложения) <code class="muted small">ideas.proactive</code>
        </label>
        <div class="opt row" role="radiogroup" aria-label="Идеи: разбирать сам / предлагать мне">
          Идеи:
          {#each [['review', 'разбирать сам'], ['propose', 'предлагать мне']] as const as [m, title] (m)}
            <label class="opt">
              <input type="radio" name="ideas-mode" value={m} checked={ideasMode === m} onchange={() => setIdeasMode(m)} />
              {title}
            </label>
          {/each}
          <code class="muted small">ideas.mode</code>
        </div>
        <p class="muted small">
          «Разбирать сам» — агент сам разбирает новые идеи: вносит в план («Задачи»), возвращает на доработку с
          вопросами или отклоняет с причиной; разбор виден в карточке идеи. «Предлагать мне» — агент предлагает
          идею вопросом, решаете вы. В общем решении действует режим его владельца. Выключенные подсказки
          идей гасят и то и другое.
        </p>
      {/if}
    </div>
  {:else if !flagsError}
    <p class="muted">Загрузка…</p>
  {/if}
  <section class="snapshot-keys" aria-label="Снимок сессии">
    <h3>Снимок сессии</h3>
    <p class="muted small">
      Хуки снимают из журнала агента (без LLM) хвост конспекта сжатия, последние запросы, изменённые
      файлы и команды — на событиях <code>PreCompact</code>, <code>PostCompact</code>,
      <code>SessionStart</code> (compact), <code>Stop</code> и <code>SessionEnd</code>; секреты
      маскируются. Снимок оборванной сессии приходит следующей сессии разделом «Последнее известное
      состояние». Ключи клиента — в <code>[hooks]</code> файла <code>config.toml</code>:
      <code>hooks.session_snapshot</code> (вкл.; выкл. — журнал агента не открывается),
      <code>snapshot_tail_kb</code> (2048 — КиБ хвоста журнала), <code>snapshot_ms</code> (25 — срок
      сборки, 5–50 мс). Сервер: <code>recall.snapshot_tokens</code> (400; 0 — не показывать),
      <code>[snapshot] keep_days</code> (14). Снимок виден на странице «Сессии».
    </p>
  </section>
</section>

<!-- M7.3 (§6.9 п. 1): ссылка на исполнителя внутренних работ -->
<section class="panel" style="margin-top: 16px;" aria-labelledby="setup-works-h">
  <h2 id="setup-works-h">Внутренние работы</h2>
  <p class="muted small">
    Пробные вопросы ночной проверки и кандидаты в Rules может делать агент в своей сессии (строка дайджеста
    предложит <code>memory_recall event=work</code>) или подключённая модель; по умолчанию — выкл. Режим, подключение
    модели, очередь и журнал отправок — на странице <a href={'#/works'}>Внутренние работы</a>.
  </p>
</section>

<TokenShown bind:shown />
<Prompt bind:ask />

<style>
  .snapshot-keys h3 {
    margin: 12px 0 0;
    font-size: 14px;
    font-weight: 600;
  }
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
    /* по умолчанию у fieldset `min-inline-size: min-content`: длинный пункт списка (имя решения и
       GUID) раздвигал бы форму шире телефона */
    min-width: 0;
    margin: 0;
    padding: 8px 12px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  fieldset select,
  fieldset input:not([type='radio'], [type='checkbox']) {
    max-width: 100%;
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
  .token-form {
    display: grid;
    gap: 8px;
  }
  .issued {
    align-items: center;
  }
  .issued p {
    margin: 0;
  }
  .notice:empty {
    margin: 0;
  }
  .migrate-opt {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .migrate-opt p {
    margin: 0;
  }
  .migrate p {
    margin: 0 0 6px;
  }
  .migrate textarea {
    display: block;
    box-sizing: border-box;
    width: 100%;
    max-width: 100%;
    max-height: 420px;
    margin: 0;
    padding: 10px 12px;
    overflow: auto;
    resize: vertical;
    background: var(--code-bg, var(--hover));
    color: inherit;
    border: 1px solid var(--border);
    border-radius: 6px;
    font: 12.5px/1.45 ui-monospace, Consolas, monospace;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
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
