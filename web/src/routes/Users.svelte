<script lang="ts">
  import { Dialog } from 'bits-ui';
  import {
    ApiError,
    createUser,
    issueToken,
    listTokens,
    listUsers,
    revokeToken,
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
  let shown = $state<IssuedToken | null>(null);
  let copied = $state(false);

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
      show(r.token);
      await reload();
    });
  }

  function addToken(e: SubmitEvent, userId: string) {
    e.preventDefault();
    return guard(async () => {
      show(await issueToken(userId, tokenName, tokenAccess));
      tokenName = '';
      await reload();
    });
  }

  const revoke = (id: string) =>
    guard(async () => {
      await revokeToken(id);
      await reload();
    });

  function show(t: IssuedToken) {
    copied = false;
    shown = t;
  }

  async function copy() {
    if (!shown) return;
    try {
      await navigator.clipboard.writeText(shown.plaintext);
      copied = true;
    } catch {
      error = 'Не удалось скопировать: выделите токен и скопируйте вручную';
    }
  }

  reload();
</script>

<section class="panel">
  <h2>Пользователи</h2>
  <form class="row" onsubmit={addUser}>
    <input placeholder="Имя нового пользователя" bind:value={newName} required maxlength="64" />
    <button class="primary">Создать</button>
  </form>
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <table style="margin-top: 12px;">
    <thead><tr><th>Имя</th><th>Токенов</th><th>Статус</th><th></th></tr></thead>
    <tbody>
      {#each users as u (u.id)}
        <tr>
          <td>{u.name}{#if u.is_admin} <span class="muted">(админ)</span>{/if}</td>
          <td>{u.tokens}</td>
          <td>{u.disabled_at ? 'отключён' : 'активен'}</td>
          <td><button onclick={() => toggle(u.id)}>{open === u.id ? 'Скрыть' : 'Токены'}</button></td>
        </tr>
        {#if open === u.id}
          <tr>
            <td colspan="4">
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

<Dialog.Root
  open={shown !== null}
  onOpenChange={(o) => {
    if (!o) shown = null;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog">
      <Dialog.Title>Новый токен</Dialog.Title>
      <Dialog.Description class="warn">
        Токен показан один раз и больше не будет показан. Сохраните его сейчас.
      </Dialog.Description>
      <p><code style="word-break: break-all; user-select: all;">{shown?.plaintext}</code></p>
      <div class="row">
        <button class="primary" onclick={copy}>{copied ? 'Скопировано' : 'Копировать'}</button>
        <Dialog.Close>Закрыть</Dialog.Close>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
