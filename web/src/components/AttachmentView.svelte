<script lang="ts">
  // Просмотр вложения заметки (M5): маршрут `…/n/<node>/a/<sha>/<имя>[/<locator>]` — из списка
  // вложений и из выдачи поиска (переход к месту по `locator`). Просмотрщик — по виду файла,
  // каждый — ленивым чанком: PDF (pdf.js в /viewer/pdf), DOCX (sandbox-iframe), листы, слайды,
  // текст; картинки (и SVG) — только через <img>; прочее — скачивание.
  import { ApiError, blobUrl, errorText, getNode, listAttachments, type Attachment } from '../lib/api';
  import { locatorLabel, parseLocator, sizeText, stateText, viewerKind } from '../lib/attachments';
  import { href } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    node: string;
    sha: string;
    file: string;
    locator: string | null;
    onexpired: () => void;
  }

  let { user, space, node, sha, file, locator, onexpired }: Props = $props();

  let meta = $state<Attachment | null>(null);
  let notePath = $state('');
  let error = $state('');
  let seq = 0;

  async function load(u: string, n: string, s: string, f: string) {
    const my = ++seq;
    error = '';
    try {
      const [list, info] = await Promise.all([listAttachments(u, n), getNode(u, n)]);
      if (my !== seq) return;
      notePath = info.node.path;
      meta = list.find((a) => a.sha256 === s && a.filename === f) ?? null;
      if (!meta) error = 'Вложение не найдено: возможно, оно откреплено от заметки.';
    } catch (e) {
      if (my !== seq) return;
      if (e instanceof ApiError && e.status === 401) return onexpired();
      meta = null;
      error = errorText(e);
    }
  }

  $effect(() => {
    void load(user, node, sha, file);
  });

  let kind = $derived(viewerKind(file, meta?.mime ?? ''));
  let loc = $derived(parseLocator(locator));
  let download = $derived(blobUrl(user, sha, file));
  let noteHref = $derived(href({ name: 'node', user, space, node, anchor: null }));
</script>

{#snippet wait()}
  <p class="muted">Загрузка просмотрщика…</p>
{/snippet}

{#snippet failed(e: unknown)}
  <p class="error" role="alert">Просмотрщик не загрузился: {errorText(e)}</p>
{/snippet}

<article class="attview">
  <header class="att-head">
    <a class="back" href={noteHref}>← {notePath || 'Заметка'}</a>
    <h1>{file}</h1>
    <div class="meta muted">
      {#if meta}
        {sizeText(meta.size)} · {stateText(meta)}
      {/if}
      {#if loc}<span class="loc"> · место из выдачи: {locatorLabel(loc)}</span>{/if}
      · <a href={download} download={file}>Скачать</a>
    </div>
  </header>

  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if kind === 'image'}
    <!-- картинки и SVG — только <img>: скрипты SVG не исполняются (Review Focus 5) -->
    <div class="image"><img src={download} alt={file} /></div>
  {:else if kind === 'none'}
    <p class="muted">Просмотр этого вида файлов не поддерживается: скачайте файл.</p>
  {:else}
    {#key `${sha}/${file}`}
      {#if kind === 'pdf'}
        {#await import('./viewers/PdfViewer.svelte')}
          {@render wait()}
        {:then { default: V }}
          <V {user} {sha} {file} page={loc?.kind === 'page' ? loc.n : null} {onexpired} />
        {:catch e}
          {@render failed(e)}
        {/await}
      {:else if kind === 'docx'}
        {#await import('./viewers/DocxViewer.svelte')}
          {@render wait()}
        {:then { default: V }}
          <V {user} {sha} {file} para={loc?.kind === 'para' ? loc.n : null} {onexpired} />
        {:catch e}
          {@render failed(e)}
        {/await}
      {:else if kind === 'sheet'}
        {#await import('./viewers/SheetViewer.svelte')}
          {@render wait()}
        {:then { default: V }}
          <V {user} {sha} locator={loc?.kind === 'sheet' ? loc : null} {onexpired} />
        {:catch e}
          {@render failed(e)}
        {/await}
      {:else if kind === 'slides'}
        {#await import('./viewers/SlidesViewer.svelte')}
          {@render wait()}
        {:then { default: V }}
          <V {user} {sha} slide={loc?.kind === 'slide' ? loc.n : null} {onexpired} />
        {:catch e}
          {@render failed(e)}
        {/await}
      {:else}
        {#await import('./viewers/TextViewer.svelte')}
          {@render wait()}
        {:then { default: V }}
          <V {user} {sha} {onexpired} />
        {:catch e}
          {@render failed(e)}
        {/await}
      {/if}
    {/key}
  {/if}
</article>

<style>
  .attview {
    max-width: 1200px;
    margin: 0 auto;
  }
  .att-head {
    margin-bottom: 12px;
  }
  .back {
    font-size: 13px;
    text-decoration: none;
  }
  .att-head h1 {
    margin: 4px 0 2px;
    overflow-wrap: anywhere;
  }
  .meta {
    font-size: 13px;
  }
  .image {
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
    text-align: center;
  }
  .image img {
    max-width: 100%;
    max-height: calc(100vh - var(--header-h) - 180px);
  }
</style>
