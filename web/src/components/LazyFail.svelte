<script lang="ts">
  // Сбой загрузки ленивого чанка страницы (M7.2, спецификация §5.1 п. 9): текст ошибки и
  // «Обновить страницу». Повтор `import()` без перезагрузки не делается: браузер кэширует отказ
  // модуля до перезагрузки документа (ТЗ §15.4); адрес-хэш при перезагрузке сохраняется. Не только
  // страницы: формы карточки идеи (`IdeaForm`, §5.2 п. 2) — своя подпись (`label`, `hint`) и свои
  // кнопки после «Обновить страницу» (`children`, например «Отмена»).
  import type { Snippet } from 'svelte';
  import { errorText } from '../lib/api';

  interface Props {
    error: unknown;
    /** Что не загрузилось: «Страница не загрузилась» (по умолчанию), «Форма не загрузилась». */
    label?: string;
    /** Подсказка после текста ошибки. */
    hint?: string;
    children?: Snippet;
  }

  let { error, label = 'Страница не загрузилась', hint = '', children }: Props = $props();
</script>

<div class="lazy-fail" role="alert">
  <p class="error">{label}: {errorText(error)}{hint ? ` — ${hint}` : ''}</p>
  <div class="row">
    <button type="button" class="primary" onclick={() => location.reload()}>Обновить страницу</button>
    {@render children?.()}
  </div>
</div>

<style>
  .lazy-fail {
    display: grid;
    justify-items: start;
    gap: 8px;
    margin: 8px 0;
  }
  .lazy-fail .error {
    margin: 0;
  }
</style>
