//! Снимок сессии (спецификация M6 §8а, задача 17a): адаптеры журналов агента, чтение хвоста
//! (`take`), маскировка и пределы (`mask_and_limit`). Модуль клиента подключается исходником —
//! у `zam` нет библиотечной цели, а модуль снимка зависит только от внешних крейтов.
//!
//! Фикстуры — `tests/fixtures/transcripts/<агент>-<версия>-<случай>.jsonl`:
//! - `claude-2.1.314-*` — структура записей живого журнала Claude Code 2.1.314 (обезличена:
//!   все строки — заглушки, id фиктивные; задача 17a, шаг 0, ответ владельца А);
//! - `claude-doc-*`, `codex-doc-*` — по документации (журнала Codex на машине владельца нет).
//!
//! Заглушки `@ROOT@`, `@OUTSIDE@`, `@TRANSCRIPT@` подставляет [`fixture`]. Приманки-секреты
//! собираются конкатенацией — целого ключа в одной строке исходника нет (push protection).
mod common;

#[allow(dead_code)]
#[path = "../src/cmd/hook/snapshot/mod.rs"]
mod snapshot;

use std::path::{Path, PathBuf};
use std::time::Duration;

use snapshot::{AgentKind, Snapshot, TakeOpts};
use zam_core::snapshot::{SnapCommand, SnapStatus, SnapTrigger, SnapshotData};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const MAX: usize = 16 * 1024;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-snapshot", name)
}

fn json_str(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap();
    q[1..q.len() - 1].to_string()
}

/// Текст фикстуры с подставленными корнем решения и каталогом вне корня.
fn fixture(name: &str, root: &Path, outside: &Path) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/transcripts")
        .join(name);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("@ROOT@", &json_str(&root.to_string_lossy()))
        .replace("@OUTSIDE@", &json_str(&outside.to_string_lossy()))
        .replace("@TRANSCRIPT@", "transcript.jsonl")
}

struct Case {
    root: PathBuf,
    outside: PathBuf,
    dir: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let dir = tmp(name);
        let root = dir.join("proj");
        let outside = dir.join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Case { root, outside, dir }
    }

    /// Журнал из фикстур (по порядку) и добавочных строк; путь файла.
    fn journal(&self, fixtures: &[&str], extra: &[String]) -> PathBuf {
        let mut text = String::new();
        for f in fixtures {
            text.push_str(&fixture(f, &self.root, &self.outside));
        }
        for l in extra {
            text.push_str(l);
            text.push('\n');
        }
        let p = self.dir.join("journal.jsonl");
        std::fs::write(&p, text).unwrap();
        p
    }

    fn opts(&self, agent: AgentKind, session: &str) -> TakeOpts {
        TakeOpts {
            root: self.root.clone(),
            agent,
            session: session.to_string(),
            trigger: SnapTrigger::Manual,
            tail_kb: 2048,
            deadline: Duration::from_secs(10),
            max_bytes: MAX,
        }
    }

    fn take(&self, agent: AgentKind, session: &str, p: &Path) -> Snapshot {
        snapshot::take(p, &self.opts(agent, session)).expect("снимок")
    }
}

/// Запись человека Claude (структура живой записи) с заданным текстом.
fn claude_human(c: &Case, text: &str) -> String {
    let line = fixture("claude-2.1.314-prompts.jsonl", &c.root, &c.outside)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["message"]["content"] = serde_json::Value::String(text.to_string());
    v.to_string()
}

/// Вызов Bash (структура живой записи) с заданной командой.
fn claude_bash(c: &Case, cmd: &str, id: &str) -> String {
    let line = fixture("claude-2.1.314-tools.jsonl", &c.root, &c.outside)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["message"]["content"][0]["input"]["command"] = serde_json::Value::String(cmd.to_string());
    v["message"]["content"][0]["id"] = serde_json::Value::String(id.to_string());
    v.to_string()
}

// ---------------------------------------------------------------- Claude

/// Запись `type: "summary"` — заголовок сессии для `/resume`, а не конспект.
#[test]
fn claude_summary_record_is_not_compact() {
    let c = Case::new("summary");
    let p = c.journal(&["claude-doc-summary.jsonl"], &[]);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.compact_summary, None);
    assert_eq!(s.data.prompts, ["Запрос без сжатия."]);
}

/// Конспект — `isCompactSummary` после `compact_boundary`; без границы не берётся.
/// Обёртка Claude (первая строка и хвостовые указания) снимается, разделы остаются.
#[test]
fn claude_compact_after_boundary() {
    let c = Case::new("compact");
    let p = c.journal(&["claude-2.1.314-compact.jsonl"], &[]);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    let sum = s.data.compact_summary.expect("конспект");
    assert!(sum.contains("8. Current Work:"), "{sum}");
    assert!(sum.ends_with("Запустить тесты модуля A."), "{sum}");
    assert!(!sum.contains("This session is being continued"), "{sum}");
    assert!(!sum.contains("If you need specific details"), "{sum}");
    assert!(!sum.contains("transcript.jsonl"), "{sum}");
    assert!(!sum.contains("Continue the conversation"), "{sum}");
    assert_eq!(
        s.data.prompts,
        ["Первый запрос до сжатия.", "Запрос после сжатия."]
    );
    assert_eq!(s.session_hint.as_deref(), Some(SESSION));

    // та же запись без границы перед ней — не конспект
    let text = fixture("claude-2.1.314-compact.jsonl", &c.root, &c.outside);
    let no_boundary: Vec<&str> = text
        .lines()
        .filter(|l| !l.contains("\"compact_boundary\""))
        .collect();
    let p = c.journal(
        &[],
        &no_boundary
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.compact_summary, None);
    // и конспект не стал запросом человека
    assert_eq!(s.data.prompts.len(), 2, "{:?}", s.data.prompts);
}

/// Белый список запросов: вставки, мета-записи, уведомления задач, субагенты, картинки,
/// чужая сессия — не запросы человека.
#[test]
fn claude_whitelist_drops_inserts() {
    let c = Case::new("whitelist");
    // уведомление фоновой задачи без тега — отсекает `origin.kind`, а не вырезка вставок
    let note = fixture("claude-2.1.314-prompts.jsonl", &c.root, &c.outside)
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let mut v: serde_json::Value = serde_json::from_str(&note).unwrap();
    assert_eq!(v["origin"]["kind"], "task-notification");
    v["message"]["content"] = "Фоновая задача завершена.".into();
    let p = c.journal(
        &["claude-2.1.314-prompts.jsonl", "claude-doc-inserts.jsonl"],
        &[v.to_string()],
    );
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(
        s.data.prompts,
        [
            "Обычный запрос человека.",
            "Второй запрос  человека.",
            "Запрос с картинкой.",
            "Последний запрос.",
        ]
    );
    let all = s.data.prompts.join("\n");
    for bad in [
        "bash-",
        "local-command",
        "command-name",
        "system-reminder",
        "task-notification",
        "Request interrupted",
        "субагент",
        "старой сессии",
        "ZeroAIMemory",
        "Напоминание",
        "Ответ агента",
        "Фоновая задача",
    ] {
        assert!(!all.contains(bad), "{bad}: {all}");
    }
    assert!(s.data.commands.is_empty() && s.data.files.is_empty());
}

/// Без известной сессии берётся последняя сессия файла: после `/resume` старые записи идут раньше.
#[test]
fn claude_unknown_session_takes_last() {
    let c = Case::new("lastsession");
    let p = c.journal(&["claude-doc-inserts.jsonl"], &[]);
    let s = c.take(AgentKind::Claude, "", &p);
    assert_eq!(s.session_hint.as_deref(), Some(SESSION));
    // запись чужой (старой) сессии идёт четвёртой — всё до неё отброшено сменой сессии
    assert_eq!(s.data.prompts, ["Последний запрос."]);
}

/// Команды Bash (первая строка, исход по `is_error`) и файлы `Edit`/`Write` внутри корня.
#[test]
fn claude_commands_and_files() {
    let c = Case::new("tools");
    let p = c.journal(&["claude-2.1.314-tools.jsonl"], &[]);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(
        s.data.commands,
        [
            SnapCommand {
                cmd: "cargo test -p demo".into(),
                failed: Some(true)
            },
            SnapCommand {
                cmd: "cargo build".into(),
                failed: Some(false)
            },
        ]
    );
    assert_eq!(s.data.files, ["src/a.rs"]);
}

// ---------------------------------------------------------------- Codex

#[test]
fn codex_compacted_and_user_message() {
    let c = Case::new("codex");
    let p = c.journal(&["codex-doc-session.jsonl"], &[]);
    let s = c.take(AgentKind::Codex, "", &p);
    assert_eq!(
        s.data.compact_summary.as_deref(),
        Some("Конспект сжатия Codex: работа над модулем A, следующий шаг — тесты.")
    );
    assert_eq!(
        s.data.prompts,
        ["Первый запрос человека.", "Второй запрос человека."]
    );
    assert_eq!(
        s.session_hint.as_deref(),
        Some("00000000-0000-4000-8000-0000000000c1")
    );
    assert_eq!(
        s.data.commands,
        [
            SnapCommand {
                cmd: "cargo test -p demo".into(),
                failed: Some(true)
            },
            SnapCommand {
                cmd: "cargo build".into(),
                failed: Some(false)
            },
        ]
    );
    assert_eq!(s.data.files, ["src/a.rs", "src/b.rs"]);
}

/// `response_item` с `role: user` (правила проекта, окружение) и `compaction` с
/// `encrypted_content` не берутся.
#[test]
fn codex_ignores_user_instructions_and_encrypted() {
    let c = Case::new("codex-ignore");
    let p = c.journal(&["codex-doc-session.jsonl"], &[]);
    let s = c.take(AgentKind::Codex, "", &p);
    let all = serde_json::to_string(&s.data).unwrap();
    for bad in [
        "AGENTS.md",
        "user_instructions",
        "environment_context",
        "gAAAAB",
        "Ответ агента",
    ] {
        assert!(!all.contains(bad), "{bad}: {all}");
    }
    // без `compacted` конспекта нет: шифрованная `compaction` его не даёт
    let text = fixture("codex-doc-session.jsonl", &c.root, &c.outside);
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.contains("\"type\":\"compacted\""))
        .map(str::to_string)
        .collect();
    let p = c.journal(&[], &lines);
    let s = c.take(AgentKind::Codex, "", &p);
    assert_eq!(s.data.compact_summary, None);
    // запрос в `<user_instructions>` внутри `user_message` тоже снимается
    let p = c.journal(
        &[],
        &[r#"{"type":"event_msg","payload":{"type":"user_message","message":"<user_instructions>правила</user_instructions>"}}"#.to_string()],
    );
    assert!(c.take(AgentKind::Codex, "", &p).data.prompts.is_empty());
}

// ---------------------------------------------------------------- разбор

#[test]
fn broken_and_unknown_lines_skipped() {
    let c = Case::new("broken");
    let p = c.journal(
        &[],
        &[
            r#"{"type":"user","message":{"content":"#.to_string(),
            "не json \"type\":\"user\"".to_string(),
            r#"{"type":"user","sessionId":"00000000-0000-4000-8000-000000000001","message":{"role":"user","content":42}}"#.to_string(),
            r#"{"type":"новый-тип","x":1}"#.to_string(),
            "\u{feff}".to_string(),
            String::new(),
            claude_human(&c, "Живой запрос."),
        ],
    );
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.prompts, ["Живой запрос."]);
    // три строки прошли предфильтр и не разобрались; неизвестный тип и пустые — не пропуск
    assert_eq!(s.skipped, 3);
    assert_eq!(s.status, SnapStatus::Partial);
    // без битых строк — complete
    let p = c.journal(&[], &[claude_human(&c, "Живой запрос.")]);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!((s.skipped, s.status), (0, SnapStatus::Complete));
}

/// Строка длиннее 1 МиБ не разбирается и считается в `skipped`; соседние строки целы.
#[test]
fn huge_line_skipped() {
    let c = Case::new("huge");
    let big = "я".repeat(600 * 1024); // 1,2 МиБ UTF-8
    let p = c.journal(
        &[],
        &[
            claude_human(&c, "До."),
            claude_human(&c, &big),
            claude_human(&c, "После."),
        ],
    );
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.prompts, ["До.", "После."]);
    assert_eq!(s.skipped, 1);
}

/// Пути вне корня решения (абсолютные и через `..`) не берутся.
#[test]
fn path_outside_root_dropped() {
    let c = Case::new("outside");
    let p = c.journal(&["claude-2.1.314-tools.jsonl"], &[]);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.files, ["src/a.rs"]);
    let all = serde_json::to_string(&s.data).unwrap();
    assert!(!all.contains("notes.md"), "{all}");
    // Codex: `../outside.rs` из патча
    let p = c.journal(&["codex-doc-session.jsonl"], &[]);
    let s = c.take(AgentKind::Codex, "", &p);
    assert!(
        !s.data.files.iter().any(|f| f.contains("outside")),
        "{:?}",
        s.data.files
    );
}

/// Строки без признаков предфильтра не разбираются: битая строка без них — не пропуск.
#[test]
fn prefilter_skips_unrelated_lines() {
    let c = Case::new("prefilter");
    let p = c.journal(
        &[],
        &[
            r#"{"type":"queue-operation","operation":"enqueue","content":"сломано"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"нет"#.to_string(),
            claude_human(&c, "Запрос."),
        ],
    );
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.prompts, ["Запрос."]);
    assert_eq!(s.skipped, 0);
    // у Codex свои признаки: строка Claude-формата у него не разбирается
    let s = c.take(AgentKind::Codex, "", &p);
    assert_eq!((s.skipped, s.data.prompts.len()), (0, 0));
    for agent in [AgentKind::Claude, AgentKind::Codex] {
        assert!(agent.prefilter().iter().all(|t| t.starts_with('"')));
    }
}

/// Хвост: читается не больше `tail_kb`, первая обрезанная строка отбрасывается.
#[test]
fn tail_window_drops_cut_line() {
    let c = Case::new("tail");
    let mut lines = vec![claude_human(&c, "Старый запрос.")];
    let pad = "п".repeat(40 * 1024);
    lines.push(claude_human(&c, &pad)); // ~80 КиБ, окно 64 КиБ режет её
    lines.push(claude_human(&c, "Новый запрос."));
    let p = c.journal(&[], &lines);
    let mut o = c.opts(AgentKind::Claude, SESSION);
    o.tail_kb = 64;
    let s = snapshot::take(&p, &o).unwrap();
    assert_eq!(s.data.prompts, ["Новый запрос."]);
    assert_eq!(s.skipped, 0);
}

/// Нет файла или это каталог — снимка нет; срок вышел — тоже нет, поток брошен.
#[test]
fn take_missing_or_dir_is_none() {
    let c = Case::new("missing");
    let o = c.opts(AgentKind::Claude, SESSION);
    assert!(snapshot::take(&c.dir.join("нет.jsonl"), &o).is_none());
    assert!(snapshot::take(&c.dir, &o).is_none());
}

// ---------------------------------------------------------------- маскировка и пределы

/// Приманки — конкатенацией: целого «ключа» в одной строке исходника нет.
fn bait() -> (String, String, String) {
    let pw = ["Sx9", "kQ2", "vLm", "7Rt"].concat();
    let token = [
        "eyJhbGciOiJIUzI1NiJ9",
        ".",
        "eyJzdWIiOiJ4In0",
        ".",
        "c2lnbmF0dXJlLXRlc3Q",
    ]
    .concat();
    let gh = [concat!("gh", "p_"), "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"].concat();
    (pw, token, gh)
}

#[test]
fn secrets_masked_in_prompts_and_flags() {
    let (pw, token, gh) = bait();
    let snap = Snapshot {
        trigger: SnapTrigger::Stop,
        agent: AgentKind::Claude,
        status: SnapStatus::Complete,
        skipped: 0,
        session_hint: None,
        scan_depth_capped: false,
        data: SnapshotData {
            compact_summary: Some(format!("Работа. Токен {gh} в конфиге.")),
            prompts: vec![
                format!("вот пароль password={pw} для базы"),
                format!("ключ {gh}"),
            ],
            files: vec!["src/a.rs".into()],
            commands: vec![
                SnapCommand {
                    cmd: format!("mysql -u root -p{pw} db"),
                    failed: None,
                },
                SnapCommand {
                    cmd: format!("psql --password {pw} -h h"),
                    failed: None,
                },
                SnapCommand {
                    cmd: format!("curl -u admin:{pw} https://h/x"),
                    failed: Some(false),
                },
                SnapCommand {
                    cmd: format!("curl -H \"Authorization: Bearer {token}\" https://h"),
                    failed: None,
                },
            ],
        },
    };
    let out = snapshot::mask_and_limit(snap, MAX);
    let all = serde_json::to_string(&out.data).unwrap();
    for secret in [&pw, &token, &gh] {
        assert!(!all.contains(secret.as_str()), "{secret}: {all}");
    }
    assert!(all.contains(zam_secrets::SECRET_MARK));
    // то же, что `mask_text` по каждому полю
    assert_eq!(
        out.data.prompts[0],
        zam_secrets::mask_text(&format!("вот пароль password={pw} для базы"))
    );
    assert!(
        out.data.commands[0].cmd.starts_with("mysql -u root -p"),
        "{all}"
    );
    assert_eq!(out.data.commands[2].failed, Some(false));
    assert!(!out.scan_depth_capped);
}

/// Секрет на второй строке многострочной команды не попадает, даже если первая строка
/// маскируется отдельно: маска — по всему полю, затем первая строка.
#[test]
fn command_first_line_after_mask() {
    let (pw, _, gh) = bait();
    let pem = ["-----BEGIN ", "PRIVATE KEY-----"].concat();
    let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7";
    let end = ["-----END ", "PRIVATE KEY-----"].concat();
    let snap = Snapshot {
        trigger: SnapTrigger::Stop,
        agent: AgentKind::Claude,
        status: SnapStatus::Complete,
        skipped: 0,
        session_hint: None,
        scan_depth_capped: false,
        data: SnapshotData {
            commands: vec![
                SnapCommand {
                    cmd: format!("cat > k <<EOF\n{pem}\n{body}\n{end}\nEOF"),
                    failed: None,
                },
                SnapCommand {
                    cmd: format!("\n\nexport DB_PASSWORD={pw}\nnext"),
                    failed: None,
                },
                // ключ на границе 200 символов: обрезка до маски оставила бы его начало
                SnapCommand {
                    cmd: format!("{} {gh}", "x".repeat(185)),
                    failed: None,
                },
            ],
            ..SnapshotData::default()
        },
    };
    let out = snapshot::mask_and_limit(snap, MAX);
    assert_eq!(out.data.commands[0].cmd, "cat > k <<EOF");
    assert_eq!(
        out.data.commands[1].cmd,
        format!("export DB_PASSWORD={}", zam_secrets::SECRET_MARK)
    );
    let cut = &out.data.commands[2].cmd;
    assert!(!cut.contains(&gh[4..10]), "{cut}");
    assert!(cut.chars().count() <= 200);
}

fn sized(n: usize, tag: &str) -> String {
    format!("{tag}:{}", "ж".repeat(n))
}

/// Пределы полей, затем порядок сокращения: команды (старые), запросы (старые), конспект с
/// начала (хвост остаётся). Итог ≤ `max_bytes` всего JSON.
#[test]
fn limit_order_commands_prompts_summary() {
    let mk = |summary: usize| Snapshot {
        trigger: SnapTrigger::Stop,
        agent: AgentKind::Codex,
        status: SnapStatus::Complete,
        skipped: 0,
        session_hint: None,
        scan_depth_capped: false,
        data: SnapshotData {
            compact_summary: (summary > 0).then(|| format!("НАЧАЛО {} КОНЕЦ", "к".repeat(summary))),
            prompts: (0..8).map(|i| sized(600, &format!("p{i}"))).collect(),
            files: (0..30).map(|i| format!("src/f{i}.rs")).collect(),
            commands: (0..14)
                .map(|i| SnapCommand {
                    cmd: sized(300, &format!("c{i}")),
                    failed: None,
                })
                .collect(),
        },
    };
    // шаг 1: поля ограничены сами
    let out = snapshot::mask_and_limit(mk(100), 64 * 1024);
    let d = &out.data;
    assert_eq!(d.prompts.len(), 5);
    assert!(d.prompts[0].starts_with("p3:") && d.prompts[4].starts_with("p7:"));
    assert!(d.prompts.iter().all(|p| p.chars().count() <= 500));
    assert_eq!(d.files.len(), 20);
    assert_eq!(d.files[0], "src/f10.rs");
    assert_eq!(d.commands.len(), 10);
    assert!(d.commands[0].cmd.starts_with("c4:"));
    assert!(d.commands.iter().all(|c| c.cmd.chars().count() <= 200));
    assert_eq!(out.status, SnapStatus::Complete);

    let size = |s: &Snapshot| serde_json::to_vec(s).unwrap().len();
    // шаг 2: сначала уходят команды — с самых старых
    let full = size(&out);
    let out2 = snapshot::mask_and_limit(mk(100), full - 300);
    assert!(size(&out2) <= full - 300);
    assert!(out2.data.commands.len() < 10 && !out2.data.commands.is_empty());
    assert!(out2.data.commands.last().unwrap().cmd.starts_with("c13:"));
    assert_eq!(out2.data.prompts.len(), 5);
    assert_eq!(out2.status, SnapStatus::Partial);
    // затем запросы — с самых старых, команд уже нет
    let out3 = snapshot::mask_and_limit(mk(100), full - 4500);
    assert!(out3.data.commands.is_empty());
    assert!(out3.data.prompts.len() < 5 && !out3.data.prompts.is_empty());
    assert!(out3.data.prompts.last().unwrap().starts_with("p7:"));
    assert!(out3.data.compact_summary.is_some());
    // затем конспект — с начала, хвост остаётся
    let out4 = snapshot::mask_and_limit(mk(6000), 4096);
    assert!(size(&out4) <= 4096, "{}", size(&out4));
    assert!(out4.data.commands.is_empty() && out4.data.prompts.is_empty());
    let sum = out4.data.compact_summary.as_deref().unwrap();
    assert!(sum.ends_with("КОНЕЦ") && !sum.contains("НАЧАЛО"), "{sum}");
    assert_eq!(out4.data.files.len(), 20);
}

/// Конспект > 8 КиБ: ключевые разделы («Pending Tasks», «Current Work», «Optional Next Step»,
/// русские варианты) берутся целиком, даже если за ними длинный прочий раздел; остальное — с
/// конца до предела.
#[test]
fn summary_keeps_tail_sections() {
    let long = |n: usize| "слово ".repeat(n);
    let text = [
        "1. Primary Request and Intent:".to_string(),
        long(1500),
        "7. **Pending Tasks:**".to_string(),
        "   - задача B".to_string(),
        "8. Current Work:".to_string(),
        "   Работа над модулем A.".to_string(),
        "9. **Optional Next Step**".to_string(),
        "   Запустить тесты.".to_string(),
        "10. Notes:".to_string(),
        long(1500),
        "ХВОСТ".to_string(),
    ]
    .join("\n");
    assert!(text.len() > 16 * 1024);
    let snap = Snapshot {
        trigger: SnapTrigger::PostCompact,
        agent: AgentKind::Claude,
        status: SnapStatus::Complete,
        skipped: 0,
        session_hint: None,
        scan_depth_capped: false,
        data: SnapshotData {
            compact_summary: Some(text),
            ..SnapshotData::default()
        },
    };
    let out = snapshot::mask_and_limit(snap.clone(), MAX);
    let sum = out.data.compact_summary.unwrap();
    assert!(sum.len() <= 8 * 1024, "{}", sum.len());
    for h in [
        "7. **Pending Tasks:**\n   - задача B",
        "8. Current Work:\n   Работа над модулем A.",
        "9. **Optional Next Step**\n   Запустить тесты.",
    ] {
        assert!(sum.contains(h), "{h}");
    }
    assert!(sum.ends_with("ХВОСТ"), "хвост прочего раздела");
    assert!(!sum.contains("1. Primary Request"));
    // русские заголовки
    let ru = format!(
        "1. **Основной запрос и намерение**\n{}\n7. **Ожидающие задачи**\n- Б\n8. **Текущая работа**\nА\n9. **Следующий шаг**\nтесты\n10. Прочее:\n{}",
        long(1500),
        long(1500)
    );
    let mut s2 = snap;
    s2.data.compact_summary = Some(ru);
    let sum = snapshot::mask_and_limit(s2, MAX)
        .data
        .compact_summary
        .unwrap();
    assert!(sum.len() <= 8 * 1024);
    for h in [
        "**Ожидающие задачи**\n- Б",
        "**Текущая работа**\nА",
        "**Следующий шаг**\nтесты",
    ] {
        assert!(sum.contains(h), "{h}");
    }
}

/// Срок: поток не успел — `None`, вызывающий не ждёт сверх срока.
#[test]
fn take_deadline_returns_in_time() {
    let c = Case::new("deadline");
    let line = claude_human(&c, &"з".repeat(1000));
    let lines: Vec<String> = std::iter::repeat_n(line, 4000).collect();
    let p = c.journal(&[], &lines);
    let mut o = c.opts(AgentKind::Claude, SESSION);
    o.tail_kb = 16 * 1024;
    o.deadline = Duration::from_millis(5);
    let t = std::time::Instant::now();
    let s = snapshot::take(&p, &o);
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "{:?}",
        t.elapsed()
    );
    // успел — тогда неполный по сроку или полный; не успел — None
    if let Some(s) = s {
        assert!(s.data.prompts.len() <= 5);
    }
}

/// Команды Claude из длинной сессии: последние 10, исход сохраняется.
#[test]
fn claude_many_commands_keep_last() {
    let c = Case::new("manycmd");
    let lines: Vec<String> = (0..15)
        .map(|i| claude_bash(&c, &format!("echo {i}"), &format!("toolu_{i}")))
        .collect();
    let p = c.journal(&[], &lines);
    let s = c.take(AgentKind::Claude, SESSION, &p);
    assert_eq!(s.data.commands.len(), 10);
    assert_eq!(s.data.commands[0].cmd, "echo 5");
    assert_eq!(s.data.commands[9].failed, None);
}
