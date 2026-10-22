<script lang="ts">
  // Vault (§7.6): секреты пользователя в области пространства — метаданные и маска; «Показать»
  // запрашивает значение (`?reveal=1`, ответ no-store, раскрытие записывается в аудит).
  // Значение живёт в состоянии только пока показано (не дольше REVEAL_MS), в консоль и
  // хранилища браузера не попадает. Без мастер-ключа — баннер vault-locked.
  import { onDestroy, untrack } from 'svelte';
  import {
    ApiError,
    errorText,
    isVaultLocked,
    listVault,
    revealVault,
    type VaultItem,
  } from '../lib/api';
  import { copyText } from '../lib/clipboard';
  import { fmtTime } from '../lib/journals';

  interface Props {
    user: string;
    /** `knowledge` или GUID решения. */
    space: string;
    onexpired: () => void;
  }

  let { user, space, onexpired }: Props = $props();

  /** Сколько показывается значение. */
  const REVEAL_MS = 30_000;
  const MASK = '••••••••••••';
  const TYPE_TEXT: Record<string, string> = {
    password: 'пароль',
    token: 'токен',
    ssh_key: 'SSH-ключ',
    connection_string: 'строка подключения',
    url: 'адрес',
    repo: 'репозиторий',
    note: 'заметка',
  };

  let items = $state<VaultItem[]>([]);
  let loading = $state(false);
  let error = $state('');
  let locked = $state<string | null>(null);
  /** Показанное значение: одно за раз. */
  let shown = $state<{ id: string; value: string } | null>(null);
  let copied = $state('');
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;

  function hide() {
    clearTimeout(timer);
    shown = null;
    copied = '';
  }

  onDestroy(hide);

  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      hide();
      load(u, s);
    });
  });

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    if (isVaultLocked(e)) {
      locked = e instanceof Error ? e.message : 'Vault заблокирован';
      return;
    }
    error = errorText(e);
  }

  async function load(u: string, s: string) {
    const my = ++seq;
    loading = true;
    error = '';
    locked = null;
    try {
      const r = await listVault(u, s);
      if (my === seq) items = r.items;
    } catch (e) {
      if (my !== seq) return;
      items = [];
      fail(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function reveal(it: VaultItem) {
    hide();
    error = '';
    try {
      const value = await revealVault(user, it.id);
      shown = { id: it.id, value };
      timer = setTimeout(hide, REVEAL_MS);
    } catch (e) {
      fail(e);
    }
  }

  async function copy() {
    if (!shown) return;
    copied = (await copyText(shown.value)) ? 'Скопировано' : 'Не удалось скопировать: выделите значение вручную';
  }
</script>

<section class="journal vault">
  {#if locked !== null}
    <div class="warn locked" role="alert">
      <strong>{locked}</strong>
      <p class="small">
        Значения секретов недоступны, пока не восстановлен мастер-ключ: проверьте файл ключа или
        переменную ZAM_MASTER_KEY и перезапустите сервер.
      </p>
    </div>
  {/if}
  <p class="muted small">
    Значения скрыты. «Показать» раскрывает значение на {REVEAL_MS / 1000} с; каждое раскрытие
    записывается в аудит.
  </p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if !loading && !locked && items.length === 0 && !error}
    <p class="muted">Секретов в этой области нет.</p>
  {/if}
  <ul class="items" aria-label="Секреты">
    {#each items as it (it.id)}
      <li class="item" data-vault={it.id}>
        <div class="head">
          <strong>{it.title}</strong>
          <span class="chip">{TYPE_TEXT[it.data.type] ?? it.data.type}</span>
          <span class="muted small">ревизия {it.rev} · {fmtTime(it.updated_at)}</span>
        </div>
        {#if it.data.username || it.data.url || it.data.description}
          <div class="muted small">
            {#if it.data.username}пользователь <code>{it.data.username}</code>{/if}
            {#if it.data.url}· <code>{it.data.url}</code>{/if}
            {#if it.data.description}· {it.data.description}{/if}
          </div>
        {/if}
        <div class="row value">
          {#if shown?.id === it.id}
            <input class="secret" readonly aria-label="Значение {it.title}" value={shown.value}
              onfocus={(e) => e.currentTarget.select()} />
            <button type="button" onclick={copy}>Копировать</button>
            <button type="button" onclick={hide}>Скрыть</button>
            {#if copied}<span class="muted small" role="status">{copied}</span>{/if}
          {:else}
            <code class="mask" aria-label="Значение скрыто">{MASK}</code>
            <button type="button" onclick={() => reveal(it)} disabled={locked !== null}
              title="Раскрытие записывается в аудит">Показать</button>
          {/if}
        </div>
      </li>
    {/each}
  </ul>
</section>

<style>
  .locked {
    margin-bottom: 12px;
  }
  .locked p {
    margin: 4px 0 0;
  }
  .items {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  .item {
    padding: 10px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .value {
    margin-top: 6px;
  }
  .mask {
    letter-spacing: 2px;
    color: var(--muted);
  }
  .secret {
    flex: 1;
    min-width: 200px;
    font-family: ui-monospace, Consolas, monospace;
  }
  .small {
    font-size: 12px;
  }
</style>
