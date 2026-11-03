<script lang="ts">
  // «Обзор» карты кода (M5.6, спецификация §11): `GET /code/map?format=json` → Mermaid
  // `flowchart` первых 40 файлов по рангу и до 120 рёбер между ними (`overviewMermaid`), рисует
  // `hydrate/mermaid.ts` (strict + DOMPurify). Свой ленивый чанк: страница «Код» без вкладки
  // «Обзор» его не тянет. Новое решение — новый экземпляр (`{#key}` в CodeView).
  import { tick } from 'svelte';
  import { ApiError, errorText } from '../lib/api';
  import { OVERVIEW_EDGES, OVERVIEW_FILES, codeOverview, overviewMermaid } from '../lib/code';

  let { user, space, onexpired }: { user: string; space: string; onexpired: () => void } = $props();

  let overview = $state<{ src: string; shown: number; total: number; edges: number } | null>(null);
  let error = $state('');
  let box = $state<HTMLElement | null>(null);

  $effect(() => {
    let live = true;
    codeOverview(user, space)
      .then((m) => {
        if (!live) return;
        const src = overviewMermaid(m.files, m.edges);
        overview = {
          src,
          shown: Math.min(m.files.length, OVERVIEW_FILES),
          total: m.files.length + m.files_left,
          edges: (src.match(/-->/g) ?? []).length,
        };
      })
      .catch((e) => {
        if (!live) return;
        if (e instanceof ApiError && e.status === 401) onexpired();
        else error = errorText(e);
      });
    return () => {
      live = false;
    };
  });

  // диаграмма — после вставки исходника в DOM; модуль Mermaid — ленивый чанк
  $effect(() => {
    const b = box;
    if (!b || !overview) return;
    tick()
      .then(() => import('../lib/hydrate/mermaid'))
      .then((m) => m.renderMermaid(b))
      .catch((e) => (error = errorText(e)));
  });
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if overview}
  <p class="muted" data-testid="code-overview-note">
    Файлы по рангу: показано {overview.shown} из {overview.total}, связей: {overview.edges} (первые
    {OVERVIEW_FILES} файлов и до {OVERVIEW_EDGES} самых весомых связей между ними).
  </p>
  <div class="code-overview" bind:this={box}>
    <pre class="mermaid-src"><code>{overview.src}</code></pre>
  </div>
  <details class="src">
    <summary>Исходник Mermaid</summary>
    <pre data-testid="code-overview-src"><code>{overview.src}</code></pre>
  </details>
{:else if !error}
  <p class="muted">Загрузка…</p>
{/if}

<style>
  .code-overview {
    overflow: auto;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .code-overview :global(.mermaid-diagram svg) {
    max-width: 100%;
    height: auto;
  }
  .src {
    margin-top: 8px;
  }
  .src pre {
    white-space: pre-wrap;
    font-size: 12px;
    background: var(--code-bg);
    border-radius: 4px;
    padding: 4px 6px;
  }
  @media print {
    .code-overview {
      background: #fff;
      border-color: #d9dde3;
      break-inside: avoid;
    }
  }
</style>
