// Копирование текста в буфер обмена.
//
// Clipboard API есть только в secure context (HTTPS, localhost). По HTTP в LAN —
// скрытое поле и execCommand('copy'). Поле вставляется внутрь открытого диалога,
// если фокус сейчас в нём: ловушка фокуса диалога иначе вернула бы фокус назад
// и сняла выделение до копирования.

/** Кладёт `text` в буфер обмена; `false` — не удалось ни одним способом. */
export async function copyText(text: string): Promise<boolean> {
  if (window.isSecureContext && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // запасной путь ниже
    }
  }
  return legacyCopy(text);
}

function legacyCopy(text: string): boolean {
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const host = active?.closest<HTMLElement>('[role="dialog"]') ?? document.body;
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  ta.setAttribute('aria-hidden', 'true');
  // вне экрана, но не display:none — иначе выделение невозможно
  ta.style.cssText = 'position:fixed;top:0;left:-9999px;width:1px;height:1px;opacity:0;';
  host.appendChild(ta);
  let ok = false;
  try {
    ta.focus();
    ta.select();
    ta.setSelectionRange(0, text.length);
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  } finally {
    ta.remove();
    active?.focus({ preventScroll: true });
  }
  return ok;
}
