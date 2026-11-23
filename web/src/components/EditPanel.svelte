<script lang="ts">
  // Форма правки записи журнала на месте карточки (M7.2, спецификация §5.9 п. 1): одна точка
  // загрузки ленивого чанка `EntryEdit` для всех журналов; сбой загрузки — `LazyFail` с «Отмена».
  // `dirty()` — несохранённое формы (панель доски спрашивает перед закрытием).
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

  let lazy = $state<{ dirty: () => boolean } | null>(null);

  /** В форме есть несохранённое; пока чанк не пришёл — нет. */
  export function dirty(): boolean {
    return lazy?.dirty() ?? false;
  }
</script>

<Lazy
  bind:this={lazy}
  what="Форма"
  load={() => import('./EntryEdit.svelte')}
  props={{ user, entry, onsaved, oncancel, onreread, onexpired }}
  {oncancel}
/>
