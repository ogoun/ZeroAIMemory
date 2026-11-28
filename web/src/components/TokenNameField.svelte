<script lang="ts">
  // Поле «Название токена» формы выпуска (M7.6 §9.1, решение координатора 141) — одно для
  // «Подключения агента», «Аккаунта» и «Пользователей»: видимая подпись, строка «это не сам
  // токен», ошибка под полем (`error`, с `aria-invalid`). Пустое название проверяет форма: без
  // `required` — понятная ошибка вместо всплывающей подсказки браузера; фокус — через `input`.
  let {
    value = $bindable(''),
    error = '',
    input = $bindable(),
    oninput,
  }: {
    value?: string;
    error?: string;
    input?: HTMLInputElement;
    oninput?: () => void;
  } = $props();

  const uid = $props.id();
</script>

<div class="token-name">
  <label for="{uid}-name">Название токена</label>
  <input
    id="{uid}-name"
    bind:this={input}
    bind:value
    {oninput}
    placeholder="например: ноутбук, Claude Code"
    maxlength="64"
    autocomplete="off"
    aria-invalid={error ? 'true' : undefined}
    aria-describedby={error ? `${uid}-err ${uid}-hint` : `${uid}-hint`}
  />
  <p class="muted hint" id="{uid}-hint">
    Это не сам токен, а подпись для себя — чтобы отличить его в списке. Сам токен сервер покажет один раз
    сразу после выпуска.
  </p>
  {#if error}<p class="error hint" role="alert" id="{uid}-err">{error}</p>{/if}
</div>

<style>
  .token-name {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .token-name label {
    font-weight: 600;
  }
  .token-name input {
    max-width: 100%;
    width: min(100%, 360px);
    box-sizing: border-box;
  }
  .hint {
    margin: 0;
    font-size: 12px;
  }
</style>
