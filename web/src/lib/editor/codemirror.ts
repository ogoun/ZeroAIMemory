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
  /** Вставляет строку в позицию курсора (вместо выделения). */
  insert(s: string): void;
  focus(): void;
  destroy(): void;
}

export interface EditorOpts {
  /** Документ изменён пользователем. */
  onChange: () => void;
  /** Ctrl+S / Cmd+S. */
  onSave: () => void;
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
    insert: (s) => {
      view.dispatch(view.state.replaceSelection(s));
      view.focus();
    },
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}
