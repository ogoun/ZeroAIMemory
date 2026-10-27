<script lang="ts">
  import './app.css';
  import Account from './routes/Account.svelte';
  import Login from './routes/Login.svelte';
  import Shell from './routes/Shell.svelte';
  import { logout, me as fetchMe, restore, type Me } from './lib/api';

  // Вход или оболочка: выбор по наличию сессии (CSRF-токен в памяти). Пароль, заданный
  // администратором, пользователь сначала меняет (§11.4): до смены — только экран смены.
  let session = $state<Me | null>(null);
  let ready = $state(false);

  // При старте пробуем восстановить сессию по cookie; до ответа — нейтральное состояние.
  restore()
    .then((m) => (session = m))
    .catch(() => {})
    .finally(() => (ready = true));

  async function onLogin() {
    try {
      session = await fetchMe();
    } catch {
      session = null;
    }
  }

  async function onLogout() {
    try {
      await logout();
    } catch {
      /* сессия уже недействительна */
    }
    session = null;
  }
</script>

{#if !ready}
  <p class="muted boot">Загрузка…</p>
{:else if session?.must_change_password}
  <main class="login-page">
    <div class="forced">
      <Account me={session} forced onchanged={onLogin} onexpired={() => (session = null)} />
      <p><button type="button" onclick={onLogout}>Выйти</button></p>
    </div>
  </main>
{:else if session}
  <Shell me={session} onlogout={onLogout} onexpired={() => (session = null)} />
{:else}
  <main class="login-page">
    <Login {onLogin} />
  </main>
{/if}

<style>
  .boot {
    text-align: center;
    margin-top: 20vh;
  }
  .login-page {
    padding: 24px 16px;
  }
  .forced {
    max-width: 420px;
    margin: 10vh auto 0;
  }
</style>
