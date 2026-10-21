---
tags: [pre-commit, git-hooks]
---
# Фреймворк pre-commit

pre-commit — менеджер git-хуков: описываешь в `.pre-commit-config.yaml`, какие инструменты
выполнять, он сам ставит их в изолированные окружения и вызывает на индексированных файлах.

```yaml
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v5.0.0
    hooks:
      - id: trailing-whitespace
      - id: end-of-file-fixer
      - id: check-yaml
  - repo: https://github.com/astral-sh/ruff-pre-commit
    rev: v0.8.4
    hooks:
      - id: ruff
        args: [--fix]
      - id: ruff-format
```

```bash
pip install pre-commit
pre-commit install            # пишет .git/hooks/pre-commit
pre-commit run --all-files    # прогон по всему дереву, нужен после подключения
pre-commit autoupdate         # поднять rev до свежих тегов
```

Заметки: хук, исправивший файл, завершается с ошибкой — файл надо снова добавить в индекс.
Обойти разово — `git commit --no-verify`, поэтому в CI тот же конфиг выполняется ещё раз
(`pre-commit run --all-files`).
