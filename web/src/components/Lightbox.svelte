<script lang="ts">
  // Просмотр картинки поверх страницы: «вписать» или масштаб 25–800 %
  // (кнопки, колесо с Ctrl, клавиши + − 0), прокрутка при увеличении, Esc закрывает.
  let { src, alt, onclose }: { src: string; alt: string; onclose: () => void } = $props();

  const STEPS = [0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8];
  /** `null` — вписать в окно. */
  let scale = $state<number | null>(null);
  let natural = $state({ w: 0, h: 0 });
  let closeBtn = $state<HTMLButtonElement | null>(null);

  $effect(() => {
    closeBtn?.focus();
    const prev = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.body.style.overflow = prev;
    };
  });

  /** Текущий масштаб «вписанной» картинки — опора для шагов. */
  function fitScale(): number {
    if (!natural.w || !natural.h) return 1;
    return Math.min(1, (window.innerWidth * 0.92) / natural.w, (window.innerHeight * 0.82) / natural.h);
  }

  function step(dir: 1 | -1) {
    const cur = scale ?? fitScale();
    const next = dir > 0 ? STEPS.find((s) => s > cur + 1e-3) : [...STEPS].reverse().find((s) => s < cur - 1e-3);
    scale = next ?? cur;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
    } else if (e.key === '+' || e.key === '=') step(1);
    else if (e.key === '-') step(-1);
    else if (e.key === '0') scale = null;
  }

  function onwheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    step(e.deltaY < 0 ? 1 : -1);
  }

  let label = $derived(scale === null ? 'Вписать' : `${Math.round(scale * 100)} %`);
</script>

<svelte:window {onkeydown} />

<div class="lb" role="dialog" aria-modal="true" aria-label={alt || 'Изображение'}>
  <div class="lb-bar">
    <span class="lb-title">{alt}</span>
    <button type="button" onclick={() => step(-1)} aria-label="Уменьшить">−</button>
    <button type="button" class="lb-scale" onclick={() => (scale = scale === null ? 1 : null)} title="Вписать / 100 %">
      {label}
    </button>
    <button type="button" onclick={() => step(1)} aria-label="Увеличить">+</button>
    <a class="lb-open" href={src} target="_blank" rel="noopener">Оригинал</a>
    <button type="button" bind:this={closeBtn} onclick={onclose} aria-label="Закрыть">✕</button>
  </div>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="lb-stage" class:zoomed={scale !== null} {onwheel} onclick={(e) => e.target === e.currentTarget && onclose()}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <img
      {src}
      {alt}
      onload={(e) => {
        const im = e.currentTarget as HTMLImageElement;
        natural = { w: im.naturalWidth, h: im.naturalHeight };
      }}
      onclick={() => (scale = scale === null ? 1 : null)}
      style:width={scale !== null && natural.w ? `${natural.w * scale}px` : null}
    />
  </div>
</div>

<style>
  .lb {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    grid-template-rows: auto 1fr;
    background: rgb(10 12 16 / 0.92);
    color: #f1f3f6;
  }
  .lb-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
  }
  .lb-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.85;
  }
  .lb-bar button,
  .lb-open {
    min-width: 34px;
    padding: 4px 10px;
    border: 1px solid rgb(255 255 255 / 0.25);
    border-radius: 6px;
    background: rgb(255 255 255 / 0.08);
    color: inherit;
    text-decoration: none;
    font-size: 14px;
  }
  .lb-scale {
    min-width: 72px !important;
  }
  .lb-stage {
    overflow: auto;
    display: grid;
    place-items: center;
    padding: 12px;
  }
  .lb-stage img {
    max-width: 100%;
    max-height: calc(100vh - 80px);
    cursor: zoom-in;
    background: #fff;
  }
  .lb-stage.zoomed {
    place-items: start;
  }
  .lb-stage.zoomed img {
    margin: 0 auto;
    max-width: none;
    max-height: none;
    cursor: zoom-out;
  }
</style>
