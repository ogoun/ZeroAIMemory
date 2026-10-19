# Python: виртуальное окружение venv

Модуль `venv` входит в стандартную поставку Python 3 и создаёт каталог со своей копией
интерпретатора (точнее, ссылкой на него) и собственным `site-packages`. Пакеты ставятся туда, а
не в общий Python системы, и проекты не мешают друг другу.

```bash
python -m venv .venv
source .venv/bin/activate        # Linux, macOS
.venv\Scripts\Activate.ps1       # PowerShell
python -m pip install -r requirements.txt
deactivate
```

Грабли:

- PowerShell не даёт выполнить `Activate.ps1`, пока политика `Restricted`; хватает
  `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`.
- Активация не обязательна: можно звать `.venv/bin/python` напрямую — так надёжнее в скриптах
  и cron.
- Окружение не переносится: внутри абсолютные пути. Переместили папку — создайте заново.
- На Debian и Ubuntu `venv` вынесен в пакет `python3-venv`, а голый `pip install` в системный
  Python запрещён (PEP 668, ошибка `externally-managed-environment`).
- `.venv/` — в `.gitignore`, в репозитории лежит только список зависимостей.
