// Сниппеты выдачи поиска: сервер обрамляет совпадения символами U+E000 … U+E001 (`POST /search`).

const MARK_START = '\uE000';
const MARK_END = '\uE001';

/** Сниппет → фрагменты; `m` — совпадение (между U+E000 и U+E001). */
export function snippetParts(s: string): { t: string; m: boolean }[] {
  const out: { t: string; m: boolean }[] = [];
  let m = false;
  let buf = '';
  for (const ch of s) {
    if (ch === MARK_START || ch === MARK_END) {
      if (buf) out.push({ t: buf, m });
      buf = '';
      m = ch === MARK_START;
    } else {
      buf += ch;
    }
  }
  if (buf) out.push({ t: buf, m });
  return out;
}
