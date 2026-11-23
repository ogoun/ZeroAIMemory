<script lang="ts" generics="C extends Component<any, any>">
  // Ленивая часть журнала (M7.2, спецификация §5.9): форма правки `EntryEdit` и корзина
  // `JournalTrash` грузятся отдельными чанками при первом «Изменить» и «Корзина» — в основной
  // чанк не входят. Одна обёртка на все журналы: «Загрузка…», компонент с `props`, сбой загрузки —
  // `LazyFail` (§5.1 п. 9) с «Отмена» (`oncancel`). `dirty()` — несохранённое загруженного компонента
  // (у формы правки), пока он не пришёл — нет.
  import type { Component, ComponentProps } from 'svelte';
  import LazyFail from './LazyFail.svelte';

  interface Props {
    /** Загрузка чанка: `() => import('./EntryEdit.svelte')`; вызывается один раз. */
    load: () => Promise<{ default: C }>;
    /** Свойства загруженного компонента. */
    props: ComponentProps<C>;
    /** Что не загрузилось: «Форма», «Корзина». */
    what: string;
    oncancel?: () => void;
  }

  let { load, props, what, oncancel }: Props = $props();

  let inner = $state<{ dirty?: () => boolean } | undefined>();

  /** В загруженном компоненте есть несохранённое. */
  export function dirty(): boolean {
    return inner?.dirty?.() ?? false;
  }
</script>

{#await load()}
  <p class="muted small">Загрузка…</p>
{:then { default: Loaded }}
  <Loaded bind:this={inner} {...props} />
{:catch e}
  <!-- чанк не пришёл: сервер обновился (у чанков новые имена) или нет сети -->
  <LazyFail error={e} label="{what} не загрузилась" hint="обновите страницу">
    {#if oncancel}<button type="button" onclick={oncancel}>Отмена</button>{/if}
  </LazyFail>
{/await}
