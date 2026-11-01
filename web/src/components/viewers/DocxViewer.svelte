<script lang="ts">
  // Просмотр DOCX (M5): docx-preview → DOMPurify → srcdoc sandbox-iframe без allow-scripts и
  // allow-same-origin (lib/docx.ts). Переход к абзацу из выдачи (`para:n`): абзац отмечается и
  // прокручивается в видимую часть фрейма средствами CSS (скриптов во фрейме нет).
  import { ApiError, blobBytes, errorText } from '../../lib/api';
  import { renderDocx } from '../../lib/docx';
  import { errorCode } from '../../lib/attachments';

  /** Причины неудачи сервера, при которых файл не разбирается и в браузере. */
  const HEAVY = new Set(['too_large', 'parse_oom']);

  interface Props {
    user: string;
    sha: string;
    file: string;
    /** Абзац из выдачи поиска (`para:N`). */
    para: number | null;
    /** `extract_error` вложения: `too_large`/`parse_oom` — в браузере не разбирать. */
    serverError?: string | null;
    onexpired: () => void;
  }

  let { user, sha, file, para, serverError = null, onexpired }: Props = $props();

  let srcdoc = $state<string | null>(null);
  let error = $state('');
  let note = $state('');
  let busy = $state(true);
  let seq = 0;

  async function load(u: string, s: string, f: string, p: number | null) {
    const my = ++seq;
    busy = true;
    error = '';
    note = '';
    if (serverError && HEAVY.has(errorCode(serverError))) {
      srcdoc = null;
      busy = false;
      error =
        'Документ не показан: сервер не смог его разобрать (слишком велик или не хватило памяти). Скачайте файл.';
      return;
    }
    try {
      const data = await blobBytes(u, s, f);
      const bg = getComputedStyle(document.documentElement).getPropertyValue('--bg');
      const r = await renderDocx(data, p, bg);
      if (my !== seq) return;
      srcdoc = r.srcdoc;
      if (p !== null && !r.target) note = `Абзац ${p} не найден: в документе ${r.paras} абзацев.`;
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      srcdoc = null;
      error = `Не удалось показать документ: ${errorText(e)}`;
    } finally {
      if (my === seq) busy = false;
    }
  }

  $effect(() => {
    void load(user, sha, file, para);
  });
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if note}<p class="warn" role="status">{note}</p>{/if}
{#if busy && !srcdoc}<p class="muted">Загрузка документа…</p>{/if}
{#if srcdoc !== null}
  {#key srcdoc}
    <!-- sandbox="" — без скриптов, форм, всплывающих окон и своего источника (Review Focus 5) -->
    <iframe class="docx-frame" title="Просмотр DOCX" sandbox="" referrerpolicy="no-referrer" {srcdoc}></iframe>
  {/key}
{/if}

<style>
  .docx-frame {
    display: block;
    width: 100%;
    height: calc(100vh - var(--header-h) - 150px);
    min-height: 420px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
  }
</style>
