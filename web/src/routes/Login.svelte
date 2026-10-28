<script lang="ts">
  import { login } from '../lib/api';

  let { onLogin }: { onLogin: () => void } = $props();
  // вход общий: администратор (логин из конфигурации) или пользователь (имя, §11.4)
  let name = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      await login(name, password);
      password = '';
      onLogin();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Ошибка входа';
    } finally {
      busy = false;
    }
  }
</script>

<form class="panel" style="max-width: 360px; margin: 10vh auto 0;" onsubmit={submit}>
  <h1>Вход в ZeroAIMemory</h1>
  <div style="display: grid; gap: 10px;">
    <label>Логин <input style="width: 100%;" bind:value={name} autocomplete="username" required /></label>
    <label>
      Пароль
      <input style="width: 100%;" type="password" bind:value={password} autocomplete="current-password" required />
    </label>
    <button class="primary" disabled={busy}>Войти</button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</form>
