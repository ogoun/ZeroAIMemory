<script lang="ts">
  // Просмотр PDF (M5): pdf.js работает в отдельном документе `/viewer/pdf` (своя CSP с
  // 'wasm-unsafe-eval', web.rs) во фрейме; WebUI скачивает файл сам (сессия, выбор пользователя
  // администратора) и передаёт байты сообщением (viewer-pdf/protocol.ts). Фрейм без
  // allow-same-origin (и CSP `sandbox` документа, web.rs): источник непрозрачный — ни DOM WebUI,
  // ни cookie, ни API; без allow-top-navigation, allow-forms и allow-modals — документ не уводит
  // WebUI и не показывает диалогов; allow-popups — ссылки PDF новой вкладкой (noopener).
  import { onMount } from 'svelte';
  import { ApiError, blobBytes, errorText } from '../../lib/api';
  import { accept, OPAQUE_ORIGIN, type FromViewer, type ToViewer } from '../../viewer-pdf/protocol';

  interface Props {
    user: string;
    sha: string;
    file: string;
    /** Страница из выдачи поиска (`page:N`). */
    page: number | null;
    onexpired: () => void;
  }

  let { user, sha, file, page, onexpired }: Props = $props();

  let frame = $state<HTMLIFrameElement | null>(null);
  let error = $state('');
  let status = $state('Загрузка…');
  let ready = false;
  let bytes: ArrayBuffer | null = null;
  let sent = '';

  /** Только своему фрейму и только после `ready`: адресат непрозрачный, источник не задать. */
  function send(m: ToViewer, transfer: Transferable[] = []) {
    if (ready) frame?.contentWindow?.postMessage(m, '*', transfer);
  }

  /** Файл и фрейм готовы — открыть документ (байты передаются, копия не остаётся). */
  function openIfReady() {
    if (!ready || !bytes) return;
    const data = bytes;
    bytes = null;
    send({ type: 'zam-pdf-open', data, page }, [data]);
  }

  async function load(u: string, s: string, f: string) {
    const key = `${u}/${s}/${f}`;
    if (key === sent) return;
    sent = key;
    error = '';
    status = 'Загрузка…';
    try {
      bytes = await blobBytes(u, s, f);
      if (sent !== key) return;
      openIfReady();
    } catch (e) {
      if (sent !== key) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = errorText(e);
    }
  }

  $effect(() => {
    void load(user, sha, file);
  });

  // переход к другой странице того же документа (новая карточка поиска)
  let shownPage: number | null = null;
  $effect(() => {
    const p = page;
    if (p && ready && shownPage !== null && p !== shownPage) send({ type: 'zam-pdf-page', page: p });
  });

  onMount(() => {
    const onmessage = (e: MessageEvent) => {
      const m = accept<FromViewer>(e, frame?.contentWindow, OPAQUE_ORIGIN);
      if (!m) return;
      if (m.type === 'zam-pdf-ready') {
        ready = true;
        openIfReady();
      } else if (m.type === 'zam-pdf-state') {
        shownPage = m.page;
        status = `Страница ${m.page} из ${m.pages}`;
      } else if (m.type === 'zam-pdf-error') {
        error = `Не удалось открыть PDF: ${m.message}`;
      }
    };
    window.addEventListener('message', onmessage);
    return () => window.removeEventListener('message', onmessage);
  });
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
<p class="sr-only" role="status">{status}</p>
<iframe
  bind:this={frame}
  class="pdf-frame"
  title="Просмотр PDF"
  src="viewer/pdf"
  sandbox="allow-scripts allow-popups allow-popups-to-escape-sandbox"
  referrerpolicy="no-referrer"
></iframe>

<style>
  .pdf-frame {
    display: block;
    width: 100%;
    height: calc(100vh - var(--header-h) - 150px);
    min-height: 420px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    margin: 0;
  }
</style>
