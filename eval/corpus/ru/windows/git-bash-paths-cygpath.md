# Пути в Git Bash: /c/..., автоконвертация MSYS и cygpath

Git Bash (MSYS2 внутри Git for Windows) показывает диски как каталоги в корне: `C:\Users\me`
превращается в `/c/Users/me`. Пока работают только программы из самого MSYS (`ls`, `grep`,
`sed`), это незаметно. Проблемы начинаются, когда путь уходит в обычную Windows-программу.

## Автоматическая конвертация аргументов

При запуске не-MSYS программы рантайм MSYS смотрит на аргументы и переписывает всё, что
похоже на POSIX-путь, в Windows-путь. Обычно это помогает: `notepad /c/temp/a.txt` откроет
`C:\temp\a.txt`. Но эвристика срабатывает и там, где путь не подразумевался:

```bash
cmd /c dir            # /c превращается в C:/Program Files/Git/c — cmd не понимает
reg query HKLM /v X   # /v превращается в путь
some-tool --prefix=/api/v1   # /api/v1 тоже станет путём внутри каталога Git
```

Способы отключить:

```bash
cmd //c dir                          # двойной слеш — конвертация не трогает
MSYS_NO_PATHCONV=1 some-tool --prefix=/api/v1   # отключить для одной команды (Git for Windows)
MSYS2_ARG_CONV_EXCL='--prefix=' some-tool --prefix=/api/v1   # исключить по префиксу
```

`MSYS_NO_PATHCONV` — переменная именно сборки Git for Windows; в чистом MSYS2 работает
`MSYS2_ARG_CONV_EXCL='*'`.

## cygpath — явное преобразование

Когда путь нужно передать в конфиг или переменную, лучше преобразовать его явно:

```bash
cygpath -w /c/Users/me/project     # C:\Users\me\project
cygpath -m /c/Users/me/project     # C:/Users/me/project  (смешанный, удобен в конфигах)
cygpath -u 'C:\Users\me\project'   # /c/Users/me/project
cygpath -w "$HOME"                 # домашний каталог в Windows-виде
```

Вариант `-m` (прямые слеши с буквой диска) понимают почти все Windows-программы, и его не
надо экранировать в строках JSON или TOML.

## Прочие грабли

- Переменная `PATH` в Git Bash хранится в POSIX-виде (`/c/Windows/system32:...`), а при
  передаче в Windows-процесс конвертируется обратно в `;`-список. Ручная склейка через `;`
  внутри Git Bash ломает это преобразование.
- Скрипт, который проверяет `[[ $path == C:* ]]`, не сработает: в Git Bash путь начнётся с
  `/c/`. Нормализовать через `cygpath -u` перед сравнением.
- В WSL схема другая: диск монтируется в `/mnt/c`, а для преобразования есть `wslpath`.
  Путать их не стоит — `cygpath` в WSL нет.
