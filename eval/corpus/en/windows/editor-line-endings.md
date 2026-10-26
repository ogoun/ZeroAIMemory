---
index: false
tags: [editors, line-endings]
---
# Line endings in editors: VS Code, Visual Studio, Notepad, .editorconfig

Repository rules decide what gets committed, but the editor decides what gets written to
disk while you work. If they disagree, you get files with mixed endings and noisy diffs.
These are my notes on making editors behave on Windows.

## VS Code

- The status bar shows `CRLF` or `LF` for the current file; click it to convert.
- `"files.eol": "\n"` in settings sets the ending for **new** files only. Existing files keep
  whatever they have.
- Per-language override, useful for batch files:

  ```json
  "[bat]": { "files.eol": "\r\n" }
  ```

- With the EditorConfig extension, `.editorconfig` overrides `files.eol`.

## Visual Studio

Visual Studio preserves the endings of an existing file. When a file has a mix, it shows
the "Inconsistent Line Endings" dialog on open and offers to normalize. Saying "No" leaves
the mix, which later surfaces as whole-file diffs. Advanced Save Options (under File, may
need to be added to the menu) let you pick an ending per file.

## Notepad and others

Since Windows 10 1809, Notepad reads and preserves LF and CR-only files and shows the
format in the status bar. Older tools (and some installers that template config files)
still write CRLF unconditionally.

JetBrains IDEs show the separator in the status bar and have a project-wide default under
Code Style → Line separator.

## .editorconfig: one rule for every editor

```ini
root = true

[*]
end_of_line = lf
insert_final_newline = true
charset = utf-8

[*.{bat,cmd}]
end_of_line = crlf
```

Most editors honor it natively or through a plugin. It complements, not replaces, the
repository's attributes file: the editor writes the right ending, the VCS stores it
consistently.

## Detecting mixed endings

```bash
file src/*.ts                      # reports "with CRLF line terminators"
grep -rlI $'\r' --include='*.sh' .
```

In PowerShell: `(Get-Content -Raw f.txt) -match "\r\n"` returns `True` if any CRLF exists.
