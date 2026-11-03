<script lang="ts">
  // «Обзор» карты кода (M5.6, спецификация §11): `GET /code/map?format=json` → Mermaid
  // `flowchart` первых 40 файлов по рангу и до 120 рёбер между ними (`overviewMermaid`), рисует
  // `hydrate/mermaid.ts` (strict + DOMPurify). Свой ленивый чанк: страница «Код» без вкладки
  // «Обзор» его не тянет. Новое решение — новый экземпляр (`{#key}` в CodeView). Ответ карты —
  // из кэша `codeOverviewCached` по версии снимка: переключение вкладок сервер не запрашивает.
  // Печать: при тёмной теме ОС рядом рисуется светлая копия диаграммы (`default`, только для
  // печати) — экранная в теме `dark` на белом листе нечитаема.
  import { tick } from 'svelte';
  import { ApiError, errorText } from '../lib/api';
  import { OVERVIEW_EDGES, OVERVIEW_FILES, codeOverviewCached, overviewMermaid } from '../lib/code';

  interface Props {
    user: string;
    space: string;
    /** Версия карты (`overviewVersion`): ключ кэша ответа; `null` — без кэша. */
    version: string | null;
    onexpired: () => void;
  }

  let { user, space, version, onexpired }: Props = $props();

  let overview = $state<{ src: string; shown: number; total: number; edges: number } | null>(null);
  let error = $state('');
  let box = $state<HTMLElement | null>(null);
  let printBox = $state<HTMLElement | null>(null);
  /** Тёмная тема ОС на момент показа — нужна светлая печатная копия. */
  const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;

  $effect(() => {
    let live = true;
    codeOverviewCached(user, space, version)
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

  // диаграмма — после вставки исходника в DOM; модуль Mermaid — ленивый чанк; печатная копия —
  // после экранной (очередь `renderMermaid`), своей темой
  $effect(() => {
    const b = box;
    const pb = printBox;
    if (!b || !overview) return;
    tick()
      .then(() => import('../lib/hydrate/mermaid'))
      .then(async (m) => {
        await m.renderMermaid(b);
        if (pb) await m.renderMermaid(pb, { theme: 'default' });
      })
      .catch((e) => (error = errorText(e)));
  });
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if overview}
  <p class="muted" data-testid="code-overview-note">
    Файлы по рангу: показано {overview.shown} из {overview.total}, связей: {overview.edges} (первые
    {OVERVIEW_FILES} файлов и до {OVERVIEW_EDGES} самых весомых связей между ними).
  </p>
  <div class="code-overview" class:screen-only={dark} bind:this={box}>
    <pre class="mermaid-src"><code>{overview.src}</code></pre>
  </div>
  {#if dark}
    <div class="code-overview print-only" data-testid="code-overview-print" bind:this={printBox}>
      <pre class="mermaid-src"><code>{overview.src}</code></pre>
    </div>
  {/if}
  <details class="src">
    <summary>Исходник Mermaid</summary>
    <pre data-testid="code-overview-src"><code>{overview.src}</code></pre>
  </details>
{:else if !error}
  <p class="muted" role="status">Загрузка…</p>
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
  @media screen {
    .print-only {
      display: none;
    }
  }
  /* печать: светлая диаграмма целиком (без прокрутки и обрезки), подпись не отрывается от неё,
     исходник (<details>) и экранная копия тёмной темы — не печатаются */
  @media print {
    .code-overview {
      overflow: visible;
      background: #fff;
      border-color: #d9dde3;
      break-inside: avoid;
    }
    .screen-only,
    .src {
      display: none !important;
    }
    [data-testid='code-overview-note'] {
      break-after: avoid;
    }
  }
</style>
