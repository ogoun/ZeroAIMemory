//! `zam code …` — карта кода решения (спецификация M5.6 §4, §5, §9 CLI, §12, §13).
//!
//! - [`config`] — секция `[code]` клиента; [`scan`] — отпечатки по git и обходу;
//!   [`state`] — `code-state.json` и `code-sync.json`; [`sync`] — синхронизация, журнал
//!   падений, замок; [`view`] — `status`, `map`, `sym`, `refs`, `impact`.
//! - Корень — корень проекта хуков ([`super::hook::project_root`]), решение — `solution.toml`.
//!   Корень и каталог найденного `solution.toml` обязаны совпадать (ревью задачи 5, I3):
//!   иначе синхронизация с подкаталога или из чужого корня удалила бы из карты остальное.
//! - `zam code sync --background` — тот же `sync`, но весь вывод — одна строка итога или ошибки
//!   в `.zeroaimemory/logs/hooks.log` (запускают хуки, задача 6); stdout и stderr пусты.
//! - Коды выхода — ТЗ §13.2: нет решения или неверный `[code]` — 1, сервер недоступен — 2,
//!   неверный ввод — 6; идущая синхронизация и `code.enabled = false` — 0 со строкой в stderr.
pub mod auto;
pub mod config;
pub mod scan;
pub mod state;
pub mod sync;
pub mod view;

use std::path::Path;

use clap::Subcommand;

use super::{Common, Ctx, connect, emit, emit_json, home_dir, refs, solution_guid};
use crate::client::CliError;

/// Источник строк фоновой синхронизации в `hooks.log`.
pub const LOG_SOURCE: &str = "code-sync";

#[derive(Subcommand, Debug)]
pub enum CodeCmd {
    /// Синхронизировать карту: отпечатки → /code/sync → разбор нужных файлов → /code/files
    Sync {
        /// Разобрать и загрузить все файлы, а не только изменившиеся
        #[arg(long)]
        full: bool,
        /// Фоновый режим хуков: итог или ошибка — одной строкой в .zeroaimemory/logs/hooks.log
        #[arg(long)]
        background: bool,
        /// Разрешить удаление из карты больше половины файлов прошлой синхронизации
        #[arg(long)]
        prune: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Состояние карты: файлы по языкам, пропуски, центральные символы
    Status {
        #[command(flatten)]
        common: Common,
    },
    /// Карта в бюджете токенов
    Map {
        /// Фокус: путь (есть `/` или расширение кода) или имя символа; повторяемый
        #[arg(long, value_name = "ПУТЬ|ИМЯ")]
        focus: Vec<String>,
        /// Текст задачи (focus.text, до 2000 символов)
        #[arg(long)]
        task: Option<String>,
        /// Бюджет, токенов (1..=20000; по умолчанию — code.map_tokens сервера)
        #[arg(long)]
        tokens: Option<usize>,
        #[command(flatten)]
        common: Common,
    },
    /// Карточка символа: определение, кто ссылается, привязанные знания
    Sym {
        /// Имя, qual (`Store::open`) или путь#qual
        #[arg(value_name = "ИМЯ|QUAL")]
        name: String,
        #[command(flatten)]
        common: Common,
    },
    /// Кто ссылается на символ
    Refs {
        #[arg(value_name = "ИМЯ|QUAL")]
        name: String,
        #[command(flatten)]
        common: Common,
    },
    /// Изменённые символы рабочей копии → привязанные знания и файлы-потребители
    Impact {
        /// Изменённые после коммита (git diff C..HEAD и git status); иначе — против code-state.json
        #[arg(long)]
        since: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

/// Подключение, решение и корень проекта команды.
fn open(common: &Common) -> Result<(Ctx, String, std::path::PathBuf), CliError> {
    let ctx = connect(common)?;
    let guid = solution_guid(&ctx, None)?;
    let (cwd, root) = refs::cli_root();
    check_root(&cwd, &root)?;
    Ok((ctx, guid, root))
}

/// Корень проекта — каталог найденного `solution.toml` (`<d>/.zeroaimemory/solution.toml` или
/// `<d>/solution.toml`); иначе код 1 с объяснением.
fn check_root(cwd: &Path, root: &Path) -> Result<(), CliError> {
    let Some(sol) = crate::locate::find_solution(cwd) else {
        return Ok(()); // решения нет — ошибку даст solution_guid
    };
    let parent = sol.parent().unwrap_or(Path::new(""));
    let dir = if parent.file_name().is_some_and(|n| n == ".zeroaimemory") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(dir) == canon(root) {
        return Ok(());
    }
    Err(CliError::config(format!(
        "корень проекта {} не совпадает с каталогом решения {} ({}): ниже корня решения есть \
         свой .zeroaimemory/ — запустите zam code из {} или удалите лишний .zeroaimemory/",
        root.display(),
        dir.display(),
        sol.display(),
        dir.display()
    )))
}

pub fn run(c: CodeCmd) -> Result<(), CliError> {
    match c {
        CodeCmd::Sync {
            full,
            background: true,
            prune,
            common,
        } => background(&common, sync::SyncOpts { full, prune }),
        CodeCmd::Sync {
            full,
            prune,
            common,
            ..
        } => {
            let (ctx, guid, root) = open(&common)?;
            match run_sync(&ctx, &guid, &root, sync::SyncOpts { full, prune })? {
                Outcome::Done(r) if ctx.json => {
                    emit_json(&serde_json::to_value(&r).unwrap_or_default())
                }
                Outcome::Done(r) => {
                    for w in &r.warnings {
                        eprintln!("предупреждение: {w}");
                    }
                    emit(format!("{}\n", r.line()).as_bytes())
                }
                Outcome::Skipped(why) => {
                    eprintln!("{why}");
                    Ok(())
                }
            }
        }
        CodeCmd::Status { common } => {
            let (ctx, guid, root) = open(&common)?;
            view::status(&ctx, &guid, &root)
        }
        CodeCmd::Map {
            focus,
            task,
            tokens,
            common,
        } => {
            let (ctx, guid, root) = open(&common)?;
            view::map(
                &ctx,
                &guid,
                &root,
                &view::MapOpts {
                    focus: &focus,
                    task: task.as_deref(),
                    tokens,
                },
            )
        }
        CodeCmd::Sym { name, common } => {
            let (ctx, guid, _) = open(&common)?;
            view::sym(&ctx, &guid, &name)
        }
        CodeCmd::Refs { name, common } => {
            let (ctx, guid, _) = open(&common)?;
            view::refs_of(&ctx, &guid, &name)
        }
        CodeCmd::Impact { since, common } => {
            let (ctx, guid, root) = open(&common)?;
            let cfg = config::load(&home_dir(), &root)?;
            view::impact(&ctx, &guid, &root, &cfg, since.as_deref())
        }
    }
}

/// Итог `sync`: выполнена или не начата (с причиной).
enum Outcome {
    Done(sync::SyncReport),
    Skipped(String),
}

/// Конфигурация, выключатель, замок и синхронизация.
fn run_sync(ctx: &Ctx, guid: &str, root: &Path, opts: sync::SyncOpts) -> Result<Outcome, CliError> {
    let cfg = config::load(&home_dir(), root)?;
    if !cfg.enabled {
        return Ok(Outcome::Skipped(
            "карта кода выключена (code.enabled = false)".into(),
        ));
    }
    let Some(_lock) = sync::SyncLock::acquire(root)? else {
        return Ok(Outcome::Skipped(
            "синхронизация уже идёт (.zeroaimemory/cache/code-sync.lock)".into(),
        ));
    };
    sync::sync(ctx, guid, root, &cfg, opts).map(Outcome::Done)
}

/// `--background`: одна строка в `hooks.log`, stdout и stderr пусты; код ошибки сохраняется.
fn background(common: &Common, opts: sync::SyncOpts) -> Result<(), CliError> {
    let (_, root) = refs::cli_root();
    let log = |msg: &str| {
        super::hook::log_line(
            &root.join(".zeroaimemory").join("logs").join("hooks.log"),
            LOG_SOURCE,
            msg,
        )
    };
    let res = open(common).and_then(|(ctx, guid, root)| run_sync(&ctx, &guid, &root, opts));
    match res {
        Ok(Outcome::Done(r)) => {
            log(&r.line_with_warnings());
            Ok(())
        }
        Ok(Outcome::Skipped(why)) => {
            log(&why);
            Ok(())
        }
        Err(e) => {
            log(&format!("ошибка: {}", e.msg));
            Err(CliError::new(e.code, ""))
        }
    }
}
