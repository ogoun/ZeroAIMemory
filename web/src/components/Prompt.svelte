<script lang="ts" module>
  /** Запрос подтверждения или ввода строки. */
  export interface Ask {
    title: string;
    /** Пояснение под заголовком. */
    message?: string;
    /** Подпись поля ввода; без неё — только подтверждение. */
    label?: string;
    value?: string;
    placeholder?: string;
    /** Поле пароля: скрытый ввод, значение без обрезки пробелов. */
    secret?: boolean;
    confirm: string;
    danger?: boolean;
    /** Действие; исключение показывается в окне, окно остаётся открытым. */
    run: (value: string) => Promise<void> | void;
    /** После удавшегося `run`: куда перевести фокус при закрытии окна (кнопка, открывшая окно,
     * могла уйти вместе с записью); `null` — как обычно, на неё. */
    refocus?: () => Element | null | undefined;
  }
</script>

<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { focusNow } from '../lib/focus';

  let { ask = $bindable() }: { ask: Ask | null } = $props();

  let value = $state('');
  let error = $state('');
  let busy = $state(false);
  let field = $state<HTMLInputElement | null>(null);
  /** Куда перевести фокус при закрытии окна после удавшегося действия (`Ask.refocus`). */
  let refocus: Ask['refocus'] = undefined;

  // Новое окно — новое значение поля.
  $effect(() => {
    if (ask) {
      value = ask.value ?? '';
      error = '';
      busy = false;
    }
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!ask || busy) return;
    const v = ask.secret ? value : value.trim();
    if (ask.label && !v) return;
    busy = true;
    error = '';
    try {
      await ask.run(v);
      refocus = ask?.refocus;
      ask = null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Ошибка';
    } finally {
      busy = false;
    }
  }
</script>

<Dialog.Root
  open={ask !== null}
  onOpenChange={(o) => {
    if (!o) ask = null;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content
      class="panel dialog"
      onOpenAutoFocus={(e) => {
        refocus = undefined;
        if (field) {
          e.preventDefault();
          field.focus();
          field.select();
        }
      }}
      onCloseAutoFocus={(e) => {
        const r = refocus;
        refocus = undefined;
        if (r && focusNow(r())) e.preventDefault();
      }}
    >
      {#if ask}
        <form onsubmit={submit}>
          <Dialog.Title>{ask.title}</Dialog.Title>
          {#if ask.message}<Dialog.Description class="muted">{ask.message}</Dialog.Description>{/if}
          {#if ask.label}
            <label class="field">
              {ask.label}
              {#if ask.secret}
                <input bind:this={field} bind:value type="password" autocomplete="new-password" required />
              {:else}
                <input bind:this={field} bind:value placeholder={ask.placeholder} required />
              {/if}
            </label>
          {/if}
          {#if error}<p class="error" role="alert">{error}</p>{/if}
          <div class="row actions">
            <button type="submit" class={ask.danger ? 'danger-fill' : 'primary'} disabled={busy}>
              {ask.confirm}
            </button>
            <Dialog.Close type="button">Отмена</Dialog.Close>
          </div>
        </form>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .field {
    display: grid;
    gap: 4px;
    margin-top: 12px;
  }
  .field input {
    width: 100%;
  }
  .actions {
    margin-top: 16px;
    justify-content: flex-end;
  }
</style>
