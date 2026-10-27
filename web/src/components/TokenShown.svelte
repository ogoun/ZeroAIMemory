<script lang="ts">
  // Окно с только что выпущенным токеном: показывается один раз, с копированием.
  // copyText: Clipboard API в secure context, по HTTP в LAN — execCommand('copy');
  // не вышло — токен выделяется в поле с подсказкой скопировать вручную.
  import { Dialog } from 'bits-ui';
  import type { IssuedToken } from '../lib/api';
  import { copyText } from '../lib/clipboard';

  let { shown = $bindable() }: { shown: IssuedToken | null } = $props();

  let copied = $state(false);
  let copyHint = $state('');
  let tokenField = $state<HTMLInputElement | null>(null);

  // новый токен — подсказки сначала
  $effect(() => {
    if (shown) {
      copied = false;
      copyHint = '';
    }
  });

  async function copy() {
    if (!shown) return;
    copyHint = '';
    if (await copyText(shown.plaintext)) {
      copied = true;
      return;
    }
    tokenField?.focus();
    tokenField?.select();
    copyHint = 'Не удалось скопировать автоматически: токен выделен, скопируйте его вручную (Ctrl+C).';
  }
</script>

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
