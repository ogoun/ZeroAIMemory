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
  let copyHint = $state('');
  let tokenField = $state<HTMLInputElement | null>(null);

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
    copyHint = '';
    shown = t;
  }

  // Clipboard API есть только в secure context (HTTPS, localhost); по HTTP в LAN —
  // выделение поля и execCommand('copy'), иначе подсказка скопировать вручную.
  async function copy() {
    if (!shown) return;
    copyHint = '';
    if (window.isSecureContext && navigator.clipboard) {
      try {
        await navigator.clipboard.writeText(shown.plaintext);
        copied = true;
        return;
      } catch {
        // запасной путь ниже
      }
    }
    tokenField?.focus();
    tokenField?.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    if (ok) copied = true;
    else copyHint = 'Не удалось скопировать автоматически: токен выделен, скопируйте его вручную (Ctrl+C).';
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
      <input
        class="token-field"
        readonly
        aria-label="Токен"
        value={shown?.plaintext ?? ''}
        bind:this={tokenField}
        onfocus={(e) => e.currentTarget.select()}
      />
      <div class="row" style="margin-top: 8px;">
        <button class="primary" onclick={copy}>{copied ? 'Скопировано' : 'Копировать'}</button>
        <Dialog.Close>Закрыть</Dialog.Close>
      </div>
      {#if copied}<p class="muted" role="status">Токен скопирован в буфер обмена.</p>{/if}
      {#if copyHint}<p class="error" role="alert">{copyHint}</p>{/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
