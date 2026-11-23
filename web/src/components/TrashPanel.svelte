<script lang="ts">
  // «Корзина» журнала на месте ленты (M7.2, спецификация §5.9 п. 3): одна точка загрузки ленивого
  // чанка `JournalTrash` для всех журналов (сбой загрузки — `LazyFail`). Журнал при возврате из
  // корзины перечитывает живую ленту сам, поэтому `onrestored` не передаётся.
  import Lazy from './Lazy.svelte';
  import type { EntryKind } from '../lib/api';

  interface Props {
    user: string;
    space: string;
    kind: EntryKind;
    onexpired: () => void;
  }

  let { user, space, kind, onexpired }: Props = $props();
</script>

<Lazy what="Корзина" load={() => import('./JournalTrash.svelte')} props={{ user, space, kind, onexpired }} />
