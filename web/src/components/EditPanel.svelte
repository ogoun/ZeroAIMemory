<script lang="ts">
  // Форма правки записи журнала на месте карточки (M7.2, спецификация §5.9 п. 1): одна точка
  // загрузки ленивого чанка `EntryEdit` для всех журналов; сбой загрузки — `LazyFail` с «Отмена».
  // Пробрасывает экспорты формы: `dirty()` (панель доски спрашивает перед закрытием), `leave()`
  // (действие журнала, которое закрыло бы форму, — сначала вопрос при несохранённом) и `release()`
  // (журнал уже спросил сам). Пока чанк не пришёл, несохранённого нет.
  import Lazy from './Lazy.svelte';
  import type { Entry } from '../lib/api';

  interface Props {
    user: string;
    entry: Entry;
    onsaved: (e: Entry) => void;
    oncancel: () => void;
    onreread?: (e: Entry) => void;
    onexpired?: () => void;
  }

  let { user, entry, onsaved, oncancel, onreread, onexpired }: Props = $props();

  let lazy = $state<{ instance: () => { dirty?: () => boolean; leave?: () => boolean; release?: () => void } | undefined } | null>(
    null,
  );
  const form = () => lazy?.instance();

  export function dirty(): boolean {
    return form()?.dirty?.() ?? false;
  }

  export function leave(): boolean {
    return form()?.leave?.() ?? true;
  }

  export function release(): void {
    form()?.release?.();
  }
</script>

<Lazy
  bind:this={lazy}
  what="Форма"
  load={() => import('./EntryEdit.svelte')}
  props={{ user, entry, onsaved, oncancel, onreread, onexpired }}
  {oncancel}
/>
