<script lang="ts">
  // Унифицированный дифф как текст: каждая строка — отдельный элемент с классом по
  // первому символу (+, -, @@, заголовки). Только текстовые узлы, без разметки из диффа.
  let { diff }: { diff: string } = $props();

  type Kind = 'add' | 'del' | 'hunk' | 'meta' | 'ctx';

  function kindOf(l: string): Kind {
    if (l.startsWith('+++') || l.startsWith('---')) return 'meta';
    if (l.startsWith('+')) return 'add';
    if (l.startsWith('-')) return 'del';
    if (l.startsWith('@@')) return 'hunk';
    if (l.startsWith('\\')) return 'meta';
    return 'ctx';
  }

  let lines = $derived(
    diff
      .replace(/\r?\n$/, '')
      .split(/\r?\n/)
      .map((text) => ({ text, kind: kindOf(text) })),
  );
</script>

{#if diff.trim() === ''}
  <p class="muted">Различий нет.</p>
{:else}
  <pre class="diff" aria-label="Различия">{#each lines as l, i (i)}<span class="ln {l.kind}">{l.text || ' '}</span>{/each}</pre>
{/if}

<style>
  .diff {
    margin: 0;
    max-height: 60vh;
    overflow: auto;
    padding: 8px 0;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--code-bg);
    font: 13px/1.45 ui-monospace, Consolas, monospace;
  }
  .ln {
    display: block;
    padding: 0 10px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .add {
    background: rgb(46 160 67 / 0.18);
  }
  .del {
    background: rgb(248 81 73 / 0.18);
  }
  .hunk {
    color: var(--accent);
  }
  .meta {
    color: var(--muted);
  }
</style>
