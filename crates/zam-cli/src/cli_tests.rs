//! Модульные тесты разбора clap `zam` (M6.6 §9): флаги области, решения, доказательства, полей
//! задачи и страниц лент, справка; проверка всего дерева команд (`debug_assert`). Вынесены из
//! `main.rs` (ревью задачи 8, M3): доступ к приватным `Cli` и `Cmd` — через `use super::*`.
use super::*;
use clap::CommandFactory;

/// Разбор и обход дерева команд clap — в потоке со стеком 8 МБ, как у главного потока `zam`
/// (`build.rs`): в отладочной сборке дерево `Cmd` не помещается в стек потока теста.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    let args: Vec<String> = std::iter::once("zam")
        .chain(args.iter().copied())
        .map(String::from)
        .collect();
    big_stack(move || Cli::try_parse_from(args))
}

/// Справка флага `--<long>` команды `path` (`["log", "term"]`) и его видимые синонимы.
fn arg_help(path: &[&str], long: &str) -> (String, Vec<String>) {
    let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    let long = long.to_string();
    big_stack(move || {
        let mut c = Cli::command();
        for p in &path {
            c = c
                .find_subcommand(p)
                .unwrap_or_else(|| panic!("{p}"))
                .clone();
        }
        let a = c
            .get_arguments()
            .find(|a| a.get_long() == Some(long.as_str()))
            .unwrap_or_else(|| panic!("--{long}"));
        let help = a.get_help().map(|h| h.to_string()).unwrap_or_default();
        let aliases = a
            .get_visible_aliases()
            .unwrap_or_default()
            .into_iter()
            .map(String::from)
            .collect();
        (help, aliases)
    })
}

/// M6.6 §9 (24): область — канон `--global`, `--knowledge` — видимый синоним у всех лент и
/// записей (у идей — прежний `--knowledge` работает).
#[test]
fn scope_flag_global_with_knowledge_alias() {
    for flag in ["--global", "--knowledge"] {
        let Ok(Cli {
            cmd: Cmd::Insight {
                cmd: InsightCmd::List { global, .. },
            },
        }) = parse(&["insight", "list", flag])
        else {
            panic!("insight list {flag}");
        };
        assert!(global, "insight list {flag}");
        let Ok(Cli {
            cmd: Cmd::Idea {
                cmd: IdeaCmd::List { global, .. },
            },
        }) = parse(&["idea", "list", flag])
        else {
            panic!("idea list {flag}");
        };
        assert!(global, "idea list {flag}");
        let Ok(Cli {
            cmd: Cmd::Idea {
                cmd: IdeaCmd::New { global, .. },
            },
        }) = parse(&["idea", "new", "Т", flag])
        else {
            panic!("idea new {flag}");
        };
        assert!(global, "idea new {flag}");
        let Ok(Cli {
            cmd: Cmd::Task {
                cmd: TaskCmd::New { global, .. },
            },
        }) = parse(&["task", "new", "--title", "Т", flag])
        else {
            panic!("task new {flag}");
        };
        assert!(global, "task new {flag}");
        let Ok(Cli {
            cmd: Cmd::Vault {
                cmd: VaultCmd::List { opts, .. },
            },
        }) = parse(&["vault", "list", flag])
        else {
            panic!("vault list {flag}");
        };
        assert!(opts.global, "vault list {flag}");
        for args in [
            vec!["task", "list", flag],
            vec!["question", "list", flag],
            vec!["question", "ask", "--title", "Т", flag],
        ] {
            assert!(parse(&args).is_ok(), "{args:?}");
        }
    }
    for path in [
        &["insight", "list"][..],
        &["task", "list"],
        &["task", "new"],
        &["question", "list"],
        &["question", "ask"],
        &["idea", "list"],
        &["idea", "new"],
        &["vault", "list"],
    ] {
        let (_, aliases) = arg_help(path, "global");
        assert_eq!(aliases, ["knowledge"], "{path:?}");
    }
}

/// M6.6 §9 (14): у `zam release list` `--limit` — N новейших по дате (лента читается целиком), а
/// не страница сервера: справка команды переопределяет общую справку `cmd::Paging`; у лент
/// журналов — прежняя.
#[test]
fn release_list_limit_help_newest() {
    let (limit, _) = arg_help(&["release", "list"], "limit");
    assert!(limit.contains("Новейших релизов по дате"), "{limit}");
    let (all, _) = arg_help(&["release", "list"], "all");
    assert_eq!(all, "Все релизы решения");
    let (page, _) = arg_help(&["task", "list"], "limit");
    assert!(page.contains("Записей на странице"), "{page}");
}

/// M6.6 §9 (16д): значение `--code`, начинающееся с «-», — через «=»; справка это говорит.
#[test]
fn term_code_hyphen_value_help() {
    let (help, _) = arg_help(&["log", "term"], "code");
    assert!(help.contains("--code=--dry-run"), "{help}");
    let Ok(Cli {
        cmd: Cmd::Log {
            cmd: LogCmd::Term { code, .. },
        },
    }) = parse(&[
        "log",
        "term",
        "--title",
        "Т",
        "--definition",
        "О",
        "--code=--dry-run",
    ])
    else {
        panic!("--code=--dry-run");
    };
    assert_eq!(code, ["--dry-run"]);
    // без «=» значение с «-» — ошибка разбора, а не молча проглоченный флаг
    assert!(
        parse(&[
            "log",
            "term",
            "--title",
            "Т",
            "--definition",
            "О",
            "--code",
            "--dry-run",
        ])
        .is_err()
    );
}

/// Ревизия замещаемой записи (финальное ревью M7.1, задача 6 m2): `insight supersede --rev`
/// (`If-Match`) и `log insight --supersedes … --supersedes-rev`; `--supersedes-rev` без
/// `--supersedes` — ошибка разбора.
#[test]
fn insight_supersede_revision_flags() {
    let Ok(Cli {
        cmd: Cmd::Insight {
            cmd: InsightCmd::Supersede { rev, .. },
        },
    }) = parse(&["insight", "supersede", "01E", "--title", "Т", "--rev", "3"])
    else {
        panic!("supersede --rev");
    };
    assert_eq!(rev, Some(3));
    let Ok(Cli {
        cmd:
            Cmd::Log {
                cmd:
                    LogCmd::Insight {
                        supersedes,
                        supersedes_rev,
                        ..
                    },
            },
    }) = parse(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "Т",
        "--supersedes",
        "01E",
        "--supersedes-rev",
        "2",
    ])
    else {
        panic!("log insight --supersedes-rev");
    };
    assert_eq!(
        (supersedes.as_deref(), supersedes_rev),
        (Some("01E"), Some(2))
    );
    assert!(
        parse(&[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Т",
            "--supersedes-rev",
            "2"
        ])
        .is_err()
    );
}

/// M6.6 §9 (8): справка `--certainty` называет умолчание; флаги решения и доказательства
/// разбираются у `log insight` и `insight supersede`; доказательство — все три флага вместе,
/// не с `--evidence-file`.
#[test]
fn insight_decision_and_evidence_flags() {
    for path in [&["log", "insight"][..], &["insight", "supersede"]] {
        let (help, _) = arg_help(path, "certainty");
        assert!(help.contains("без флага — hypothesis"), "{path:?}: {help}");
    }
    let Ok(Cli {
        cmd: Cmd::Insight {
            cmd: InsightCmd::Supersede { new, .. },
        },
    }) = parse(&[
        "insight",
        "supersede",
        "01E",
        "--title",
        "Т",
        "--option",
        "a",
        "--option",
        "b",
        "--chosen",
        "c",
        "--decided-by",
        "agent",
    ])
    else {
        panic!("supersede с флагами решения");
    };
    let f = new.fields(None);
    assert_eq!(f.decision.options, ["a", "b"]);
    assert_eq!(
        (f.decision.chosen, f.decision.decided_by),
        (Some("c"), Some("agent"))
    );
    let base = ["log", "insight", "--kind", "decision", "--title", "Т"];
    let ev = [
        "--evidence-checked",
        "c",
        "--evidence-method",
        "m",
        "--evidence-result",
        "r",
    ];
    let Ok(Cli {
        cmd: Cmd::Log {
            cmd: LogCmd::Insight { new, .. },
        },
    }) = parse(&[&base[..], &ev].concat())
    else {
        panic!("три флага доказательства");
    };
    let e = new
        .fields(Some("decision"))
        .evidence
        .expect("доказательство");
    assert_eq!((e.checked, e.method, e.result), ("c", "m", "r"));
    let with_file: Vec<&str> = ev
        .iter()
        .copied()
        .chain(["--evidence-file", "e.json"])
        .collect();
    for extra in [
        &ev[..4],
        &ev[2..],
        &["--decided-by", "robot"][..],
        &with_file[..],
    ] {
        assert!(parse(&[&base[..], extra].concat()).is_err(), "{extra:?}");
    }
}

/// M6.6 §9 (9, 16г): поля `task new`; `--limit` и `--all` у четырёх лент, вместе — ошибка.
#[test]
fn task_fields_and_list_paging() {
    let Ok(Cli {
        cmd:
            Cmd::Task {
                cmd:
                    TaskCmd::New {
                        owner,
                        estimate,
                        links,
                        priority,
                        ..
                    },
            },
    }) = parse(&[
        "task",
        "new",
        "--title",
        "Т",
        "--owner",
        "agent",
        "--estimate",
        "30",
        "--link",
        "a",
        "--link",
        "b",
        "--priority",
        "low",
    ])
    else {
        panic!("task new с полями");
    };
    assert_eq!(
        (owner.as_deref(), estimate, priority.as_deref()),
        (Some("agent"), Some(30), Some("low"))
    );
    assert_eq!(links, ["a", "b"]);
    for kind in ["insight", "task", "question", "idea"] {
        assert!(parse(&[kind, "list", "--all"]).is_ok(), "{kind}");
        assert!(parse(&[kind, "list", "--limit", "7"]).is_ok(), "{kind}");
        assert!(
            parse(&[kind, "list", "--all", "--limit", "7"]).is_err(),
            "{kind}"
        );
        assert!(parse(&[kind, "list", "--limit", "201"]).is_err(), "{kind}");
    }
}

/// M6.6, замечание 35: `zam task status <id> done --next ""` — способ снять шаг задачи, закрытой
/// раньше; clap принимает пустое значение (`Some("")`), тело — `cmd::task::status_body`.
#[test]
fn task_status_empty_next_accepted() {
    let Ok(Cli {
        cmd: Cmd::Task {
            cmd: TaskCmd::Status { status, next, .. },
        },
    }) = parse(&["task", "status", "k:1", "done", "--next", ""])
    else {
        panic!("task status done --next \"\"");
    };
    assert_eq!((status.as_str(), next.as_deref()), ("done", Some("")));
}

/// M7.5 §8.9 п. 4: `zam sessions snapshot --tail-kb N` — в диапазоне ключа `hooks.snapshot_tail_kb`
/// (64..=16384), вне — ошибка разбора; без флага — `None` (ключ клиента); `zam sessions show <id>`.
#[test]
fn snapshot_tail_kb_flag_range() {
    let snap = |extra: &[&str]| {
        let mut args = vec!["sessions", "snapshot", "--transcript", "t.jsonl"];
        args.extend_from_slice(extra);
        parse(&args)
    };
    for (n, want) in [
        ("64", Some(64)),
        ("16384", Some(16384)),
        ("2048", Some(2048)),
    ] {
        let Ok(Cli {
            cmd:
                Cmd::Sessions {
                    cmd: SessionsCmd::Snapshot { tail_kb, .. },
                },
        }) = snap(&["--tail-kb", n])
        else {
            panic!("--tail-kb {n}");
        };
        assert_eq!(tail_kb, want);
    }
    for bad in ["63", "16385", "0", "x"] {
        assert!(snap(&["--tail-kb", bad]).is_err(), "--tail-kb {bad}");
    }
    let Ok(Cli {
        cmd: Cmd::Sessions {
            cmd: SessionsCmd::Snapshot { tail_kb: None, .. },
        },
    }) = snap(&[])
    else {
        panic!("без --tail-kb");
    };
    let Ok(Cli {
        cmd: Cmd::Sessions {
            cmd: SessionsCmd::Show { id, .. },
        },
    }) = parse(&["session", "show", "S-1", "--json"])
    else {
        panic!("session show");
    };
    assert_eq!(id, "S-1");
    // `--reject-secrets` у `write` и `import` (M7.5 §8.9 п. 5)
    let Ok(Cli {
        cmd: Cmd::Write { reject_secrets, .. },
    }) = parse(&["write", "a/b", "--reject-secrets"])
    else {
        panic!("write --reject-secrets");
    };
    assert!(reject_secrets);
    let Ok(Cli {
        cmd: Cmd::Import { reject_secrets, .. },
    }) = parse(&["import", "dir", "--reject-secrets"])
    else {
        panic!("import --reject-secrets");
    };
    assert!(reject_secrets);
}

/// M7.6 §9.5: `zam init --block-to agent|local` (без флага — `None`, режим прежнего подключения),
/// `--exclude-local`; `zam idea reopen <w:id> --reason` (причина обязательна) и `zam idea adopt`
/// с `--by`/`--channel` по образцу `accept`.
#[test]
fn init_block_to_and_idea_reopen_adopt_flags() {
    let Ok(Cli {
        cmd: Cmd::Init {
            block_to,
            exclude_local,
            ..
        },
    }) = parse(&["init", "--block-to", "local", "--exclude-local"])
    else {
        panic!("init --block-to local --exclude-local");
    };
    assert_eq!(block_to, Some(cmd::init::BlockTo::Local));
    assert!(exclude_local);
    let Ok(Cli {
        cmd:
            Cmd::Init {
                block_to: None,
                exclude_local: false,
                ..
            },
    }) = parse(&["init"])
    else {
        panic!("init без флагов");
    };
    assert!(parse(&["init", "--block-to", "home"]).is_err());
    let Ok(Cli {
        cmd:
            Cmd::Idea {
                cmd: IdeaCmd::Reopen {
                    reason, channel, ..
                },
            },
    }) = parse(&[
        "idea",
        "reopen",
        "w:x",
        "--reason",
        "снова",
        "--by",
        "human",
    ])
    else {
        panic!("idea reopen");
    };
    assert_eq!((reason.as_str(), channel.as_str()), ("снова", "cli"));
    assert!(
        parse(&["idea", "reopen", "w:x"]).is_err(),
        "--reason обязателен"
    );
    let Ok(Cli {
        cmd: Cmd::Idea {
            cmd: IdeaCmd::Adopt { by, channel, .. },
        },
    }) = parse(&["idea", "adopt", "w:x", "--by", "human", "--channel", "chat"])
    else {
        panic!("idea adopt");
    };
    assert_eq!((by.as_deref(), channel.as_str()), (Some("human"), "chat"));
    assert!(parse(&["idea", "adopt", "w:x", "--channel", "webui"]).is_err());
}

/// Ревью задачи 8, M4: отладочные проверки clap по всему дереву команд (имена и синонимы без
/// повторов, `conflicts_with`, `requires_all`, группы ссылаются на существующие аргументы) — а не
/// только у подкоманд, которые разбирают другие тесты.
#[test]
fn cli_debug_assert() {
    big_stack(|| Cli::command().debug_assert());
}
