<script lang="ts">
  // «Аккаунт» (§11.4, §15.1): смена своего пароля и свои токены агентов (выпуск и отзыв
  // `rw`/`ro`). В режиме `forced` — только смена пароля, заданного администратором: до неё
  // прочие разделы закрыты сервером (403 password-change-required).
  import { onMount } from 'svelte';
  import TokenShown from '../components/TokenShown.svelte';
  import {
    ApiError,
    changePassword,
    errorText,
    issueOwnToken,
    listOwnTokens,
    revokeOwnToken,
    type Access,
    type IssuedToken,
    type Me,
    type TokenInfo,
  } from '../lib/api';
  import { fmtTime } from '../lib/journals';

  let {
    me,
    forced = false,
    onchanged,
    onexpired,
  }: { me: Me; forced?: boolean; onchanged?: () => void; onexpired: () => void } = $props();

  const MIN = 10;

  let oldPw = $state('');
  let newPw = $state('');
  let repeatPw = $state('');
  let pwError = $state('');
  let pwDone = $state('');
  let pwBusy = $state(false);

  let tokens = $state<TokenInfo[]>([]);
  let tokError = $state('');
  let tokenName = $state('');
  let tokenAccess = $state<Access>('rw');
  let shown = $state<IssuedToken | null>(null);

  /** Ошибка до отправки: короткий пароль, повтор не совпал, тот же пароль. */
  let pwProblem = $derived(
    newPw && [...newPw].length < MIN
      ? `Новый пароль — не короче ${MIN} символов.`
      : repeatPw && newPw !== repeatPw
        ? 'Повтор не совпадает с новым паролем.'
        : newPw && newPw === oldPw
          ? 'Новый пароль должен отличаться от текущего.'
          : '',
  );

  async function submitPassword(e: SubmitEvent) {
    e.preventDefault();
    if (pwProblem || newPw !== repeatPw) return;
    pwBusy = true;
    pwError = '';
    pwDone = '';
    try {
      await changePassword(oldPw, newPw);
      oldPw = newPw = repeatPw = '';
      pwDone = 'Пароль изменён. Другие сессии этого пользователя закрыты.';
      onchanged?.();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired();
      pwError = errorText(err);
    } finally {
      pwBusy = false;
    }
  }

  async function loadTokens() {
    tokError = '';
    try {
      tokens = await listOwnTokens();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired();
      tokError = errorText(err);
    }
  }

  async function addToken(e: SubmitEvent) {
    e.preventDefault();
    tokError = '';
    try {
      shown = await issueOwnToken(tokenName, tokenAccess);
      tokenName = '';
      await loadTokens();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired();
      tokError = errorText(err);
    }
  }

  async function revoke(t: TokenInfo) {
    tokError = '';
    try {
      await revokeOwnToken(t.id);
      await loadTokens();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return onexpired();
      tokError = errorText(err);
    }
  }

  // свои токены — после обязательной смены пароля (до неё сервер их не отдаёт)
  onMount(() => {
    if (!forced) loadTokens();
  });
</script>

<section class="panel">
  <h2>{forced ? 'Смена пароля' : 'Аккаунт'}</h2>
  {#if forced}
    <p class="warn" role="alert">
      Пароль задан администратором. Задайте свой пароль, чтобы продолжить работу.
    </p>
  {:else}
    <p class="muted small">Пользователь <b>{me.name}</b>.</p>
  {/if}

  {#if me.kind === 'user'}
    <form class="pw" onsubmit={submitPassword} aria-label="Смена пароля">
      <label>
        Текущий пароль
        <input type="password" bind:value={oldPw} autocomplete="current-password" required />
      </label>
      <label>
        Новый пароль
        <input type="password" bind:value={newPw} autocomplete="new-password" required minlength={MIN} />
      </label>
      <label>
        Повтор нового пароля
        <input type="password" bind:value={repeatPw} autocomplete="new-password" required />
      </label>
      {#if pwProblem}<p class="error small" role="alert">{pwProblem}</p>{/if}
      {#if pwError}<p class="error" role="alert">{pwError}</p>{/if}
      {#if pwDone}<p class="muted" role="status">{pwDone}</p>{/if}
      <div class="row"><button class="primary" disabled={pwBusy || !!pwProblem}>Сменить пароль</button></div>
    </form>
  {:else}
    <p class="muted small">
      Пароль администратора задаётся в конфигурации сервера (<code>zam-server hash-password</code>).
    </p>
  {/if}
</section>

{#if !forced}
  <section class="panel" style="margin-top: 16px;">
    <h2>Мои токены</h2>
    <p class="muted small">
      Токены для агентов и CLI: <code>rw</code> — чтение и запись, <code>ro</code> — только чтение. Токен
      показывается один раз; утёкший токен отзовите и выпустите новый.
    </p>
    {#if tokError}<p class="error" role="alert">{tokError}</p>{/if}
    <table aria-label="Мои токены">
      <thead><tr><th>Имя</th><th>Префикс</th><th>Доступ</th><th>Использован</th><th></th></tr></thead>
      <tbody>
        {#each tokens as t (t.id)}
          <tr data-token={t.name}>
            <td>{t.name}</td>
            <td><code>zam_{t.prefix}…</code></td>
            <td>{t.access}</td>
            <td class="muted">{t.last_used_at ? fmtTime(t.last_used_at) : '—'}{#if t.last_ip} · {t.last_ip}{/if}</td>
            <td>
              {#if t.revoked_at}
                <span class="muted">отозван</span>
              {:else}
                <button class="danger" onclick={() => revoke(t)}>Отозвать</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <form class="row" style="margin-top: 8px;" onsubmit={addToken}>
      <input placeholder="Имя токена" bind:value={tokenName} required maxlength="64" aria-label="Имя токена" />
      <select bind:value={tokenAccess} aria-label="Доступ">
        <option value="rw">rw (чтение и запись)</option>
        <option value="ro">ro (только чтение)</option>
      </select>
      <button class="primary">Выпустить токен</button>
    </form>
  </section>
{/if}

<TokenShown bind:shown />

<style>
  .pw {
    display: grid;
    gap: 10px;
    max-width: 360px;
    margin-top: 12px;
  }
  .pw input {
    width: 100%;
  }
  .small {
    font-size: 12px;
  }
</style>
