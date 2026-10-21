# Windows: keep the data directory path under 190 characters

Gotcha found while testing the managed Qdrant on Windows.

## The rule

On Windows the full path of `data_dir` must be at most 190 characters. Files inside it have
paths 60–160 characters longer than the directory itself, and two consumers still go through
plain Win32 paths:

- SQLite databases are opened with ordinary Win32 paths, limited by MAX_PATH = 260;
- the working directory of the Qdrant child process (`<data_dir>/qdrant/`) is also bound by
  MAX_PATH.

Qdrant's own storage copes with paths longer than 260, so the limit comes from us, not from it.
The `\\?\` long-path prefix is not counted towards the 190.

## What happens if it is longer

`serve` and every command that touches the data directory (`migrate`, `reindex`, `qdrant …`,
`models …`, `cuda fetch`) fail immediately, before any download, with an error saying the data
directory is too long for Windows. Fix by moving it or pointing to a shorter location via
`data_dir` in `config.toml`, `ZAM_DATA_DIR` or `--data-dir`.

Failing early was a deliberate choice: the alternative was a server that starts, accepts writes
and then breaks deep inside Qdrant or SQLite on the first long file name.

## Linux

No such limit on Linux.
