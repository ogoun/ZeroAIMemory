<script lang="ts">
  // Администрирование → «Настройки»: rules.agent_write — разрешить запись в Rules по токенам
  // rw (агентам). Изменение записывается в аудит (settings.update).
  import { ApiError, errorText, getSettings, setAgentWrite, type Settings } from '../lib/api';

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
          Выключено — правила (Rules) меняет только администратор в WebUI, запись по любому токену
          отклоняется (403). Включено — писать в Rules могут и токены rw.
        </span>
      </span>
    </label>
    {#if saved}<p class="muted" role="status">{saved}</p>{/if}
  {:else if !error}
    <p class="muted">Загрузка…</p>
  {/if}
</section>

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
</style>
