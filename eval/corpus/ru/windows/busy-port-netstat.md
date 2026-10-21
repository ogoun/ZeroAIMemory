# Порт занят: кто его держит (netstat -ano, Get-NetTCPConnection)

Сервер при старте падает с одной из двух ошибок, и это **разные** ситуации:

- `WSAEADDRINUSE` (10048): «Only one usage of each socket address (protocol/network
  address/port) is normally permitted» — порт действительно кто-то слушает;
- `WSAEACCES` (10013): «An attempt was made to access a socket in a way forbidden by its
  access permissions» — порт никто не слушает, но он входит в зарезервированный системой
  диапазон.

## Найти процесс

Через cmd:

```
netstat -ano | findstr :8080
  TCP    0.0.0.0:8080     0.0.0.0:0     LISTENING     14236
tasklist /FI "PID eq 14236"
```

Последняя колонка — PID. Через PowerShell сразу с именем процесса:

```powershell
Get-NetTCPConnection -LocalPort 8080 -State Listen |
  Select-Object LocalAddress, LocalPort, OwningProcess,
    @{n='Name'; e={(Get-Process -Id $_.OwningProcess).ProcessName}}
```

Завершить: `Stop-Process -Id 14236` или `taskkill /PID 14236 /F`. Часто это зависший
экземпляр того же сервера, оставшийся от прошлого запуска в другом терминале.

## Особые случаи

- **PID 4 (System).** Порт держит драйвер `http.sys`: его используют IIS, WinRM, служба
  публикации и т. п. Кто именно зарегистрировал адрес, покажет
  `netsh http show servicestate`.
- **Порт свободен, но 10013.** Hyper-V, WSL2 и Docker Desktop через WinNAT резервируют
  диапазоны портов, и они меняются после перезагрузки. Посмотреть:

  ```
  netsh interface ipv4 show excludedportrange protocol=tcp
  ```

  Обходные пути: выбрать порт вне диапазонов или закрепить свой порт до того, как его
  захватит резервирование:
  `netsh int ipv4 add excludedportrange protocol=tcp startport=8080 numberofports=1`
  (от администратора, после временной остановки службы `winnat`).
- **Слушает на 127.0.0.1, а вы ходите на IPv6 `::1`.** `localhost` может разрешаться в
  IPv6; в выводе `netstat` это видно по адресу `[::1]:8080`. Тогда «порт занят» и
  «соединение отклонено» могут чередоваться.
- **Состояние TIME_WAIT.** После остановки сервера в выводе остаются строки `TIME_WAIT` с
  PID 0 — это недавно закрытые соединения. Искать виновника надо по строке `LISTENING`, а
  не по ним.

## Короткий чек-лист

1. `netstat -ano | findstr :PORT` — есть ли `LISTENING`.
2. Если есть — PID → `tasklist` → решить, убивать ли.
3. Если нет, а ошибка 10013 — `excludedportrange`.
4. Если PID 4 — `netsh http show servicestate`.
