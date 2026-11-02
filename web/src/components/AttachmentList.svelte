<script lang="ts">
  // Вложения заметки (M5): имя (ссылка на просмотр), размер, состояние извлечения текста
  // (причина неудачи — по коду, «нужно распознавание»), скачивание и открепление с
  // подтверждением — только при праве записи.
  import Prompt, { type Ask } from './Prompt.svelte';
  import { ApiError, blobUrl, detachAttachment, errorText, type Attachment } from '../lib/api';
  import { errorCode, sizeText, stateText, viewerKind } from '../lib/attachments';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    node: string;
    items: Attachment[];
    /** Право записи в заметку: открепление. */
    writable: boolean;
    ondetached: (a: Attachment) => void;
    onexpired: () => void;
  }

  let { user, space, node, items, writable, ondetached, onexpired }: Props = $props();

  let ask = $state<Ask | null>(null);

  const KIND_TEXT: Record<string, string> = {
    pdf: 'PDF',
    docx: 'DOCX',
    sheet: 'таблица',
    slides: 'презентация',
    image: 'картинка',
    text: 'текст',
    none: 'файл',
  };

  function detach(a: Attachment) {
    ask = {
      title: 'Открепить вложение?',
      message: `«${a.filename}» больше не будет вложением этой заметки, его текст пропадёт из поиска. Ссылки на файл в тексте заметки останутся.`,
      confirm: 'Открепить',
      danger: true,
      run: async () => {
        try {
          await detachAttachment(user, node, a.sha256, a.filename);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) {
            onexpired();
            return;
          }
          throw new Error(errorText(e));
        }
        ondetached(a);
      },
    };
  }
</script>

{#if items.length}
  <ul class="atts" aria-label="Вложения заметки">
    {#each items as a (`${a.sha256}/${a.filename}`)}
      {@const kind = viewerKind(a.filename, a.mime)}
      <li>
        <span class="kind">{KIND_TEXT[kind]}</span>
        <span class="main">
          <a href={href({ name: 'attachment', user, space, node, sha: a.sha256, file: a.filename, locator: null })}
            >{a.filename}</a
          >
          <span class="muted info">
            {sizeText(a.size)} ·
            <span
              class="state"
              class:failed={a.extract_state === 'failed'}
              title={a.extract_error ? `Код причины: ${errorCode(a.extract_error)}` : undefined}>{stateText(a)}</span
            >
          </span>
        </span>
        <a class="dl" href={blobUrl(user, a.sha256, a.filename)} download={a.filename} aria-label={`Скачать ${a.filename}`}>Скачать</a>
        {#if writable}
          <button type="button" class="danger" aria-label={`Открепить ${a.filename}`} onclick={() => detach(a)}>Открепить</button>
        {/if}
      </li>
    {/each}
  </ul>
{:else}
  <p class="muted">У заметки нет вложений. Файл прикрепляется в редакторе (вставка или перетаскивание).</p>
{/if}

<Prompt bind:ask />

<style>
  .atts {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .atts li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 4px;
    border-bottom: 1px solid var(--border);
  }
  .kind {
    flex: none;
    min-width: 92px;
    font: 11px/18px ui-monospace, Consolas, monospace;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--chip);
    color: var(--muted);
    text-align: center;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .main a {
    overflow-wrap: anywhere;
  }
  .info {
    font-size: 12px;
  }
  .state.failed {
    color: var(--danger);
  }
  .dl {
    font-size: 13px;
  }
  .atts button {
    padding: 3px 10px;
    font-size: 13px;
  }
</style>
