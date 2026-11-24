<script lang="ts" module>
  /** Запрос подтверждения или ввода строки. */
  export interface Ask {
    title: string;
    /** Пояснение под заголовком. */
    message?: string;
    /** Подпись поля ввода; без неё — только подтверждение. */
    label?: string;
    value?: string;
    placeholder?: string;
    /** Поле пароля: скрытый ввод, значение без обрезки пробелов. */
    secret?: boolean;
    confirm: string;
    danger?: boolean;
    /** Действие; исключение показывается в окне, окно остаётся открытым. */
    run: (value: string) => Promise<void> | void;
    /** После удавшегося `run`: куда перевести фокус при закрытии окна (кнопка, открывшая окно,
     * могла уйти вместе с записью); `null` — как обычно, на неё. */
    refocus?: () => Element | null | undefined;
  }
</script>

<script lang="ts">
  // Подтверждение или ввод строки (дерево заметок, корзина, сброс пароля, решения, кэш, история) —
  // обёртка над окном формы `FormDialog` (M7.3, §6.14 п. 1): интерфейс `Ask` прежний, каркас общий —
  // шапка, порядок кнопок, телефон (окно с полем — во весь экран, подтверждение — нет), вопрос о
  // несохранённом (набранное в поле против начального значения), фокус: поле с выделенным текстом;
  // у подтверждения `danger` — «Отмена».
  import FormDialog from './FormDialog.svelte';

  let { ask = $bindable() }: { ask: Ask | null } = $props();

  let value = $state('');
  /** Окно, для которого взято значение поля (новое окно — новое значение). */
  let shownFor: Ask | null = null;
  /** Куда перевести фокус при закрытии окна после удавшегося действия (`Ask.refocus`). */
  let after: Ask['refocus'] = undefined;

  $effect.pre(() => {
    const a = ask;
    if (a && a !== shownFor) {
      shownFor = a;
      value = a.value ?? '';
      after = undefined;
    } else if (!a) shownFor = null;
  });

  async function run() {
    const a = ask;
    if (!a) return;
    const v = a.secret ? value : value.trim();
    if (a.label && !v) return;
    // исключение — в окне (`FormDialog`), окно открыто
    await a.run(v);
    // действие могло открыть новое окно (`ask` сменился) — его не закрывать
    if (ask === a) {
      after = a.refocus;
      ask = null;
    }
  }
</script>

<FormDialog
  open={ask !== null}
  title={ask?.title ?? ''}
  description={ask?.message}
  submit={ask?.confirm ?? ''}
  danger={ask?.danger}
  confirm={!ask?.label}
  dirty={() => !!ask?.label && value !== (ask.value ?? '')}
  refocus={() => {
    const r = after;
    after = undefined;
    return r?.() ?? null;
  }}
  onsubmit={run}
  onclose={() => (ask = null)}
>
  {#if ask?.label}
    <label>
      {ask.label}
      {#if ask.secret}
        <input bind:value type="password" autocomplete="new-password" required data-autofocus />
      {:else}
        <input bind:value placeholder={ask.placeholder} required data-autofocus data-select />
      {/if}
    </label>
  {/if}
</FormDialog>
