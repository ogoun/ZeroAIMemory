<script lang="ts">
  // Форма правки записи журнала на месте карточки (M7.2, спецификация §5.9 п. 1): одна точка
  // загрузки ленивого чанка `EntryEdit` для всех журналов; сбой загрузки — `LazyFail` с «Отмена».
  // Пробрасывает экспорты формы: `dirty()` (панель доски спрашивает перед закрытием), `leave()`
  // (действие журнала, которое закрыло бы форму, — сначала вопрос при несохранённом) и `release()`
  // (журнал уже спросил сам). Пока чанк не пришёл, несохранённого нет.
  //
  // `dialog` (M7.3, §6.14 п. 3, решение 85): правка в окне формы — у лент без карточки (Вопросы,
  // Changelog, Releases): `FormDialog` (`wide`) с заголовком формы и записью в шапке, кнопки
  // «Сохранить» и «Отмена» — у окна, поля — `EntryEdit` с `bare`; несохранённое спрашивает окно.
  import FormDialog from './FormDialog.svelte';
  import Lazy from './Lazy.svelte';
  import type { Entry } from '../lib/api';
  import { EDIT_TITLE } from '../lib/journals';

  interface Props {
    user: string;
    entry: Entry;
    onsaved: (e: Entry) => void;
    oncancel: () => void;
    onreread?: (e: Entry) => void;
    onexpired?: () => void;
    /** Правка в окне формы, а не в карточке. */
    dialog?: boolean;
    /** Куда перевести фокус при закрытии окна (`dialog`); `null` — на кнопку, открывшую окно. */
    refocus?: () => Element | null | undefined;
  }

  let { user, entry, onsaved, oncancel, onreread, onexpired, dialog = false, refocus }: Props = $props();

  let lazy = $state<{
    instance: () => { dirty?: () => boolean; leave?: () => boolean; release?: () => void; submit?: () => Promise<void> } | undefined;
  } | null>(null);
  const form = () => lazy?.instance();
  /** Окно открыто, пока форма не сохранена и не отменена (журнал закрывает её своим `mode`). */
  let open = $state(true);

  export function dirty(): boolean {
    return form()?.dirty?.() ?? false;
  }

  export function leave(): boolean {
    return form()?.leave?.() ?? true;
  }

  export function release(): void {
    form()?.release?.();
  }

  const saved = (e: Entry) => {
    open = false;
    onsaved(e);
  };
  const cancel = () => {
    open = false;
    oncancel();
  };
</script>

{#if dialog}
  <FormDialog
    {open}
    title={EDIT_TITLE[entry.kind] ?? 'Правка записи'}
    subject={entry.title}
    submit="Сохранить"
    wide
    {dirty}
    {refocus}
    onsubmit={async () => {
      await form()?.submit?.();
    }}
    onclose={cancel}
  >
    <Lazy
      bind:this={lazy}
      what="Форма"
      load={() => import('./EntryEdit.svelte')}
      props={{ user, entry, onsaved: saved, oncancel: cancel, onreread, onexpired, bare: true }}
      oncancel={cancel}
    />
  </FormDialog>
{:else}
  <Lazy
    bind:this={lazy}
    what="Форма"
    load={() => import('./EntryEdit.svelte')}
    props={{ user, entry, onsaved, oncancel, onreread, onexpired }}
    {oncancel}
  />
{/if}
