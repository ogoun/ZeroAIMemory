<script lang="ts">
  // Просмотр PPTX v1 (M5): текст слайдов и заметки докладчика из превью сервера
  // (`GET /previews/{sha}?kind=text`), список слайдов, переход стрелками и кнопками, слайд из
  // выдачи поиска (`slide:N`). Визуальный просмотр слайдов — v2 (ТЗ §19).
  import { tick } from 'svelte';
  import { ApiError, errorText, previewText, type PreviewSlide } from '../../lib/api';

  interface Props {
    user: string;
    sha: string;
    /** Слайд из выдачи поиска (`slide:N`). */
    slide: number | null;
    onexpired: () => void;
  }

  let { user, sha, slide, onexpired }: Props = $props();

  let slides = $state<PreviewSlide[]>([]);
  let warnings = $state<string[]>([]);
  let at = $state(0);
  let error = $state('');
  let loading = $state(true);
  let region = $state<HTMLElement | null>(null);
  let seq = 0;

  async function load(u: string, s: string, want: number | null) {
    const my = ++seq;
    loading = true;
    error = '';
    try {
      const r = await previewText(u, s);
      if (my !== seq) return;
      if (r.format !== 'slides') throw new Error('сервер не вернул слайды');
      slides = r.slides;
      warnings = r.warnings;
      const i = want ? r.slides.findIndex((x) => x.slide === want) : 0;
      at = i >= 0 ? i : 0;
      if (want && i < 0) error = `Слайда ${want} нет: в презентации ${r.slides.length} слайдов.`;
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      error = `Не удалось показать слайды: ${errorText(e)}`;
    } finally {
      if (my === seq) loading = false;
    }
  }

  $effect(() => {
    void load(user, sha, slide);
  });

  let cur = $derived(slides[at] ?? null);

  async function show(i: number, focus = false) {
    if (i < 0 || i >= slides.length) return;
    at = i;
    if (focus) {
      await tick();
      region?.focus();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    const k = e.key;
    if (k === 'ArrowRight' || k === 'ArrowDown' || k === 'PageDown') show(at + 1);
    else if (k === 'ArrowLeft' || k === 'ArrowUp' || k === 'PageUp') show(at - 1);
    else if (k === 'Home') show(0);
    else if (k === 'End') show(slides.length - 1);
    else return;
    e.preventDefault();
  }
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#each warnings as w, i (i)}<p class="warn">Текст показан не полностью: {w}</p>{/each}
{#if loading && !slides.length}
  <p class="muted">Загрузка слайдов…</p>
{:else if slides.length}
  <div class="slides">
    <nav class="list" aria-label="Слайды">
      <ol>
        {#each slides as s, i (s.slide)}
          <li>
            <button type="button" aria-current={i === at ? 'true' : undefined} onclick={() => show(i, true)}>
              <span class="no">{s.slide}</span>
              <span class="t">{s.title || 'Без заголовка'}</span>
            </button>
          </li>
        {/each}
      </ol>
    </nav>
    <div class="main">
      <div class="bar">
        <button type="button" aria-label="Предыдущий слайд" disabled={at === 0} onclick={() => show(at - 1)}>‹</button>
        <span data-testid="slide-pos" aria-live="polite">Слайд {cur?.slide ?? at + 1} из {slides.length}</span>
        <button type="button" aria-label="Следующий слайд" disabled={at >= slides.length - 1} onclick={() => show(at + 1)}>›</button>
      </div>
      {#if cur}
        <!-- область слайда фокусируется для переключения клавишами (стрелки, PageUp/PageDown) -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
        <section
          class="slide"
          bind:this={region}
          tabindex="0"
          aria-label={`Слайд ${cur.slide}`}
          aria-roledescription="слайд"
          {onkeydown}
        >
          <h2>{cur.title || 'Без заголовка'}</h2>
          {#if cur.text}<p class="text">{cur.text}</p>{:else}<p class="muted">На слайде нет текста.</p>{/if}
          {#if cur.notes}
            <details class="notes">
              <summary>Заметки докладчика</summary>
              <p class="text">{cur.notes}</p>
            </details>
          {/if}
        </section>
      {/if}
    </div>
  </div>
{/if}

<style>
  .slides {
    display: grid;
    grid-template-columns: minmax(160px, 240px) minmax(0, 1fr);
    gap: 16px;
  }
  @media (max-width: 719px) {
    .slides {
      grid-template-columns: minmax(0, 1fr);
    }
    .list {
      max-height: 180px;
    }
  }
  .list {
    max-height: calc(100vh - var(--header-h) - 180px);
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .list ol {
    list-style: none;
    margin: 0;
    padding: 4px;
  }
  .list button {
    display: flex;
    gap: 8px;
    width: 100%;
    text-align: left;
    border: 0;
    background: transparent;
    padding: 6px 8px;
  }
  .list button[aria-current='true'] {
    background: var(--selected);
  }
  .list button:hover {
    background: var(--hover);
  }
  .no {
    min-width: 20px;
    color: var(--muted);
    text-align: right;
  }
  .t {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
  }
  .slide {
    min-height: 320px;
    padding: 24px 28px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .slide:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .notes {
    margin-top: 16px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
    color: var(--muted);
  }
</style>
