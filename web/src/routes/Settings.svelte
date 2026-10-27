<script lang="ts">
  // Администрирование → «Настройки»: rules.agent_write — разрешить запись в Rules по токенам
  // rw (агентам); «Шаблон инструкции» (§15.5) — правка блока для CLAUDE.md / AGENTS.md с
  // предпросмотром (`POST /admin/agent-setup/preview`), сбросом к умолчанию и номером версии.
  // Изменения записываются в аудит (settings.update).
  import {
    AGENT_KINDS,
    ApiError,
    LANGS,
    errorText,
    getSettings,
    previewTemplate,
    saveTemplate,
    setAgentWrite,
    type AgentKind,
    type Lang,
    type Settings,
    type TemplatePreview,
  } from '../lib/api';

  let { onexpired }: { onexpired: () => void } = $props();

  let settings = $state<Settings | null>(null);
  let error = $state('');
  let busy = $state(false);
  let saved = $state('');

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = errorText(e);
  }

  async function load() {
    error = '';
    try {
      settings = await getSettings();
    } catch (e) {
      fail(e);
    }
  }

  async function toggle(e: Event) {
    const on = (e.currentTarget as HTMLInputElement).checked;
    busy = true;
    error = '';
    saved = '';
    try {
      settings = await setAgentWrite(on);
      saved = on ? 'Запись в Rules по токенам rw разрешена.' : 'Запись в Rules — только из WebUI.';
    } catch (err) {
      fail(err);
      await load();
    } finally {
      busy = false;
    }
  }

  // ---- шаблон инструкции ----

  const AGENT_TITLE: Record<AgentKind, string> = { claude: 'Claude Code', agents: 'AGENTS.md', codex: 'Codex' };
  let tplAgent = $state<AgentKind>('claude');
  let tplLang = $state<Lang>('ru');
  let tplKey = $derived(`${tplAgent}.${tplLang}`);
  let draft = $state('');
  /** Ключ, текст которого сейчас в поле (смена ключа — новый текст). */
  let draftKey = $state('');
  let preview = $state<TemplatePreview | null>(null);
  let previewError = $state('');
  let tplError = $state('');
  let tplSaved = $state('');
  let tplBusy = $state(false);

  let current = $derived(settings?.agent_setup.templates[tplKey] ?? null);
  let dirty = $derived(!!current && draft !== current.text);

  $effect(() => {
    // новый шаблон в поле: при первой загрузке и при смене агента или языка
    const c = current;
    if (c && draftKey !== tplKey) {
      draft = c.text;
      draftKey = tplKey;
      tplSaved = '';
      tplError = '';
    }
  });

  let pseq = 0;
  $effect(() => {
    const text = draft;
    const [a, l] = [tplAgent, tplLang];
    if (!current) return;
    const t = setTimeout(async () => {
      const n = ++pseq;
      try {
        const p = await previewTemplate(a, l, text);
        if (n !== pseq) return;
        preview = p;
        previewError = '';
      } catch (e) {
        if (n !== pseq) return;
        if (e instanceof ApiError && e.status === 401) return onexpired();
        preview = null;
        previewError = errorText(e);
      }
    }, 300);
    return () => clearTimeout(t);
  });

  async function saveTpl(text: string | null) {
    tplBusy = true;
    tplError = '';
    tplSaved = '';
    const before = settings?.agent_setup.version;
    try {
      settings = await saveTemplate(tplKey, text);
      const c = settings.agent_setup.templates[tplKey];
      draft = c.text;
      const v = settings.agent_setup.version;
      tplSaved =
        v === before
          ? 'Текст не изменился — версия прежняя.'
          : text === null
            ? `Шаблон сброшен к умолчанию, версия блока — v${v}.`
            : `Шаблон сохранён, версия блока — v${v}.`;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      tplError = errorText(e);
    } finally {
      tplBusy = false;
    }
  }

  load();
</script>

<section class="panel">
  <h2>Настройки</h2>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if settings}
    <label class="opt">
      <input type="checkbox" checked={settings.rules.agent_write} disabled={busy} onchange={toggle} />
      <span>
        <strong>Агенты могут писать в Rules</strong> <code>rules.agent_write</code>
        <span class="muted small">
          Выключено — правила (Rules) меняют только люди в WebUI (администратор и пользователь в своей
          памяти), запись по любому токену отклоняется (403). Включено — писать в Rules могут и токены rw.
        </span>
      </span>
    </label>
    {#if saved}<p class="muted" role="status">{saved}</p>{/if}
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

{#if settings}
  <section class="panel tpl" style="margin-top: 16px;" aria-label="Шаблон инструкции">
    <h2>Шаблон инструкции</h2>
    <p class="muted small">
      Блок для CLAUDE.md / AGENTS.md (§14.2). Подстановки: <code>{'{url}'}</code> — адрес сервера (обязательна),
      <code>{'{solution}'}</code> — решение, <code>{'{rules}'}</code> — можно ли агенту писать в Rules. Маркеры
      <code>&lt;!-- zeroaimemory:… --&gt;</code> добавляет сервер. Версия блока одна на сервер и растёт при каждом
      сохранении, которое меняет текст; агенты с устаревшим блоком получают напоминание.
    </p>
    <div class="row">
      <span>Версия блока: <b data-testid="tpl-version">v{settings.agent_setup.version}</b></span>
      <select bind:value={tplAgent} aria-label="Агент шаблона">
        {#each AGENT_KINDS as a (a)}<option value={a}>{AGENT_TITLE[a]}</option>{/each}
      </select>
      <select bind:value={tplLang} aria-label="Язык шаблона">
        {#each LANGS as l (l)}<option value={l}>{l}</option>{/each}
      </select>
      <span class="muted small">{current?.custom ? 'свой шаблон' : 'шаблон по умолчанию'}</span>
    </div>
    <textarea bind:value={draft} rows="12" spellcheck="false" aria-label="Текст шаблона"></textarea>
    <div class="row">
      <button class="primary" disabled={tplBusy || !dirty || !!previewError} onclick={() => saveTpl(draft)}>Сохранить</button>
      <button disabled={tplBusy || !current?.custom} onclick={() => saveTpl(null)}>Сбросить к умолчанию</button>
      {#if dirty}<span class="muted small">есть несохранённые изменения</span>{/if}
    </div>
    {#if tplError}<p class="error" role="alert">{tplError}</p>{/if}
    {#if tplSaved}<p class="muted" role="status">{tplSaved}</p>{/if}
    <h3>Предпросмотр</h3>
    {#if previewError}
      <p class="error" role="alert">{previewError}</p>
    {:else if preview}
      <p class="muted small">
        {preview.tokens} токенов · после сохранения — v{preview.version_next} · пример решения
      </p>
      {#each preview.warnings as w (w)}<p class="warn" role="alert">{w}</p>{/each}
      <pre data-testid="tpl-preview">{preview.block}</pre>
    {/if}
  </section>
{/if}

<style>
  .opt {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .opt input {
    margin-top: 4px;
  }
  .opt > span {
    display: grid;
    gap: 2px;
  }
  .small {
    font-size: 12px;
  }
  .tpl textarea {
    width: 100%;
    margin: 8px 0;
    font: 12.5px/1.45 ui-monospace, Consolas, monospace;
    box-sizing: border-box;
  }
  .tpl h3 {
    margin: 12px 0 4px;
    font-size: 14px;
  }
  .tpl pre {
    margin: 0;
    padding: 10px 12px;
    overflow: auto;
    background: var(--hover);
    border: 1px solid var(--border);
    border-radius: 6px;
    font: 12.5px/1.45 ui-monospace, Consolas, monospace;
  }
</style>
