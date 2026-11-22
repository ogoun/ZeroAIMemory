<script lang="ts" module>
  // Вкладки статусов с числами (M7.2, спецификация §5.2 п. 1, п. 6): шаблон ARIA tabs — `tablist`,
  // ←/→ по кругу, Home, End, бегающий `tabindex`, выбор следует за фокусом; панель — `children`
  // (`tabpanel`, подписан выбранной вкладкой). Имя вкладки для чтения с экрана — «<статус>, N
  // <существительное>» («Выполнен, 60 задач»), видимо — «Выполнен 60». Вкладки переносятся на
  // вторую строку, а не прокручиваются: переход к любому статусу — одно касание. Общий компонент
  // вкладок ARIA: прежние вкладки «Кода», документа, «Идей» и таблиц на него в волне M7.2 не
  // переводятся.
  /** Вкладка: статус, подпись и число записей статуса (сервер; нет — без числа). */
  export interface StatusTab {
    id: string;
    title: string;
    count?: number;
  }
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import { RECORD_FORMS, countText, type Forms } from '../lib/plural';

  interface Props {
    /** Имя списка вкладок. */
    label: string;
    tabs: StatusTab[];
    selected: string;
    onselect: (id: string) => void;
    /** Формы существительного числа в имени вкладки (по умолчанию — «запись»). */
    noun?: Forms;
    /** Содержимое панели выбранной вкладки. */
    children?: Snippet;
  }

  let { label, tabs, selected, onselect, noun = RECORD_FORMS, children }: Props = $props();

  const uid = $props.id();
  let els = $state<HTMLButtonElement[]>([]);

  /** Вкладка в порядке Tab: выбранная, а если выбранной нет — первая. */
  let focusable = $derived(tabs.some((t) => t.id === selected) ? selected : (tabs[0]?.id ?? ''));

  const nameOf = (t: StatusTab) => (t.count === undefined ? t.title : `${t.title}, ${countText(t.count, noun)}`);

  /** Клавиатура tablist: ←/→ по кругу, Home, End; выбор следует за фокусом. */
  function onkeydown(e: KeyboardEvent, i: number) {
    const n = tabs.length;
    let j = -1;
    if (e.key === 'ArrowRight') j = (i + 1) % n;
    else if (e.key === 'ArrowLeft') j = (i - 1 + n) % n;
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = n - 1;
    if (j < 0) return;
    e.preventDefault();
    onselect(tabs[j].id);
    els[j]?.focus();
  }
</script>

<div class="status-tabs" role="tablist" aria-label={label}>
  {#each tabs as t, i (t.id)}
    <button
      type="button"
      role="tab"
      id={`${uid}-tab-${t.id}`}
      bind:this={els[i]}
      aria-selected={selected === t.id}
      aria-controls={`${uid}-panel`}
      aria-label={nameOf(t)}
      tabindex={focusable === t.id ? 0 : -1}
      data-status={t.id}
      onclick={() => onselect(t.id)}
      onkeydown={(e) => onkeydown(e, i)}
      >{t.title}{#if t.count !== undefined}{' '}<span class="count">{t.count}</span>{/if}</button
    >
  {/each}
</div>
{#if children}
  <!-- панель — в порядке Tab (`tabindex="0"`, как в образце ARIA APG): пустая вкладка («Задач
       нет.») тоже достижима, Tab с вкладки — на панель, затем на её содержимое -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="status-panel" role="tabpanel" id={`${uid}-panel`} aria-labelledby={`${uid}-tab-${selected}`} tabindex="0">
    {@render children()}
  </div>
{/if}

<style>
  .status-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 10px;
  }
  .status-tabs button {
    padding: 4px 12px;
    border-radius: 16px;
    white-space: nowrap;
  }
  .status-tabs button[aria-selected='true'] {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--selected);
  }
  .count {
    margin-left: 2px;
    color: var(--muted);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  [aria-selected='true'] .count {
    color: inherit;
  }
  .status-panel:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 4px;
  }
  /* при касании — цели 44 px (app.css) и зазор между соседними (`--touch-gap`) */
  @media (pointer: coarse) {
    .status-tabs {
      gap: var(--touch-gap);
    }
  }
</style>
