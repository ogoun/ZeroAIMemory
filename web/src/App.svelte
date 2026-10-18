<script lang="ts">
  import './app.css';
  import Login from './routes/Login.svelte';
  import Users from './routes/Users.svelte';
  import { hasSession, logout, me } from './lib/api';

  // Два экрана без роутера: выбор по наличию сессии (CSRF-токен в памяти).
  let authed = $state(false);
  let adminLogin = $state('');

  async function onLogin() {
    authed = hasSession();
    try {
      adminLogin = (await me()).login;
    } catch {
      adminLogin = '';
    }
  }

  async function onLogout() {
    try {
      await logout();
    } catch {
      /* сессия уже недействительна */
    }
    authed = false;
  }
</script>

<main style="max-width: 880px; margin: 0 auto; padding: 24px 16px;">
  {#if authed}
    <header class="row" style="justify-content: space-between; margin-bottom: 16px;">
      <h1>ZeroAIMemory</h1>
      <span class="row">
        <span class="muted">{adminLogin}</span>
        <button onclick={onLogout}>Выйти</button>
      </span>
    </header>
    <Users onExpired={() => (authed = false)} />
  {:else}
    <Login {onLogin} />
  {/if}
</main>
