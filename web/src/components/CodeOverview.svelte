<script lang="ts">
  // «Обзор» карты кода (M5.6, спецификация §11): `GET /code/map?format=json` → Mermaid
  // `flowchart` первых 40 файлов по рангу и до 120 рёбер между ними (`overviewMermaid`), рисует
  // `hydrate/mermaid.ts` (strict + DOMPurify). Свой ленивый чанк: страница «Код» без вкладки
  // «Обзор» его не тянет. Новое решение — новый экземпляр (`{#key}` в CodeView). Ответ карты —
  // и готовые SVG — из кэша `codeOverviewCached` по версии снимка: переключение вкладок сервер не
  // запрашивает и диаграмму заново не рисует. Печать: при тёмной теме ОС рядом рисуется светлая
  // копия диаграммы (`default`, только для печати; экранная `dark` на белом листе нечитаема) — в
  // простое браузера после экранной; до её готовности печатается экранная.
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
  /** Печатная копия нарисована: экранная тёмная в печать не идёт. */
  let printReady = $state(false);
  /**
   * Тёмная тема ОС — на момент монтирования (как `pageTheme()` hydrate/mermaid.ts: смена темы ОС
   * при открытом «Обзоре» не перерисовывает диаграмму; общий долг).
   */
  const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  let svgs = new Map<string, string>();

  $effect(() => {
    let live = true;
    const entry = codeOverviewCached(user, space, version);
    svgs = entry.svgs;
    entry.data
      .then((m) => {
        if (!live) return;
        const src = overviewMermaid(m.files, m.edges);
        printReady = false;
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

  /** В простое браузера (запасной путь — таймер): печатная копия не задерживает экранную. */
  const idle = (f: () => void) =>
    'requestIdleCallback' in window ? requestIdleCallback(f, { timeout: 2000 }) : setTimeout(f, 200);

  // диаграмма — после вставки исходника в DOM; модуль Mermaid — ленивый чанк; готовые SVG — из
  // кэша записи; печатная копия — после экранной, своей темой
  $effect(() => {
    const b = box;
    const pb = printBox;
    if (!b || !overview) return;
    tick()
      .then(() => import('../lib/hydrate/mermaid'))
      .then(async (m) => {
        await m.renderMermaid(b, { cache: svgs });
        if (pb)
          idle(() =>
            m
              .renderMermaid(pb, { theme: 'default', cache: svgs })
              .then(() => (printReady = !!pb.querySelector('.mermaid-diagram'))),
          );
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
  <!-- новый исходник (другой снимок) — новые узлы: прежний <pre> уже заменён диаграммой -->
  {#key overview.src}
    <div class="code-overview" class:screen-only={printReady} bind:this={box}>
      <pre class="mermaid-src"><code>{overview.src}</code></pre>
    </div>
    {#if dark}
      <div class="code-overview print-only" class:ready={printReady} data-testid="code-overview-print" bind:this={printBox}>
        <pre class="mermaid-src"><code>{overview.src}</code></pre>
      </div>
    {/if}
  {/key}
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
     исходник (<details>), неготовая печатная копия и экранная тёмная при готовой — не печатаются */
  @media print {
    .code-overview {
      overflow: visible;
      background: #fff;
      border-color: #d9dde3;
      break-inside: avoid;
    }
    .screen-only,
    .print-only:not(.ready),
    .src {
      display: none !important;
    }
    [data-testid='code-overview-note'] {
      break-after: avoid;
    }
  }
</style>
