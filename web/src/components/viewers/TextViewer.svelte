<script lang="ts">
  // Просмотр текста и кода (M5): HTML подсветки сервера (`GET /previews/{sha}?kind=text`,
  // syntect — только `<span class>` и экранированный текст), стили — /render/highlight.css.
  import { ApiError, HIGHLIGHT_CSS, errorText, previewText } from '../../lib/api';

  let { user, sha, onexpired }: { user: string; sha: string; onexpired: () => void } = $props();

  let html = $state<string | null>(null);
  let note = $state('');
  let error = $state('');
  let seq = 0;

  async function load(u: string, s: string) {
    const my = ++seq;
    error = '';
    note = '';
    try {
      const r = await previewText(u, s);
      if (my !== seq) return;
      if (r.format !== 'html') throw new Error('сервер не вернул текст');
      html = r.html;
      const parts = [];
      if (r.truncated) parts.push('показано начало файла (1 МиБ)');
      if (r.highlight_skipped) parts.push('без подсветки: файл слишком велик');
      note = parts.join('; ');
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      html = null;
      error = `Не удалось показать текст: ${errorText(e)}`;
    }
  }

  $effect(() => {
    void load(user, sha);
  });
</script>

<svelte:head>
  <link rel="stylesheet" href={HIGHLIGHT_CSS} />
</svelte:head>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if note}<p class="warn" role="status">{note}</p>{/if}
{#if html === null && !error}<p class="muted">Загрузка…</p>{/if}
{#if html !== null}
  <!-- HTML сервера: syntect, только <span class> и экранированный текст (задача 8 M5) -->
  <!-- прокручиваемая область — с клавиатуры (WCAG 2.1.1) -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <pre class="hl-code text-view" tabindex="0">{@html html}</pre>
{/if}

<style>
  .text-view {
    margin: 0;
    max-height: calc(100vh - var(--header-h) - 170px);
    overflow: auto;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--code-bg);
    font: 13px/1.5 ui-monospace, Consolas, monospace;
    white-space: pre;
  }
</style>
