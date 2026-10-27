<script lang="ts">
  // Администрирование → «Пользователи»: пользователи, их токены и пароль входа в WebUI
  // (§11.4: пароль задаёт администратор, при первом входе пользователь обязан его сменить).
  import Prompt, { type Ask } from '../components/Prompt.svelte';
  import TokenShown from '../components/TokenShown.svelte';
  import {
    ApiError,
    clearUserPassword,
    createUser,
    issueToken,
    listTokens,
    listUsers,
    revokeToken,
    setUserPassword,
    type Access,
    type IssuedToken,
    type TokenInfo,
    type User,
  } from '../lib/api';

  let { onExpired }: { onExpired: () => void } = $props();

  let users = $state<User[]>([]);
  let tokens = $state<Record<string, TokenInfo[]>>({});
  let open = $state<string | null>(null);
  let newName = $state('');
  let tokenName = $state('');
  let tokenAccess = $state<Access>('rw');
  let error = $state('');
  let notice = $state('');
  let shown = $state<IssuedToken | null>(null);
  let ask = $state<Ask | null>(null);

  // 401/403 означает потерю сессии, остальные ошибки показываем.
  async function guard(fn: () => Promise<void>) {
    error = '';
    try {
      await fn();
    } catch (e) {
      if (e instanceof ApiError && (e.status === 401 || e.status === 403)) return onExpired();
      error = e instanceof Error ? e.message : 'Ошибка';
    }
  }

  const reload = () =>
    guard(async () => {
      users = await listUsers();
      if (open) tokens[open] = await listTokens(open);
    });

  const toggle = (id: string) =>
    guard(async () => {
      open = open === id ? null : id;
      if (open) tokens[open] = await listTokens(open);
    });

  function addUser(e: SubmitEvent) {
    e.preventDefault();
    return guard(async () => {
      const r = await createUser(newName);
      newName = '';
      shown = r.token;
      await reload();
    });
  }

  function addToken(e: SubmitEvent, userId: string) {
    e.preventDefault();
    return guard(async () => {
      shown = await issueToken(userId, tokenName, tokenAccess);
      tokenName = '';
      await reload();
    });
  }

  const revoke = (id: string) =>
    guard(async () => {
      await revokeToken(id);
      await reload();
    });

  /** Пароль входа: задать (с обязательной сменой при входе) или закрыть вход. */
  function setPassword(u: User) {
    notice = '';
    ask = {
      title: `Пароль пользователя «${u.name}»`,
      message:
        'Не короче 10 символов. Передайте пароль пользователю: при первом входе он обязан его сменить. ' +
        'Открытые сессии пользователя закрываются.',
      label: 'Пароль',
      secret: true,
      confirm: 'Задать пароль',
      run: async (pw) => {
        try {
          await setUserPassword(u.id, pw);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onExpired();
          throw e;
        }
        notice = `Пароль пользователя «${u.name}» задан: при входе потребуется смена.`;
        await reload();
      },
    };
  }

  function clearPassword(u: User) {
    notice = '';
    ask = {
      title: `Закрыть вход «${u.name}» в WebUI`,
      message: 'Пароль стирается, сессии пользователя закрываются. Токены агентов продолжают работать.',
      confirm: 'Закрыть вход',
      danger: true,
      run: async () => {
        try {
          await clearUserPassword(u.id);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onExpired();
          throw e;
        }
        notice = `Вход пользователя «${u.name}» в WebUI закрыт.`;
        await reload();
      },
    };
  }

  const passwordText = (u: User) =>
    u.is_admin ? 'из конфигурации' : !u.has_password ? 'вход закрыт' : u.must_change_password ? 'задан, смена при входе' : 'задан';

  reload();
</script>

<section class="panel">
  <h2>Пользователи</h2>
  <form class="row" onsubmit={addUser}>
    <input placeholder="Имя нового пользователя" bind:value={newName} required maxlength="64" />
    <button class="primary">Создать</button>
  </form>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="muted" role="status">{notice}</p>{/if}

  <table style="margin-top: 12px;" aria-label="Пользователи">
    <thead><tr><th>Имя</th><th>Токенов</th><th>Статус</th><th>Вход в WebUI</th><th></th></tr></thead>
    <tbody>
      {#each users as u (u.id)}
        <tr data-user={u.name}>
          <td>{u.name}{#if u.is_admin} <span class="muted">(админ)</span>{/if}</td>
          <td>{u.tokens}</td>
          <td>{u.disabled_at ? 'отключён' : 'активен'}</td>
          <td class="muted">{passwordText(u)}</td>
          <td class="row acts">
            <button onclick={() => toggle(u.id)}>{open === u.id ? 'Скрыть' : 'Токены'}</button>
            {#if !u.is_admin}
              <button onclick={() => setPassword(u)}>{u.has_password ? 'Сменить пароль' : 'Задать пароль'}</button>
              {#if u.has_password}
                <button class="danger" onclick={() => clearPassword(u)}>Закрыть вход</button>
              {/if}
            {/if}
          </td>
        </tr>
        {#if open === u.id}
          <tr>
            <td colspan="5">
              <table>
                <thead><tr><th>Имя</th><th>Префикс</th><th>Доступ</th><th>Использован</th><th></th></tr></thead>
                <tbody>
                  {#each tokens[u.id] ?? [] as t (t.id)}
                    <tr>
                      <td>{t.name}</td>
                      <td><code>zam_{t.prefix}…</code></td>
                      <td>{t.access}</td>
                      <td>{t.last_used_at ?? '—'}</td>
                      <td>
                        {#if t.revoked_at}
                          <span class="muted">отозван</span>
                        {:else}
                          <button class="danger" onclick={() => revoke(t.id)}>Отозвать</button>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
              {#if !u.disabled_at}
                <form class="row" style="margin-top: 8px;" onsubmit={(e) => addToken(e, u.id)}>
                  <input placeholder="Имя токена" bind:value={tokenName} required maxlength="64" />
                  <select bind:value={tokenAccess} aria-label="Доступ">
                    <option value="rw">rw (чтение и запись)</option>
                    <option value="ro">ro (только чтение)</option>
                  </select>
                  <button class="primary">Выпустить токен</button>
                </form>
              {/if}
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
</section>

<TokenShown bind:shown />
<Prompt bind:ask />

<style>
  .acts {
    gap: 4px;
  }
  .acts button {
    padding: 2px 10px;
  }
</style>
