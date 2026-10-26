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
    /// Журналы решения: запись в Changelog или Insights
    Log {
        #[command(subcommand)]
        cmd: LogCmd,
    },
    /// Insights: лента, карточка, замещение, отзыв, пересмотр, дописывание
    Insight {
        #[command(subcommand)]
        cmd: InsightCmd,
    },
    /// Релизы решения: добавить, сравнить
    Release {
        #[command(subcommand)]
        cmd: ReleaseCmd,
    },
    /// Vault: секреты (значение — только из stdin или скрытого ввода)
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// Решение: совместный доступ
    Solution {
        #[command(subcommand)]
        cmd: SolutionCmd,
    },
    /// Связь графа: `SRC TYPE DST` — создать (TYPE: links_to, relates_to, depends_on,
    /// part_of, caused_by, fixes, derived_from, mentions; DST — ещё и [[Имя]]); `rm EDGE-ID`
    /// — удалить явную связь
    Link {
        /// SRC TYPE DST или rm EDGE-ID; SRC и DST — n:<id>, e:<id> (i:/c:/r:) или [space:]path
        #[arg(required = true, num_args = 2..=3, value_names = ["SRC|rm", "TYPE|EDGE-ID", "DST"])]
        args: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Связи узла или записи: исходящие (→) и обратные (←)
    Links {
        /// n:<id>, e:<id> (i:/c:/r:) или [space:]path
        target: String,
        #[command(flatten)]
        common: Common,
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
    /// Открытие, провал, эксперимент (текст — из --body-file или stdin); похожий insight —
    /// код 4 со списком кандидатов
    Insight {
        /// Вид: gotcha, failure, discovery, decision, experiment, pattern
        #[arg(long, value_parser = ["gotcha", "failure", "discovery", "decision", "experiment", "pattern"])]
        kind: String,
        #[command(flatten)]
        new: InsightArgs,
        /// solution (по умолчанию) или global — в Knowledge, для всех проектов
        #[arg(long, value_parser = ["solution", "global"])]
        scope: Option<String>,
        /// Записать, даже если похожий insight уже есть
        #[arg(long)]
        force: bool,
        /// Заменить существующий insight (id) этой записью
        #[arg(long)]
        supersedes: Option<String>,
        /// GUID решения (иначе agent_solution_id из solution.toml)
        #[arg(long)]
        solution: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

/// Поля новой записи insight (`log insight`, `insight supersede`).
#[derive(clap::Args)]
struct InsightArgs {
    /// Заголовок
    #[arg(long)]
    title: String,
    /// Файл с текстом (иначе stdin)
    #[arg(long)]
    body_file: Option<PathBuf>,
    /// Эпистемический статус: theorem (с --evidence-file) или hypothesis
    #[arg(long, value_parser = ["theorem", "hypothesis"])]
    certainty: Option<String>,
    /// JSON-файл доказательства {checked, method, result} → data.evidence
    #[arg(long)]
    evidence_file: Option<PathBuf>,
    /// Триггер вспоминания ВИД:ШАБЛОН: file_glob:**/*.sh, command_regex:…, error_regex:…,
    /// keyword:… (можно повторять)
    #[arg(long = "trigger")]
    triggers: Vec<String>,
    /// Тег (можно повторять)
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// Область (монорепозиторий)
    #[arg(long)]
    area: Option<String>,
    /// Ключ идемпотентности
    #[arg(long)]
    key: Option<String>,
}

impl InsightArgs {
    fn fields<'a>(&'a self, kind: Option<&'a str>) -> cmd::insight::Fields<'a> {
        cmd::insight::Fields {
            kind,
            title: &self.title,
            body_file: self.body_file.as_deref(),
            certainty: self.certainty.as_deref(),
            evidence_file: self.evidence_file.as_deref(),
            triggers: &self.triggers,
            tags: &self.tags,
            area: self.area.as_deref(),
            key: self.key.as_deref(),
        }
    }
}

#[derive(Subcommand)]
enum InsightCmd {
    /// Лента insight решения (или Knowledge с --global)
    List {
        /// Статусы через запятую: active, revisit, superseded, retracted
        #[arg(long)]
        status: Option<String>,
        /// Knowledge вместо решения
        #[arg(long)]
        global: bool,
        /// GUID решения (иначе agent_solution_id из solution.toml)
        #[arg(long)]
        solution: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: Option<u32>,
        #[command(flatten)]
        common: Common,
    },
    /// Карточка insight: статус, связи, триггеры, текст
    Show {
        id: String,
        #[command(flatten)]
        common: Common,
    },
    /// Заменить insight новой записью (старая — superseded, ничего не удаляется)
    Supersede {
        id: String,
        /// Вид (по умолчанию — как у замещаемой)
        #[arg(long, value_parser = ["gotcha", "failure", "discovery", "decision", "experiment", "pattern"])]
        kind: Option<String>,
        #[command(flatten)]
        new: InsightArgs,
        #[command(flatten)]
        common: Common,
    },
    /// Отозвать insight с причиной (ничего не удаляется)
    Retract {
        id: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        common: Common,
    },
    /// Пометить «пересмотреть при новых идеях»
    Revisit {
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Сменить эпистемический статус: axiom (только --human), theorem (агенту — с
    /// --evidence-file), hypothesis или none; причина обязательна
    Certify {
        id: String,
        #[arg(value_parser = ["axiom", "theorem", "hypothesis", "none"])]
        certainty: String,
        #[arg(long)]
        reason: String,
        /// JSON-файл доказательства {checked, method, result} → data.evidence
        #[arg(long)]
        evidence_file: Option<PathBuf>,
        /// Человеческий токен (url и human_token — только из ~/.zeroaimemory/config.toml);
        /// только в интерактивном терминале с подтверждением вводом id: axiom ставит и меняет
        /// только человек
        #[arg(long)]
        human: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Дописать к тексту insight (из --body-file или stdin)
    Append {
        id: String,
        #[arg(long)]
        body_file: Option<PathBuf>,
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

/// Область Vault и подключение.
#[derive(clap::Args)]
struct VaultOpts {
    /// Личный Vault (Knowledge, для всех проектов) вместо Vault решения
    #[arg(long)]
    global: bool,
    /// GUID решения (иначе agent_solution_id из solution.toml)
    #[arg(long, conflicts_with = "global")]
    solution: Option<String>,
    #[command(flatten)]
    common: Common,
}

impl VaultOpts {
    fn area(&self) -> cmd::vault::Area<'_> {
        cmd::vault::Area {
            global: self.global,
            solution: self.solution.as_deref(),
        }
    }
}

#[derive(Subcommand)]
enum VaultCmd {
    /// Записи без значений: имя, тип, пользователь или адрес, id
    List {
        /// Поиск по имени, url и описанию
        #[arg(long = "q")]
        q: Option<String>,
        #[command(flatten)]
        opts: VaultOpts,
    },
    /// Значение записи в stdout (раскрытие пишется в аудит)
    Get {
        name: String,
        /// Без перевода строки в конце
        #[arg(long)]
        raw: bool,
        #[command(flatten)]
        opts: VaultOpts,
    },
    /// Записать значение из stdin или скрытого ввода с терминала (не из аргументов)
    Set {
        name: String,
        /// Тип: password, token, ssh_key, connection_string, url, repo, note
        #[arg(long = "type", value_parser = ["password", "token", "ssh_key", "connection_string", "url", "repo", "note"])]
        kind: String,
        /// Имя пользователя (data.username)
        #[arg(long)]
        username: Option<String>,
        /// Адрес ресурса (data.url; `--url` — адрес сервера zam)
        #[arg(long = "link", value_name = "URL")]
        link: Option<String>,
        /// Описание (data.description)
        #[arg(long)]
        description: Option<String>,
        #[command(flatten)]
        opts: VaultOpts,
    },
}

fn vault(c: VaultCmd) -> Result<(), CliError> {
    use cmd::vault as v;
    match c {
        VaultCmd::List { q, opts } => {
            v::list(&cmd::connect(&opts.common)?, &opts.area(), q.as_deref())
        }
        VaultCmd::Get { name, raw, opts } => {
            v::get(&cmd::connect(&opts.common)?, &opts.area(), &name, raw)
        }
        VaultCmd::Set {
            name,
            kind,
            username,
            link,
            description,
            opts,
        } => {
            let s = v::Set {
                name: &name,
                kind: &kind,
                username: username.as_deref(),
                url: link.as_deref(),
                description: description.as_deref(),
            };
            v::set(&cmd::connect(&opts.common)?, &opts.area(), &s)
        }
    }
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

fn insight(c: InsightCmd) -> Result<(), CliError> {
    use cmd::insight as i;
    match c {
        InsightCmd::List {
            status,
            global,
            solution,
            limit,
            common,
        } => i::list(
            &cmd::connect(&common)?,
            status.as_deref(),
            global,
            solution.as_deref(),
            limit,
        ),
        InsightCmd::Show { id, common } => i::show(&cmd::connect(&common)?, &id),
        InsightCmd::Supersede {
            id,
            kind,
            new,
            common,
        } => i::supersede(&cmd::connect(&common)?, &id, &new.fields(kind.as_deref())),
        InsightCmd::Retract { id, reason, common } => {
            i::retract(&cmd::connect(&common)?, &id, &reason)
        }
        InsightCmd::Revisit { id, reason, common } => {
            i::revisit(&cmd::connect(&common)?, &id, reason.as_deref())
        }
        InsightCmd::Append {
            id,
            body_file,
            common,
        } => i::append(&cmd::connect(&common)?, &id, body_file.as_deref()),
        InsightCmd::Certify {
            id,
            certainty,
            reason,
            evidence_file,
            human,
            common,
        } => {
            let ctx = if human {
                cmd::connect_human(&common, &id)?
            } else {
                cmd::connect(&common)?
            };
            i::certify(&ctx, &id, &certainty, &reason, evidence_file.as_deref())
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
        Cmd::Log {
            cmd:
                LogCmd::Insight {
                    kind,
                    new,
                    scope,
                    force,
                    supersedes,
                    solution,
                    common,
                },
        } => {
            let o = cmd::insight::Log {
                fields: new.fields(Some(&kind)),
                scope: scope.as_deref(),
                force,
                supersedes: supersedes.as_deref(),
                solution: solution.as_deref(),
            };
            cmd::insight::log(&cmd::connect(&common)?, &o)
        }
        Cmd::Insight { cmd } => insight(cmd),
        Cmd::Vault { cmd } => vault(cmd),
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
        Cmd::Link { args, common } => match args.as_slice() {
            [rm, id] if rm == "rm" => cmd::link::remove(&cmd::connect(&common)?, id),
            [src, kind, dst] => cmd::link::create(&cmd::connect(&common)?, src, kind, dst),
            _ => Err(CliError::input(
                "zam link SRC TYPE DST или zam link rm EDGE-ID",
            )),
        },
        Cmd::Links { target, common } => cmd::link::list(&cmd::connect(&common)?, &target),
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
