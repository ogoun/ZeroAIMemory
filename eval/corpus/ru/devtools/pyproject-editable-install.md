# pyproject.toml и editable-установка

## Минимальный pyproject.toml

С PEP 517/518/621 вся метаинформация пакета живёт в одном файле; `setup.py` больше не нужен.

```toml
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[project]
name = "billing"
version = "0.4.1"
requires-python = ">=3.11"
dependencies = ["httpx>=0.27", "pydantic>=2"]

[project.optional-dependencies]
dev = ["pytest", "ruff"]

[project.scripts]
billing = "billing.cli:main"
```

`[build-system]` говорит фронтенду (pip, build, uv), каким бэкендом собирать: hatchling,
setuptools, flit-core, pdm-backend. `[project.scripts]` создаёт консольную команду при установке.

## Editable install

```bash
pip install -e ".[dev]"
```

Пакет ставится так, что импорт идёт прямо из рабочей копии: правки в исходниках видны без
переустановки. С PEP 660 это делает сам бэкенд — через `.pth`-файл или import-хук.

Грабли, на которые наступали:

- **Без `[build-system]`** старый pip молча откатывается на `setup.py develop`, а свежий
  setuptools ругается. Таблицу указываем всегда.
- **Новые точки входа и зависимости** не подхватываются сами: после правки `[project.scripts]`
  или `dependencies` нужно повторить `pip install -e .`.
- **Статические анализаторы.** Если setuptools сделал editable через import-хук, mypy и Pylance
  не видят пакет. Помогает `--config-settings editable_mode=compat` или src-раскладка с
  `.pth`-путём.
- **Плоская раскладка** (`billing/` в корне) позволяет тестам случайно импортировать пакет из
  текущего каталога, а не установленный. Поэтому `src/billing/`: тесты гарантированно
  работают с установленной версией.

## Где фиксировать версии

`dependencies` в pyproject — диапазоны для библиотеки. Точные версии для приложения — в
lock-файле: `pip-compile pyproject.toml -o requirements.txt` из pip-tools (затем `pip-sync`) или
`uv lock`. Не путать: диапазоны — что допустимо, lock — что установлено.

Проверка, что пакет собирается не только в editable-режиме: `python -m build` и установка
полученного wheel в чистое окружение. Так ловятся забытые файлы данных, которые в рабочей
копии лежат рядом и потому «работают».
