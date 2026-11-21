//! Адрес сервера для `zam init` (M6.6 §10.1, замечание обкатки 1): режим [`UrlTo`] и его вывод
//! из прежнего блока, блок без строки адреса, `CLAUDE.local.md` и его исключение из git
//! (`info/exclude`), итог по адресу. git запускается с потолками поиска `zam`. Чистые функции
//! над текстом — в [`super::merge`].
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

use super::{merge, read_opt, rel, write_project_file};
use crate::client::CliError;
use crate::locate;

/// Куда записать адрес сервера (M6.6 §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum UrlTo {
    /// Строкой ZeroAIMemoryUrl= в блоке CLAUDE.md/AGENTS.md (как раньше)
    Block,
    /// В ~/.zeroaimemory/config.toml; блок без адреса
    Home,
    /// Строкой в CLAUDE.local.md (исключается из git); блок без адреса
    Local,
}

/// Локальный файл Claude Code для `--url-to local`.
pub const LOCAL_MD: &str = "CLAUDE.local.md";

/// `git -C root <args>`: код выхода и stdout без перевода строки в конце; git не запустился —
/// `None`. Потолки поиска `zam` (`ZAM_CEILING_DIRECTORIES`) дописываются в
/// `GIT_CEILING_DIRECTORIES`: git не поднимается выше них, как и поиск файлов проекта — иначе из
/// каталога теста он нашёл бы репозиторий выше и его `info/exclude`.
fn git(root: &Path, args: &[&str]) -> Option<(i32, String)> {
    let mut c = std::process::Command::new("git");
    c.arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(z) = std::env::var_os(zam_core::CEILING_ENV) {
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&z)
            .filter(|p| p.is_absolute())
            .collect();
        if let Some(g) = std::env::var_os("GIT_CEILING_DIRECTORIES") {
            dirs.extend(std::env::split_paths(&g).filter(|p| p.is_absolute()));
        }
        if let Ok(j) = std::env::join_paths(dirs) {
            c.env("GIT_CEILING_DIRECTORIES", j);
        }
    }
    let o = c.output().ok()?;
    let out = String::from_utf8_lossy(&o.stdout);
    Some((
        o.status.code()?,
        out.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

/// Исключён ли `rel` (путь от корня проекта) из git: `Some(false)` — нет или файл уже в индексе
/// (`check-ignore` не применяет правила к отслеживаемым); `None` — не репозиторий git или git
/// недоступен.
pub fn git_ignored(root: &Path, rel: &str) -> Option<bool> {
    match git(root, &["check-ignore", "-q", "--", rel])?.0 {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Есть ли `rel` в индексе git (`ls-files --error-unmatch`); `None` — git не ответил.
fn git_tracked(root: &Path, rel: &str) -> Option<bool> {
    match git(root, &["ls-files", "--error-unmatch", "--", rel])?.0 {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Исключение `CLAUDE.local.md` из git для `--url-to local`.
#[derive(Debug)]
pub enum Exclude {
    /// Уже исключён или проект не в git (коммитить некуда).
    Done,
    /// Дописать строку `line` в `path` (`info/exclude` репозитория, общий для всех worktree);
    /// новый текст файла — `text`.
    Add {
        path: PathBuf,
        text: String,
        line: String,
    },
    /// Строка уже в `path`, а файл не исключён: правило `!` в `.gitignore` сильнее
    /// `info/exclude`.
    Ineffective { path: PathBuf, line: String },
    /// `.git` есть, а git не ответил — проверить нельзя.
    Unknown,
}

/// План исключения `CLAUDE.local.md` (только чтение: `ls-files`, `check-ignore`, `rev-parse`).
/// Строка — `/<префикс>CLAUDE.local.md`, префикс каталога (`--show-prefix`) — с экранированием
/// символов шаблона gitignore. Файл уже в индексе git — отказ (код 1) до записи и регистрации:
/// правила исключения к отслеживаемому файлу не применяются, и адрес ушёл бы в репозиторий
/// следующим коммитом (решение координатора, круг правок 2 задачи 10).
pub fn plan_exclude(root: &Path) -> Result<Exclude, CliError> {
    match git_tracked(root, LOCAL_MD) {
        Some(true) => {
            return Err(CliError::config(format!(
                "{LOCAL_MD} в индексе git — адрес попал бы в репозиторий: git rm --cached \
                 {LOCAL_MD} или zam init --url-to home"
            )));
        }
        Some(false) => {}
        None => {
            let in_git = locate::ancestors(root).any(|d| d.join(".git").exists());
            return Ok(if in_git {
                Exclude::Unknown
            } else {
                Exclude::Done
            });
        }
    }
    match git_ignored(root, LOCAL_MD) {
        Some(true) => return Ok(Exclude::Done),
        Some(false) => {}
        None => return Ok(Exclude::Unknown),
    }
    let path = git(root, &["rev-parse", "--git-path", "info/exclude"]);
    let prefix = git(root, &["rev-parse", "--show-prefix"]);
    let (Some((0, path)), Some((0, prefix))) = (path, prefix) else {
        return Ok(Exclude::Unknown);
    };
    let path = root.join(path);
    let line = format!("/{}{LOCAL_MD}", merge::gitignore_escape(&prefix));
    let old = read_opt(&path)?;
    let text = merge::append_lines(old.as_deref(), &[&line]);
    Ok(if old.as_deref() == Some(text.as_str()) {
        Exclude::Ineffective { path, line }
    } else {
        Exclude::Add { path, text, line }
    })
}

/// Запись исключения, если она подтверждена, и повторная проверка git: `Some(true)` — файл
/// исключён. Пишется раньше самого `CLAUDE.local.md` (`check-ignore` не требует файла): обрыв
/// между записями не оставит файл с адресом вне исключения.
pub fn apply_exclude(root: &Path, exclude: &Exclude, ok: bool) -> Result<Option<bool>, CliError> {
    let (true, Exclude::Add { path, text, .. }) = (ok, exclude) else {
        return Ok(None);
    };
    write_project_file(path, text.as_bytes())?;
    Ok(git_ignored(root, LOCAL_MD))
}

/// Адреса сервера для проверки блока: клиента и подставленный сервером (`url` ответа
/// `/agent-setup`, `server.public_url`). Пустые не берутся: пустая подстрока есть в любом тексте.
pub fn urls_of<'a>(client: &str, setups: impl IntoIterator<Item = &'a Value>) -> Vec<String> {
    std::iter::once(client)
        .chain(setups.into_iter().filter_map(|v| v["url"].as_str()))
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .collect()
}

/// Есть ли непустой адрес из `urls` в тексте.
fn has_url(text: &str, urls: &[String]) -> Option<String> {
    urls.iter()
        .find(|u| !u.is_empty() && text.contains(u.as_str()))
        .cloned()
}

/// Режим адреса без `--url-to`: прежний блок файлов агента без строки адреса (и без адреса в
/// другом месте — свой шаблон) — адрес вне блока: `local`, если в `CLAUDE.local.md` есть строка
/// адреса, иначе `home`. Иначе (блок с адресом или блока нет) — `block`. Так переход блока на
/// новую версию не возвращает адрес в коммитящийся файл.
pub fn kept_url_to(
    olds: &[Option<String>],
    urls: &[String],
    root: &Path,
) -> Result<UrlTo, CliError> {
    let Some(block) = olds
        .iter()
        .flatten()
        .find_map(|t| merge::find_block(t).ok().flatten())
    else {
        return Ok(UrlTo::Block);
    };
    let (rest, cut) = merge::cut_url_lines(block);
    if !cut.is_empty() || has_url(&rest, urls).is_some() {
        return Ok(UrlTo::Block);
    }
    let local = read_opt(&root.join(LOCAL_MD))?;
    Ok(
        if local
            .as_deref()
            .map(merge::url_values)
            .unwrap_or_default()
            .is_empty()
        {
            UrlTo::Home
        } else {
            UrlTo::Local
        },
    )
}

/// Блок для `--url-to home|local`: без строк адреса. Адрес остался в блоке где-то ещё (свой
/// шаблон администратора) — отказ (код 1): иначе он попал бы в файл проекта.
pub fn block_without_url(file: &str, block: &str, urls: &[String]) -> Result<String, CliError> {
    let (rest, cut) = merge::cut_url_lines(block);
    let all: Vec<String> = urls.iter().chain(&cut).cloned().collect();
    if let Some(u) = has_url(&rest, &all) {
        return Err(CliError::config(format!(
            "{file}: адрес сервера {u} есть в блоке не только строкой ZeroAIMemoryUrl= (свой \
             шаблон администратора) — с --url-to home|local он попал бы в файл проекта. Уберите \
             {{url}} из остального текста шаблона (WebUI: Настройки → Шаблон инструкции) или \
             подключите с --url-to block"
        )));
    }
    Ok(rest)
}

/// План адреса `--url-to home`: другой адрес в домашнем конфиге — отказ (код 1).
pub fn plan_home(home: &Path, url: &str) -> Result<locate::HomeUrlPlan, CliError> {
    match locate::plan_home_url(home, url).map_err(CliError::config)? {
        locate::HomeUrlPlan::Conflict { path, old } => Err(CliError::config(format!(
            "{}: там уже адрес {old} — он общий для всех проектов машины. Адрес только этого \
             проекта — zam init --url-to local (CLAUDE.local.md); сменить общий — поправьте url \
             в этом файле",
            path.display()
        ))),
        p => Ok(p),
    }
}

/// Итог `zam init` по адресу сервера: собирается по ходу запуска, печатается в конце.
#[derive(Debug)]
pub struct UrlReport {
    pub mode: UrlTo,
    /// `block`: файлы агента, не исключённые из git.
    pub tracked: Vec<String>,
    /// `home`/`local`: файлы агента со своей строкой адреса вне блока.
    pub outside: Vec<String>,
    /// Переход с `local` (`--url-to home|block`): строки адреса убраны из `CLAUDE.local.md`;
    /// `true` — в нём больше ничего не было, файл удалён.
    pub local_dropped: Option<bool>,
    /// Переход с `local`: строки адреса в `CLAUDE.local.md` оставлены — почему.
    pub local_kept: Option<String>,
    pub exclude: Exclude,
    /// Запись исключения подтверждена.
    pub exclude_ok: bool,
    /// Проверка git после записи исключения: `Some(true)` — `CLAUDE.local.md` исключён.
    pub excluded: Option<bool>,
    /// Подключается Codex (`--agent codex|both`).
    pub codex: bool,
}

impl UrlReport {
    pub fn new(mode: UrlTo, codex: bool) -> UrlReport {
        UrlReport {
            mode,
            tracked: Vec::new(),
            outside: Vec::new(),
            local_dropped: None,
            local_kept: None,
            exclude: Exclude::Done,
            exclude_ok: false,
            excluded: None,
            codex,
        }
    }

    /// Итог: где адрес, исключение `CLAUDE.local.md`, предупреждения (stderr).
    pub fn print(&self, root: &Path) {
        let flag = match self.mode {
            UrlTo::Block => "block",
            UrlTo::Home => "home",
            UrlTo::Local => "local",
        };
        match self.mode {
            UrlTo::Block => {
                for f in &self.tracked {
                    eprintln!(
                        "предупреждение: {f} не исключён из git — адрес сервера окажется в \
                         репозитории: zam init --url-to home"
                    );
                }
            }
            UrlTo::Home => println!(
                "Адрес сервера — в ~/.zeroaimemory/config.toml, в блоке его нет (--url-to home)."
            ),
            UrlTo::Local => {
                println!("Адрес сервера — в {LOCAL_MD}, в блоке его нет (--url-to local).");
                self.print_exclude(root);
                if self.codex {
                    // `locate::resolve` от агента не зависит: хуки Codex и `zam mcp` читают
                    // CLAUDE.local.md; не видит адреса только сам агент (финальное ревью M6.6, M2)
                    eprintln!(
                        "предупреждение: {LOCAL_MD} агент Codex не читает и адреса сервера в \
                         контексте не увидит (как и при --url-to home); zam, хуки и zam mcp адрес \
                         находят — поиск адреса от агента не зависит"
                    );
                }
            }
        }
        match self.local_dropped {
            Some(true) => println!(
                "{LOCAL_MD} удалён: кроме строки адреса в нём ничего не было (--url-to {flag})."
            ),
            Some(false) => println!(
                "Строка ZeroAIMemoryUrl убрана из {LOCAL_MD} (--url-to {flag}): она важнее, и \
                 по ней повтор zam init выбрал бы local."
            ),
            None => {}
        }
        if let Some(why) = &self.local_kept {
            eprintln!("предупреждение: строка ZeroAIMemoryUrl в {LOCAL_MD} оставлена: {why}");
        }
        for f in &self.outside {
            eprintln!(
                "предупреждение: в {f} своя строка ZeroAIMemoryUrl вне блока ZeroAIMemory — \
                 адрес сервера уйдёт в репозиторий вместе с файлом; уберите её (адрес — в \
                 {})",
                if self.mode == UrlTo::Local {
                    LOCAL_MD
                } else {
                    "~/.zeroaimemory/config.toml"
                }
            );
        }
    }

    fn print_exclude(&self, root: &Path) {
        match self.exclude_message(root) {
            Some((false, m)) => println!("{m}"),
            Some((true, m)) => eprintln!("предупреждение: {m}"),
            None => {}
        }
    }

    /// Строка итога об исключении `CLAUDE.local.md`: `(предупреждение?, текст)`.
    fn exclude_message(&self, root: &Path) -> Option<(bool, String)> {
        let leak = "адрес сервера может попасть в репозиторий";
        Some(match &self.exclude {
            Exclude::Done => return None,
            Exclude::Add { path, line, .. } if self.exclude_ok => {
                let path = rel(root, path);
                match self.excluded {
                    Some(true) => (false, format!("{LOCAL_MD} внесён в {path}.")),
                    Some(false) => (
                        true,
                        format!(
                            "строка {line} дописана в {path}, но git по-прежнему не исключает \
                             {LOCAL_MD} (правило «!» в .gitignore?) — {leak}"
                        ),
                    ),
                    None => (
                        true,
                        format!(
                            "строка {line} дописана в {path}, но не удалось проверить \
                             исключение (git check-ignore не ответил) — проверьте, что \
                             {LOCAL_MD} исключён из git"
                        ),
                    ),
                }
            }
            Exclude::Add { path, line, .. } => (
                true,
                format!(
                    "{LOCAL_MD} не внесён в {}: {leak} — повторите с --yes или допишите туда \
                     строку {line}",
                    rel(root, path)
                ),
            ),
            Exclude::Ineffective { path, line } => (
                true,
                format!(
                    "строка {line} уже есть в {}, но git не исключает {LOCAL_MD} (правило «!» в \
                     .gitignore?) — {leak}",
                    rel(root, path)
                ),
            ),
            Exclude::Unknown => (
                true,
                format!(
                    "git не ответил — проверьте, что {LOCAL_MD} исключён из git (в нём адрес \
                     сервера)"
                ),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Итог исключения после записи (ревью задачи 10, круг 2, N3): повторный `check-ignore` не
    /// ответил — «не удалось проверить», а не догадка о правиле «!»; исключён — «внесён».
    #[test]
    fn exclude_message_after_write() {
        let root = Path::new("/p");
        let mut r = UrlReport::new(UrlTo::Local, false);
        r.exclude = Exclude::Add {
            path: root.join(".git/info/exclude"),
            text: String::new(),
            line: "/CLAUDE.local.md".into(),
        };
        r.exclude_ok = true;
        let msg = |r: &UrlReport| r.exclude_message(root).unwrap();
        let (warn, m) = msg(&r);
        assert!(warn, "{m}");
        assert!(
            m.contains("не удалось проверить исключение (git check-ignore не ответил)")
                && !m.contains("«!»"),
            "{m}"
        );
        r.excluded = Some(false);
        assert!(msg(&r).1.contains("правило «!» в .gitignore?"));
        r.excluded = Some(true);
        assert_eq!(
            msg(&r),
            (
                false,
                "CLAUDE.local.md внесён в .git/info/exclude.".to_string()
            )
        );
        r.exclude_ok = false;
        assert!(msg(&r).1.contains("не внесён"));
    }

    /// Пустой `url` ответа `/agent-setup` (или `/`) — не адрес: иначе `contains("")` всегда
    /// истинно, и режим без флага навсегда остался бы `block` (ревью задачи 10, M2).
    #[test]
    fn empty_server_url_ignored() {
        let v = [
            serde_json::json!({"url": ""}),
            serde_json::json!({"url": "/"}),
            serde_json::json!({"url": "http://192.0.2.9:7700/"}),
            serde_json::json!({}),
        ];
        assert_eq!(
            urls_of("http://192.0.2.1:7700", &v),
            ["http://192.0.2.1:7700", "http://192.0.2.9:7700"]
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-init-url-empty");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let block =
            "<!-- zeroaimemory:begin v8 -->\n## Память\nРешение: x.\n<!-- zeroaimemory:end -->\n";
        let urls = ["http://192.0.2.1:7700".to_string(), String::new()];
        assert_eq!(
            kept_url_to(&[Some(block.to_string())], &urls, &root).unwrap(),
            UrlTo::Home
        );
        assert_eq!(block_without_url("CLAUDE.md", block, &urls).unwrap(), block);
    }
}
