<script lang="ts" generics="C extends Component<any, any>">
  // Ленивая часть журнала (M7.2, спецификация §5.9): форма правки `EntryEdit` и корзина
  // `JournalTrash` грузятся отдельными чанками при первом «Изменить» и «Корзина» — в основной
  // чанк не входят. Одна обёртка на все журналы: «Загрузка…», компонент с `props`, сбой загрузки —
  // `LazyFail` (§5.1 п. 9) с «Отмена» (`oncancel`). `instance()` — загруженный компонент (его
  // экспорты: у формы правки — `dirty`, `leave`, `release`), пока он не пришёл — `undefined`.
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

  /** Экспорты загруженного компонента, которые читают обёртки (`EditPanel`). */
  type Exports = { dirty?: () => boolean; leave?: () => boolean; release?: () => void };
  let inner = $state<Exports | undefined>();

  /** Загруженный компонент; пока чанк не пришёл — `undefined`. */
  export function instance(): Exports | undefined {
    return inner;
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
