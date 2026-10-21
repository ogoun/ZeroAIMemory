# ExecutionPolicy: «выполнение сценариев отключено в этой системе»

На свежей клиентской Windows запуск `.\build.ps1` заканчивается ошибкой:

```
File C:\src\build.ps1 cannot be loaded because running scripts is disabled on this system.
```

Это политика выполнения: на клиентских редакциях по умолчанию `Restricted` (скрипты
запрещены), на серверных — `RemoteSigned`. Посмотреть все уровни сразу:

```powershell
Get-ExecutionPolicy -List
```

Что делаю я:

```powershell
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
```

Права администратора не нужны, затрагивается только текущий пользователь. `RemoteSigned`
разрешает локальные скрипты и требует подписи у скачанных.

Для разового запуска в CI или из bat-файла политику можно обойти, не меняя её:

```
powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1
```

Если скрипт скачан из интернета, Windows помечает его альтернативным потоком
`Zone.Identifier`, и `RemoteSigned` его не пустит. Снять пометку: `Unblock-File .\build.ps1`.

Помнить: ExecutionPolicy — защита от случайного запуска, а не граница безопасности. Её
обходит кто угодно, поэтому полагаться на неё как на защиту нельзя.
