//! `zam` — клиент ZeroAIMemory. Без tokio и tracing (холодный старт ≤ 20 мс).
//!
//! Коды выхода: 0 — успех; 1 — конфигурация; 2 — сервер недоступен; 3 — токен;
//! 4 — конфликт; 5 — не найдено; 6 — неверный ввод (в том числе аргументы).
mod client;
mod cmd;
mod locate;
mod target;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use client::{CliError, EXIT_INPUT};
use cmd::Common;
use locate::Flags;

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ZAM_GIT_SHA"), ")");

#[derive(Parser)]
#[command(name = "zam", version = VERSION, about = "Клиент ZeroAIMemory")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Проверить подключение: сервер, токен, пользователь, solution.toml
    Status {
        #[command(flatten)]
        common: Common,
    },
    /// Дерево пространства с L0: [space:]path (без префикса — решение или knowledge)
    Tree {
        target: Option<String>,
        /// Глубина (1–5)
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=5))]
        depth: u32,
        #[command(flatten)]
        common: Common,
    },
    /// Прочитать заметки: [space:]path или id; Markdown в stdout как сохранён
    Read {
        #[arg(required = true)]
        targets: Vec<String>,
        /// Уровень: L0 (сводка), L1 (оглавление), L2 (полный текст)
        #[arg(long, value_parser = ["L0", "L1", "L2", "l0", "l1", "l2"])]
        tier: Option<String>,
        /// Секция по пути заголовков: "A > B"
        #[arg(long)]
        section: Option<String>,
        /// Ревизия
        #[arg(long)]
        rev: Option<i64>,
        #[command(flatten)]
        common: Common,
    },
    /// Записать заметку из --file или stdin (новая — создаётся, есть — заменяется)
    Write {
        target: String,
        /// Файл с текстом (иначе stdin)
        #[arg(long)]
        file: Option<PathBuf>,
        /// Сообщение ревизии
        #[arg(long, short = 'm')]
        message: Option<String>,
        /// Ревизия, от которой сделана правка (иначе текущая)
        #[arg(long)]
        base_rev: Option<i64>,
        /// Сводка L0
        #[arg(long)]
        l0: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Ревизии заметки; --diff A..B — дифф, --revert N — откат
    History {
        target: String,
        /// Диапазон ревизий A..B (A.. — до текущей)
        #[arg(long, conflicts_with = "revert")]
        diff: Option<String>,
        /// Откатить к ревизии N (создаётся новая ревизия)
        #[arg(long)]
        revert: Option<i64>,
        #[command(flatten)]
        common: Common,
    },
    /// Поиск: Markdown по умолчанию
    Search {
        query: String,
        /// Во всех решениях
        #[arg(long)]
        all: bool,
        /// Режим выдачи
        #[arg(long, default_value = "brief", value_parser = ["brief", "chunks", "notes"])]
        mode: String,
        /// Число результатов (1–100)
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
        top_k: Option<u32>,
        /// Серверный реранк первых кандидатов (медленнее, точнее порядок; нужна модель
        /// реранкера: zam-server models fetch --reranker)
        #[arg(long)]
        rerank: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Прикрепить файл к заметке; печатает ссылку для вставки
    Attach {
        target: String,
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Журналы решения: запись в Changelog
    Log {
        #[command(subcommand)]
        cmd: LogCmd,
    },
    /// Релизы решения: добавить, сравнить
    Release {
        #[command(subcommand)]
        cmd: ReleaseCmd,
    },
    /// Решение: совместный доступ
    Solution {
        #[command(subcommand)]
        cmd: SolutionCmd,
    },
}

#[derive(Subcommand)]
enum LogCmd {
    /// Запись об изменении (текст — из --body-file или stdin)
    Changelog {
        /// Вид: feat, fix, refactor, docs, research, ops, other
        #[arg(long, value_parser = ["feat", "fix", "refactor", "docs", "research", "ops", "other"])]
        kind: String,
        #[arg(long)]
        title: String,
        /// Файл с текстом (иначе stdin)
        #[arg(long)]
        body_file: Option<PathBuf>,
        /// Ссылка: commit:SHA | file:PATH | note:ID | issue:X (можно повторять)
        #[arg(long = "ref")]
        refs: Vec<String>,
        /// Область (монорепозиторий)
        #[arg(long)]
        area: Option<String>,
        /// GUID решения (иначе agent_solution_id из solution.toml)
        #[arg(long)]
        solution: Option<String>,
        /// Ключ идемпотентности: повтор с тем же ключом не создаёт вторую запись
        #[arg(long)]
        key: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand)]
enum ReleaseCmd {
    /// Добавить релиз
    Add {
        version: String,
        /// Дата YYYY-MM-DD (по умолчанию сегодня, UTC)
        #[arg(long)]
        date: Option<String>,
        /// Git-тег релиза
        #[arg(long)]
        tag: Option<String>,
        /// Файл с описанием релиза
        #[arg(long)]
        notes_file: Option<PathBuf>,
        /// Собрать коммиты из git: FROM..TO (без диапазона — от предыдущего тега до HEAD)
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        from_git: Option<String>,
        /// GUID решения (иначе agent_solution_id из solution.toml)
        #[arg(long)]
        solution: Option<String>,
        /// Ключ идемпотентности
        #[arg(long)]
        key: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Сравнить релиз с предыдущим или указанным: changelog, описание, коммиты
    Compare {
        /// Версия или id релиза
        release: String,
        /// С чем сравнивать: prev (по умолчанию), версия или id
        #[arg(long = "with")]
        with: Option<String>,
        /// GUID решения (иначе agent_solution_id из solution.toml)
        #[arg(long)]
        solution: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand)]
enum SolutionCmd {
    /// Доступ других пользователей к решению (владелец или администратор)
    Access {
        #[command(subcommand)]
        cmd: AccessCmd,
    },
}

/// Решение и подключение для команд доступа.
#[derive(clap::Args)]
struct AccessOpts {
    /// GUID решения (иначе agent_solution_id из solution.toml)
    #[arg(long)]
    solution: Option<String>,
    #[command(flatten)]
    common: Common,
}

#[derive(Subcommand)]
enum AccessCmd {
    /// Все запросы и участники
    List {
        #[command(flatten)]
        opts: AccessOpts,
    },
    /// Ожидающие подтверждения запросы
    Requests {
        #[command(flatten)]
        opts: AccessOpts,
    },
    /// Одобрить запрос (по умолчанию rw) или сменить режим участника
    Approve {
        /// Пользователь: имя или id
        user: String,
        /// Только чтение
        #[arg(long)]
        ro: bool,
        #[command(flatten)]
        opts: AccessOpts,
    },
    /// Отклонить ожидающий запрос
    Reject {
        /// Пользователь: имя или id
        user: String,
        /// Причина
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        opts: AccessOpts,
    },
    /// Отозвать доступ участника (действует со следующего запроса)
    Revoke {
        /// Пользователь: имя или id
        user: String,
        #[command(flatten)]
        opts: AccessOpts,
    },
}

impl AccessCmd {
    fn split(self) -> (cmd::solution::Action, AccessOpts) {
        use cmd::solution::Action;
        match self {
            AccessCmd::List { opts } => (Action::List, opts),
            AccessCmd::Requests { opts } => (Action::Requests, opts),
            AccessCmd::Approve { user, ro, opts } => (Action::Approve { user, ro }, opts),
            AccessCmd::Reject { user, reason, opts } => (Action::Reject { user, reason }, opts),
            AccessCmd::Revoke { user, opts } => (Action::Revoke { user }, opts),
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // --help и --version — успех; ошибка аргументов — неверный ввод (6), не 2
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { EXIT_INPUT } else { 0 });
        }
    };
    if let Cmd::Status { common } = &cli.cmd {
        let flags = Flags {
            url: common.url.clone(),
            token: common.token.clone(),
        };
        return ExitCode::from(cmd::status::run(flags, common.json));
    }
    match dispatch(cli.cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if !e.msg.is_empty() {
                eprintln!("{e}");
            }
            ExitCode::from(e.code)
        }
    }
}

fn dispatch(c: Cmd) -> Result<(), CliError> {
    match c {
        Cmd::Status { .. } => unreachable!("обработан в main"),
        Cmd::Tree {
            target,
            depth,
            common,
        } => cmd::tree::run(&cmd::connect(&common)?, target.as_deref(), depth),
        Cmd::Read {
            targets,
            tier,
            section,
            rev,
            common,
        } => {
            let o = cmd::read::Opts {
                tier: tier.as_deref(),
                section: section.as_deref(),
                rev,
            };
            cmd::read::run(&cmd::connect(&common)?, &targets, &o)
        }
        Cmd::Write {
            target,
            file,
            message,
            base_rev,
            l0,
            common,
        } => {
            let o = cmd::write::Opts {
                file: file.as_deref(),
                message: message.as_deref(),
                base_rev,
                l0: l0.as_deref(),
            };
            cmd::write::run(&cmd::connect(&common)?, &target, &o)
        }
        Cmd::History {
            target,
            diff,
            revert,
            common,
        } => {
            let action = match (diff, revert) {
                (Some(d), _) => cmd::history::Action::Diff(d),
                (None, Some(r)) => cmd::history::Action::Revert(r),
                (None, None) => cmd::history::Action::List,
            };
            cmd::history::run(&cmd::connect(&common)?, &target, &action)
        }
        Cmd::Search {
            query,
            all,
            mode,
            top_k,
            rerank,
            common,
        } => {
            let o = cmd::search::Opts {
                all,
                mode: &mode,
                top_k,
                rerank,
            };
            cmd::search::run(&cmd::connect(&common)?, &query, &o)
        }
        Cmd::Attach {
            target,
            file,
            common,
        } => cmd::attach::run(&cmd::connect(&common)?, &target, &file),
        Cmd::Log {
            cmd:
                LogCmd::Changelog {
                    kind,
                    title,
                    body_file,
                    refs,
                    area,
                    solution,
                    key,
                    common,
                },
        } => {
            let o = cmd::log::Changelog {
                kind: &kind,
                title: &title,
                body_file: body_file.as_deref(),
                refs: &refs,
                area: area.as_deref(),
                solution: solution.as_deref(),
                key: key.as_deref(),
            };
            cmd::log::changelog(&cmd::connect(&common)?, &o)
        }
        Cmd::Release {
            cmd:
                ReleaseCmd::Add {
                    version,
                    date,
                    tag,
                    notes_file,
                    from_git,
                    solution,
                    key,
                    common,
                },
        } => {
            let o = cmd::release::Add {
                version: &version,
                date: date.as_deref(),
                tag: tag.as_deref(),
                notes_file: notes_file.as_deref(),
                from_git: from_git.as_deref(),
                solution: solution.as_deref(),
                key: key.as_deref(),
            };
            cmd::release::add(&cmd::connect(&common)?, &o)
        }
        Cmd::Release {
            cmd:
                ReleaseCmd::Compare {
                    release,
                    with,
                    solution,
                    common,
                },
        } => cmd::release::compare(
            &cmd::connect(&common)?,
            &release,
            with.as_deref(),
            solution.as_deref(),
        ),
        Cmd::Solution {
            cmd: SolutionCmd::Access { cmd },
        } => {
            let (action, opts) = cmd.split();
            cmd::solution::run(
                &cmd::connect(&opts.common)?,
                opts.solution.as_deref(),
                &action,
            )
        }
    }
}
