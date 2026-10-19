---
tags: [windows, symlink]
---
# Символические ссылки на Windows и режим разработчика

Создание символической ссылки требует привилегии `SeCreateSymbolicLinkPrivilege`, которая по
умолчанию есть только у администраторов. Без неё `mklink` отвечает
`You do not have sufficient privilege to perform this operation.`

С Windows 10 1703 есть выход: **режим разработчика** (Параметры → Для разработчиков). При
включённом режиме обычный пользователь может создавать симлинки, если программа передаёт
флаг `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`. `mklink`, PowerShell 7 и современный git
его передают.

Виды ссылок:

```
mklink link.txt target.txt        :: симлинк на файл
mklink /D linkdir targetdir       :: симлинк на каталог
mklink /J linkdir targetdir       :: junction: без привилегий, только локальные каталоги
mklink /H link.txt target.txt     :: жёсткая ссылка, тот же том
```

В PowerShell: `New-Item -ItemType SymbolicLink -Path link -Target target`.

Git for Windows по умолчанию выписывает симлинки из репозитория как обычные текстовые файлы
с путём цели внутри. Чтобы получать настоящие ссылки, нужны режим разработчика (или права
администратора) и `git config core.symlinks true` до клонирования.
