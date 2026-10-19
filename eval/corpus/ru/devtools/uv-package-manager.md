---
tags: [python, uv, зависимости]
---
# uv вместо связки venv + pip + pip-tools

uv — менеджер пакетов и проектов для Python от Astral, написан на Rust. Мы перевели на него
три сервиса; главное, что изменилось, — скорость установки (холодная — в разы, с горячим
кэшем — почти мгновенно) и один инструмент вместо четырёх.

## Чем отличается от обычного venv

`python -m venv` только создаёт окружение; ставит пакеты pip, фиксирует версии pip-tools,
версию самого Python выбирает pyenv. uv делает всё это сам:

| Задача | Раньше | С uv |
|--------|--------|------|
| Поставить Python | pyenv install 3.12 | `uv python install 3.12` |
| Создать окружение | `python -m venv .venv` | `uv venv` (или автоматически) |
| Добавить зависимость | правка requirements.in + pip-compile | `uv add httpx` |
| Зафиксировать версии | `pip-compile` | `uv lock` → `uv.lock` |
| Установить ровно по lock | `pip-sync` | `uv sync` |
| Запустить команду | активировать, потом запуск | `uv run pytest` |

Окружение uv — обычный `.venv` по стандарту, его понимают IDE и `python -m`.

## Рабочий цикл

```bash
uv init --package billing      # pyproject.toml + src-раскладка
uv add fastapi "sqlalchemy>=2"
uv add --dev pytest ruff
uv run pytest                  # сам синхронизирует .venv с uv.lock
```

В CI:

```bash
uv sync --locked               # ошибка, если uv.lock не соответствует pyproject.toml
uv run --no-sync pytest
```

`--locked` — аналог строгой установки по lock-файлу: если кто-то поменял `pyproject.toml` и не
обновил `uv.lock`, сборка падает, а не тихо переразрешает версии.

## Совместимый режим

Если проект пока на requirements-файлах, есть `uv pip` с тем же интерфейсом, что у pip:

```bash
uv venv
uv pip install -r requirements.txt
uv pip compile requirements.in -o requirements.txt   # замена pip-compile
```

## Грабли

- `uv.lock` кроссплатформенный (универсальное разрешение); в отличие от `pip-compile` не нужно
  держать отдельные файлы под Windows и Linux.
- Кэш (`~/.cache/uv`, на Windows `%LOCALAPPDATA%\uv\cache`) использует жёсткие ссылки; если кэш
  и проект на разных дисках, uv копирует файлы и предупреждает — задайте `UV_LINK_MODE=copy`,
  чтобы убрать предупреждение.
- В CI кэш стоит чистить `uv cache prune --ci` перед сохранением: удаляет то, что быстрее
  скачать заново, чем хранить.
- uv по умолчанию не ставит pip в окружение; инструменты, которые зовут `python -m pip`,
  ломаются. Лечится `uv venv --seed`.
