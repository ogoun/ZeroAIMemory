<script lang="ts">
  // Сбой загрузки ленивого чанка страницы (M7.2, спецификация §5.1 п. 9): текст ошибки и
  // «Обновить страницу». Повтор `import()` без перезагрузки не делается: браузер кэширует отказ
  // модуля до перезагрузки документа (ТЗ §15.4); адрес-хэш при перезагрузке сохраняется.
  import { errorText } from '../lib/api';

  let { error }: { error: unknown } = $props();
</script>

<div class="lazy-fail" role="alert">
  <p class="error">Страница не загрузилась: {errorText(error)}</p>
  <button type="button" class="primary" onclick={() => location.reload()}>Обновить страницу</button>
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
