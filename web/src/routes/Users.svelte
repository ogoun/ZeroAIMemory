<script lang="ts">
  // Администрирование → «Пользователи»: пользователи, их токены и пароль входа в WebUI
  // (§11.4: пароль задаёт администратор, при первом входе пользователь обязан его сменить).
  // Новый пользователь или токен — сразу «Подключение агента» (§15.5) с окном токена. Новый
  // пользователь и выдача токена — формы на виду (M7.3, §6.14 правило А); «Пароль пользователя» и
  // «Закрыть вход» — окна `Prompt` на общем каркасе `FormDialog`; «Отозвать» токен необратим —
  // окно-подтверждение `danger`, фокус на «Отмена» (правило Д).
  import { onMount } from 'svelte';
  import Prompt, { type Ask } from '../components/Prompt.svelte';
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
    type TokenInfo,
    type User,
  } from '../lib/api';
  import { openAgentSetup } from '../lib/agentSetup';
  import { guardLeave } from '../lib/leaveEdit';
  import { localTime } from '../lib/time';

  let { onExpired }: { onExpired: () => void } = $props();

  let users = $state<User[]>([]);
  let tokens = $state<Record<string, TokenInfo[]>>({});
  let open = $state<string | null>(null);
  let newName = $state('');
  let tokenName = $state('');
  let tokenAccess = $state<Access>('rw');
  let error = $state('');
  let notice = $state('');
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
      openAgentSetup(r.user.id, r.token);
    });
  }

  function addToken(e: SubmitEvent, userId: string) {
    e.preventDefault();
    return guard(async () => {
      const t = await issueToken(userId, tokenName, tokenAccess);
      tokenName = '';
      openAgentSetup(userId, t);
    });
  }

  /** «Отозвать» токен — подтверждение; ошибка — в окне, окно открыто. */
  function revoke(u: User, t: TokenInfo) {
    notice = '';
    ask = {
      title: 'Отозвать токен?',
      message: `Токен «${t.name}» (zam_${t.prefix}…) пользователя «${u.name}» перестанет работать сразу: агенты и CLI с ним потеряют доступ. Вернуть его нельзя — только выпустить новый.`,
      confirm: 'Отозвать',
      danger: true,
      run: async () => {
        try {
          await revokeToken(t.id);
        } catch (e) {
          if (e instanceof ApiError && (e.status === 401 || e.status === 403)) return onExpired();
          throw e;
        }
        notice = `Токен «${t.name}» пользователя «${u.name}» отозван.`;
        await reload();
      },
      // кнопка «Отозвать» ушла вместе с отзывом — фокус на итог
      refocus: () => document.querySelector('[data-testid="users-notice"]'),
    };
  }

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

  // формы на виду (новый пользователь, выдача токена; §6.14 правило А, решение 88): набранное — в
  // стеке проверок ухода; согласие уйти — поля очищаются
  onMount(() =>
    guardLeave(
      () => !!(newName.trim() || tokenName.trim()),
      () => {
        newName = tokenName = '';
      },
    ),
  );

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
  <p class="muted notice" role="status" tabindex="-1" data-testid="users-notice">{notice}</p>

  <!-- таблица (и раскрытые строки) — в `.table-wrap` (§5.1 п. 6): прокрутка внутри, страница — нет -->
  <div class="table-wrap" style="margin-top: 12px;">
    <table aria-label="Пользователи">
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
                        <td>{t.last_used_at ? localTime(t.last_used_at) : '—'}</td>
                        <td>
                          {#if t.revoked_at}
                            <span class="muted">отозван</span>
                          {:else}
                            <button type="button" class="danger" onclick={() => revoke(u, t)}>Отозвать</button>
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
  </div>
</section>

<Prompt bind:ask />

<style>
  .notice:empty {
    margin: 0;
  }
  .acts {
    gap: 4px;
  }
  .acts button {
    padding: 2px 10px;
  }
  /* при касании — зазор между кнопками ряда (`--touch-gap`, app.css) */
  @media (pointer: coarse) {
    .acts {
      gap: var(--touch-gap);
    }
  }
</style>
