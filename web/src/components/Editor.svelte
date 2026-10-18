<script lang="ts">
  // Редактор заметки: CodeMirror 6 (ленивый чанк) слева, предпросмотр сервера справа.
  // Текст правится как есть (без WYSIWYG); сохранение — PUT с If-Match текущей ревизии,
  // конфликт — окно с диффом и выбором. Уход с несохранёнными правками — с подтверждением.
  import '../doc.css';
  import { Dialog } from 'bits-ui';
  import { onMount, tick, untrack } from 'svelte';
  import DiffView from './DiffView.svelte';
  import {
    ApiError,
    conflictOf,
    getNode,
    renderPreview,
    replaceNode,
    uploadBlob,
    type NodeInfo,
    type Preview,
  } from '../lib/api';
  import { copyText } from '../lib/clipboard';
  import type { EditorHandle } from '../lib/editor/codemirror';
  import { codeOf, flashButton } from '../lib/hydrate/dom';
  import { hydrateRendered } from '../lib/hydrate/render';
  import { anchorId, href, parse, setGuard } from '../lib/router';

  interface Props {
    user: string;
    space: string;
    node: string;
    /** Сохранена новая ревизия. */
    onsaved: (n: NodeInfo) => void;
    /** Выход из редактора (правки сохранены или отброшены). */
    onclose: () => void;
    onexpired: () => void;
  }

  let { user, space, node, onsaved, onclose, onexpired }: Props = $props();

  /** Предел тела POST /render (сервер: 1 МБ). */
  const RENDER_MAX = 1024 * 1024;
  const PREVIEW_DELAY_MS = 150;
  const LEAVE_QUESTION = 'Есть несохранённые изменения. Уйти без сохранения?';

  let host = $state<HTMLDivElement | null>(null);
  let cm: EditorHandle | null = null;
  let ready = $state(false);
  let rev = $state(0);
  /** Текст последней загрузки или сохранения — эталон для «есть изменения». */
  let saved = '';
  let dirty = $state(false);
  let saving = $state(false);
  let uploading = $state(false);
  let error = $state('');
  let status = $state('');
  let preview = $state<Preview | null>(null);
  let previewNote = $state('');
  let previewEl = $state<HTMLDivElement | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);
  let conflict = $state<{ current_rev: number; diff: string; text: string } | null>(null);
  let conflictError = $state('');
  let conflictBusy = $state(false);

  let previewSeq = 0;
  let previewTimer: ReturnType<typeof setTimeout> | undefined;

  const message = (e: unknown) => (e instanceof Error ? e.message : 'Ошибка');
  const isDirty = () => cm !== null && cm.text() !== saved;

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return true;
    }
    return false;
  }

  onMount(() => {
    let alive = true;
    (async () => {
      try {
        const [mod, j] = await Promise.all([import('../lib/editor/codemirror'), getNode(user, node)]);
        if (!alive || !host) return;
        const text = j.content ?? '';
        saved = text;
        rev = j.rev;
        cm = mod.createEditor(host, text, {
          onChange,
          onSave: () => void save(),
          onFiles: (files, pos) => void attach(files, pos),
        });
        ready = true;
        cm.focus();
        schedulePreview(0);
      } catch (e) {
        if (alive && !expired(e)) error = `Не удалось открыть редактор: ${message(e)}`;
      }
    })();

    // уход по адресу (дерево, ссылка, назад) — подтверждение; внутри заметки — свободно
    const offGuard = setGuard(
      (next) =>
        (next.name === 'node' && next.user === user && next.node === node) ||
        !isDirty() ||
        window.confirm(LEAVE_QUESTION),
    );
    const onbeforeunload = (e: BeforeUnloadEvent) => {
      if (isDirty()) e.preventDefault();
    };
    window.addEventListener('beforeunload', onbeforeunload);
    return () => {
      alive = false;
      offGuard();
      window.removeEventListener('beforeunload', onbeforeunload);
      clearTimeout(previewTimer);
      previewSeq++;
      cm?.destroy();
      cm = null;
    };
  });

  function onChange() {
    dirty = isDirty();
    status = '';
    schedulePreview(PREVIEW_DELAY_MS);
  }

  function schedulePreview(delay: number) {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(runPreview, delay);
  }

  async function runPreview() {
    if (!cm) return;
    const my = ++previewSeq;
    const md = cm.text();
    // UTF-8 — не больше 3 байт на единицу UTF-16: точный подсчёт только у границы
    if (md.length * 3 > RENDER_MAX && new TextEncoder().encode(md).length > RENDER_MAX) {
      previewNote = 'Текст больше 1 МБ: предпросмотр недоступен.';
      return;
    }
    try {
      const r = await renderPreview(user, md, node);
      // ответ на устаревший текст не показывается
      if (my !== previewSeq) return;
      previewNote = '';
      preview = r;
    } catch (e) {
      if (my !== previewSeq || expired(e)) return;
      previewNote = `Предпросмотр не обновлён: ${message(e)}`;
    }
  }

  // Оживление предпросмотра — теми же функциями, что и в просмотре документа.
  $effect(() => {
    const r = preview;
    const el = previewEl;
    if (r && el) untrack(() => hydrate(r, el));
  });

  async function hydrate(r: Preview, el: HTMLElement) {
    await tick();
    if (preview !== r) return;
    const ok = await hydrateRendered(el, {
      wiki: (n, a) => href({ name: 'node', user, space, node: n, anchor: a }),
      self: (a) => href({ name: 'node', user, space, node, anchor: a }),
      alive: () => preview === r,
    });
    if (!ok && preview === r) previewNote = 'Не удалось загрузить модуль формул или диаграмм.';
  }

  /** Сохранение с базовой ревизией `base` (по умолчанию — загруженная). */
  async function save(base?: number): Promise<boolean> {
    if (!cm || saving) return false;
    const text = cm.text();
    saving = true;
    error = '';
    try {
      const n = await replaceNode(user, node, base ?? rev, text);
      rev = n.current_rev;
      saved = text;
      dirty = isDirty();
      status = `Сохранено: ревизия ${rev}`;
      onsaved(n);
      return true;
    } catch (e) {
      const c = conflictOf(e);
      if (c) {
        conflictError = '';
        conflict = { ...c, text };
      } else if (!expired(e)) {
        error =
          e instanceof ApiError && e.status === 413
            ? `Заметка больше предела: ${e.message}`
            : `Не сохранено: ${message(e)}`;
      }
      return false;
    } finally {
      saving = false;
    }
  }

  async function overwrite() {
    const c = conflict;
    if (!c || conflictBusy) return;
    conflictBusy = true;
    conflict = null;
    try {
      await save(c.current_rev);
    } finally {
      conflictBusy = false;
    }
  }

  async function copyAndReload() {
    const c = conflict;
    if (!c || conflictBusy || !cm) return;
    conflictBusy = true;
    conflictError = '';
    try {
      // без копии текст пользователя не заменяется
      if (!(await copyText(cm.text()))) {
        conflictError = 'Не удалось скопировать текст в буфер обмена: текст в редакторе не заменён.';
        return;
      }
      const j = await getNode(user, node);
      const text = j.content ?? '';
      saved = text;
      rev = j.rev;
      cm.reset(text);
      dirty = false;
      conflict = null;
      status = `Ваш текст в буфере обмена; открыта текущая ревизия ${rev}`;
      schedulePreview(0);
    } catch (e) {
      if (!expired(e)) conflictError = message(e);
    } finally {
      conflictBusy = false;
    }
  }

  function cancel() {
    if (isDirty() && !window.confirm(LEAVE_QUESTION)) return;
    onclose();
  }

  /** Markdown-ссылка на вложение: медиа — встраиванием `![…]`, остальное — ссылкой. */
  function attachmentMarkdown(name: string, link: string, mime: string): string {
    const alt = name.replace(/[\\[\]]/g, '\\$&');
    const embed = /^(image|video|audio)\//i.test(mime);
    return `${embed ? '!' : ''}[${alt}](${link})`;
  }

  /** Очередь загрузок: вставки и сбросы во время загрузки ждут её конца. */
  let uploadQueue: Promise<void> = Promise.resolve();

  /** Кнопка, вставка из буфера, перетаскивание: файлы по очереди, по ссылке на строку. */
  function attach(files: File[], pos: number | null): Promise<void> {
    uploadQueue = uploadQueue.then(() => uploadAll(files, pos));
    return uploadQueue;
  }

  async function uploadAll(files: File[], pos: number | null) {
    if (!cm || !files.length) return;
    uploading = true;
    error = '';
    let at = pos;
    let first = true;
    try {
      for (const file of files) {
        status = `Загрузка «${file.name}»…`;
        try {
          const b = await uploadBlob(user, node, file);
          if (!cm) return;
          const md = attachmentMarkdown(file.name, b.link, b.mime);
          at = cm.insertAt(at, first ? md : cm.lineBreak() + md);
          first = false;
          status = `Файл «${file.name}» прикреплён`;
        } catch (err) {
          status = '';
          if (expired(err)) return;
          error =
            err instanceof ApiError && err.status === 413
              ? `Файл «${file.name}» не загружен: файл больше лимита.`
              : `Файл «${file.name}» не загружен: ${message(err)}`;
        }
      }
    } finally {
      uploading = false;
    }
  }

  function onfile(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const files = [...(input.files ?? [])];
    input.value = '';
    void attach(files, null);
  }

  // Предпросмотр: копирование кода; якорь этой же заметки — прокрутка внутри панели.
  async function onpreviewclick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    const btn = t.closest<HTMLButtonElement>('button[data-copy]');
    if (btn) {
      const ok = await copyText(codeOf(btn));
      flashButton(btn, ok ? 'Скопировано' : 'Не удалось');
      return;
    }
    const a = t.closest<HTMLAnchorElement>('a');
    if (!a || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    if (a.classList.contains('broken')) {
      e.preventDefault();
      return;
    }
    const r = parse(a.getAttribute('href') ?? '');
    if (r.name === 'node' && r.user === user && r.node === node) {
      e.preventDefault();
      if (r.anchor) previewEl?.querySelector(`[id="${CSS.escape(anchorId(r.anchor))}"]`)?.scrollIntoView({ block: 'start' });
    }
  }

  // Ctrl+S вне поля редактора (кнопки панели); в поле его обрабатывает CodeMirror.
  function onkeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 's' && !e.defaultPrevented) {
      e.preventDefault();
      void save();
    }
  }
</script>

<svelte:window {onkeydown} />

<section class="editor" aria-label="Редактор заметки">
  <div class="toolbar row">
    <button type="button" class="primary" onclick={() => save()} disabled={!ready || saving} title="Ctrl+S">
      {saving ? 'Сохранение…' : 'Сохранить'}
    </button>
    <button type="button" onclick={cancel}>Отмена</button>
    <button type="button" onclick={() => fileInput?.click()} disabled={!ready || uploading}>
      Прикрепить файл
    </button>
    <input class="file" type="file" multiple bind:this={fileInput} onchange={onfile} tabindex="-1" aria-hidden="true" />
    <span class="state muted" role="status">
      {#if status}{status}{:else if dirty}Есть несохранённые изменения{:else if ready}Ревизия {rev}{/if}
    </span>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <div class="split">
    <div class="pane source" bind:this={host}>
      {#if !ready && !error}<p class="muted loading">Загрузка редактора…</p>{/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pane preview" aria-label="Предпросмотр" onclick={onpreviewclick}>
      {#if previewNote}<p class="warn">{previewNote}</p>{/if}
      {#if preview}
        {#key preview}
          <!-- HTML ответа POST /render: санитизирован ammonia на сервере -->
          <div class="doc" bind:this={previewEl}>{@html preview.html}</div>
        {/key}
      {/if}
    </div>
  </div>
</section>

<Dialog.Root
  open={conflict !== null}
  onOpenChange={(o) => {
    if (!o && !conflictBusy) conflict = null;
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class="panel dialog conflict">
      {#if conflict}
        <Dialog.Title>Заметка изменена в другом месте</Dialog.Title>
        <Dialog.Description class="muted">
          Пока вы правили, сохранена ревизия {conflict.current_rev}. Ниже — её отличия от
          ревизии, с которой вы начали ({rev}).
        </Dialog.Description>
        <div class="conflict-diff"><DiffView diff={conflict.diff} /></div>
        {#if conflictError}<p class="error" role="alert">{conflictError}</p>{/if}
        <div class="row actions">
          <button type="button" class="danger-fill" onclick={overwrite} disabled={conflictBusy}>
            Перезаписать поверх
          </button>
          <button type="button" class="primary" onclick={copyAndReload} disabled={conflictBusy}>
            Скопировать мой текст и открыть текущую
          </button>
          <Dialog.Close type="button" disabled={conflictBusy}>Вернуться к правке</Dialog.Close>
        </div>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .editor {
    display: grid;
    gap: 8px;
  }
  .toolbar {
    position: sticky;
    top: var(--header-h);
    z-index: 5;
    padding: 8px 0;
    background: var(--bg);
  }
  .state {
    margin-left: auto;
    font-size: 13px;
  }
  .file {
    display: none;
  }
  .split {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 16px;
  }
  .pane {
    height: calc(100vh - var(--header-h) - 150px);
    min-height: 320px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .source {
    overflow: hidden;
  }
  .source :global(.cm-editor) {
    height: 100%;
  }
  .loading {
    padding: 12px;
  }
  .preview {
    padding: 12px 16px;
  }
  :global(.dialog.conflict) {
    width: min(860px, 94vw);
  }
  .conflict-diff {
    margin-top: 12px;
  }
  .actions {
    margin-top: 16px;
    justify-content: flex-end;
  }
  @media (max-width: 999px) {
    .split {
      grid-template-columns: minmax(0, 1fr);
    }
    .pane {
      height: 60vh;
    }
  }
</style>
