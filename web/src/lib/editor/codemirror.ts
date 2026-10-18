// Ленивый модуль редактора: CodeMirror 6 с минимальным набором (история, стандартные
// клавиши, перенос строк, подсветка Markdown). Без WYSIWYG: правится точный текст.
//
// Переводы строк. CodeMirror по умолчанию считает разделителями `\r\n`, `\r` и `\n`
// и отдаёт текст с `\n`. Чтобы сохранение не меняло байты, которых пользователь не
// трогал, разделитель определяется по документу при загрузке и задаётся явно
// (`EditorState.lineSeparator`); текст читается через `sliceDoc`, вставка из буфера
// приводится к тому же разделителю.

import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdownKeymap, markdownLanguage } from '@codemirror/lang-markdown';
import { defaultHighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { EditorState } from '@codemirror/state';
import {
  drawSelection,
  EditorView,
  highlightSpecialChars,
  keymap,
  type ViewUpdate,
} from '@codemirror/view';

/** Разделитель строк документа: первый встреченный вид перевода строки, иначе `\n`. */
export function detectLineSeparator(text: string): '\r\n' | '\n' | '\r' {
  const m = /\r\n|\n|\r/.exec(text);
  return (m?.[0] as '\r\n' | '\n' | '\r' | undefined) ?? '\n';
}

export interface EditorHandle {
  /** Текущий текст с исходными переводами строк. */
  text(): string;
  /** Заменяет весь текст (новая загрузка); история правок сбрасывается. */
  reset(text: string): void;
  /**
   * Вставляет строку в позицию `pos` (`null` — вместо выделения); курсор — после неё.
   * Возвращает позицию конца вставки (для следующей вставки подряд).
   */
  insertAt(pos: number | null, s: string): number;
  /** Разделитель строк документа. */
  lineBreak(): string;
  focus(): void;
  destroy(): void;
}

export interface EditorOpts {
  /** Документ изменён пользователем. */
  onChange: () => void;
  /** Ctrl+S / Cmd+S. */
  onSave: () => void;
  /** Файлы из вставки или перетаскивания; `pos` — место сброса (`null` — курсор). */
  onFiles: (files: File[], pos: number | null) => void;
}

/** Цвета — из переменных темы приложения (светлая и тёмная). */
const theme = EditorView.theme({
  '&': { color: 'var(--text)', backgroundColor: 'var(--panel)', height: '100%' },
  '&.cm-focused': { outline: 'none' },
  '.cm-scroller': { fontFamily: 'ui-monospace, Consolas, monospace', fontSize: '14px', lineHeight: '1.5' },
  '.cm-content': { caretColor: 'var(--text)', padding: '10px 0' },
  '.cm-line': { padding: '0 12px' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--text)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground':
    { backgroundColor: 'var(--selected)' },
});

function makeState(doc: string, o: EditorOpts): EditorState {
  const sep = detectLineSeparator(doc);
  const normalize = (t: string) => t.replace(/\r\n|\r|\n/g, sep);
  return EditorState.create({
    doc,
    extensions: [
      EditorState.lineSeparator.of(sep),
      EditorView.clipboardInputFilter.of((t) => normalize(t)),
      history(),
      drawSelection(),
      highlightSpecialChars(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      // только язык (GFM) и клавиши списков: markdown() тянет lang-html с CSS и JS
      markdownLanguage,
      EditorView.lineWrapping,
      theme,
      EditorView.contentAttributes.of({ 'aria-label': 'Текст заметки (Markdown)' }),
      keymap.of([
        {
          key: 'Mod-s',
          preventDefault: true,
          run: () => {
            o.onSave();
            return true;
          },
        },
        ...markdownKeymap,
        ...defaultKeymap,
        ...historyKeymap,
      ]),
      // файлы из буфера и перетаскивания — во вложения (встроенный drop вставил бы их текстом)
      EditorView.domEventHandlers({
        paste(e) {
          const files = [...(e.clipboardData?.files ?? [])];
          if (!files.length) return false;
          e.preventDefault();
          o.onFiles(files, null);
          return true;
        },
        drop(e, view) {
          const files = [...(e.dataTransfer?.files ?? [])];
          if (!files.length) return false;
          e.preventDefault();
          // неточный режим: ближайшая позиция и при сбросе на поля или пустую строку
          o.onFiles(files, view.posAtCoords({ x: e.clientX, y: e.clientY }, false));
          return true;
        },
      }),
      EditorView.updateListener.of((u: ViewUpdate) => {
        if (u.docChanged) o.onChange();
      }),
    ],
  });
}

export function createEditor(parent: HTMLElement, doc: string, o: EditorOpts): EditorHandle {
  const view = new EditorView({ state: makeState(doc, o), parent });
  return {
    text: () => view.state.sliceDoc(),
    reset: (t) => view.setState(makeState(t, o)),
    insertAt: (pos, s) => {
      const st = view.state;
      const len = st.toText(s).length;
      if (pos === null) {
        const from = st.selection.main.from;
        view.dispatch(st.replaceSelection(s));
        view.focus();
        return from + len;
      }
      const at = Math.max(0, Math.min(pos, st.doc.length));
      view.dispatch({ changes: { from: at, insert: s }, selection: { anchor: at + len } });
      view.focus();
      return at + len;
    },
    lineBreak: () => view.state.lineBreak,
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}
