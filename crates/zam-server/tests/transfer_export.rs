//! Экспорт пространства (M6 §12б.5, задача 18b1): раскладка архива и опись, имена файлов для
//! Windows, что не экспортируется (Vault, карта кода, корзина, сессии, чужие данные), права
//! (токен без Human и отчётов, участник решения, сессия WebUI), аудит, фильтры.
//!
//! Мир — `common::reports`: владелец решения `G` (Анна), участники `rw` (Борис) и `ro` (Вера) —
//! у каждого токен и сессия WebUI.
mod common;

use std::collections::{BTreeMap, HashSet};
use std::io::Read;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp};
use common::reports::{G, User, W, ok, png, web, world, world_cfg};
use serde_json::{Value, json};
use zam_server::api::transfer::{ExportManifest, Fingerprint};

/// Распакованный архив: файлы в порядке записи.
struct Zip {
    names: Vec<String>,
    files: BTreeMap<String, Vec<u8>>,
}

impl Zip {
    fn of(r: &Resp) -> Zip {
        ok(r, StatusCode::OK, "экспорт");
        assert_eq!(r.header("content-type"), "application/zip");
        assert!(
            r.header("content-disposition").starts_with("attachment;"),
            "{}",
            r.header("content-disposition")
        );
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(r.body.to_vec())).unwrap();
        let mut names = Vec::new();
        let mut files = BTreeMap::new();
        for i in 0..z.len() {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            let name = f.name().unwrap().to_string();
            names.push(name.clone());
            assert!(
                files.insert(name.clone(), b).is_none(),
                "повтор имени {name}"
            );
        }
        Zip { names, files }
    }

    fn text(&self, name: &str) -> String {
        let b = self
            .files
            .get(name)
            .unwrap_or_else(|| panic!("нет {name} среди {:?}", self.names));
        String::from_utf8(b.clone()).unwrap()
    }

    fn manifest(&self) -> ExportManifest {
        serde_json::from_slice(&self.files["manifest.json"]).unwrap()
    }

    /// Frontmatter файла `.md`: поле → значение (JSON) и тело после блока.
    fn front(&self, name: &str) -> (BTreeMap<String, Value>, String) {
        let t = self.text(name);
        let rest = t.strip_prefix("---\n").expect("frontmatter");
        let (block, body) = rest.split_once("\n---\n").expect("конец frontmatter");
        let mut m = BTreeMap::new();
        for line in block.lines() {
            let (k, v) = line.split_once(": ").unwrap();
            m.insert(k.to_string(), serde_json::from_str(v).unwrap());
        }
        (m, body.to_string())
    }

    fn with_prefix(&self, p: &str) -> Vec<&String> {
        self.names.iter().filter(|n| n.starts_with(p)).collect()
    }

    /// Какой-нибудь файл архива содержит `needle`.
    fn contains(&self, needle: &str) -> Vec<&String> {
        self.files
            .iter()
            .filter(|(_, b)| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
            .map(|(n, _)| n)
            .collect()
    }
}

async fn export_tok(a: &Api, tok: &str, space: &str, q: &str) -> Resp {
    a.tok(
        tok,
        Method::GET,
        &format!("/api/v1/spaces/{space}/export{q}"),
        None,
    )
    .await
}

async fn export_web(a: &Api, u: &User, space: &str, q: &str) -> Resp {
    web(
        a,
        &u.web,
        Method::GET,
        &format!("/api/v1/spaces/{space}/export{q}"),
        None,
    )
    .await
}

async fn note_as(a: &Api, tok: &str, space: &str, path: &str, content: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await;
    ok(&r, StatusCode::CREATED, path);
    r.json()["id"].as_str().unwrap().to_string()
}

async fn entry_as(a: &Api, tok: &str, space: &str, body: Value) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    ok(&r, StatusCode::CREATED, "запись");
    r.json()["id"].as_str().unwrap().to_string()
}

async fn link(a: &Api, tok: &str, src: &str, dst: &str) {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": src, "type": "relates_to", "dst": dst})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "связь");
}

/// Отпечаток описи совпадает с пересчитанным по файлам архива без описи; опись — последней.
fn check_fingerprint(z: &Zip) {
    assert_eq!(z.names.last().map(String::as_str), Some("manifest.json"));
    let m = z.manifest();
    let mut fp = Fingerprint::default();
    for n in z.names.iter().filter(|n| *n != "manifest.json") {
        fp.file(n, &z.files[n]);
    }
    assert_eq!(m.archive_fingerprint, fp.hex());
    assert_eq!(m.files, z.names.len() - 1);
}

/// Раскладка: заметки по путям с frontmatter, вложение двух заметок — один файл по пути из тела,
/// записи журналов, связи, история; опись с отпечатком.
#[tokio::test]
async fn export_layout_and_manifest() {
    let w = world("export-layout").await;
    let (a, o) = (&w.a, &w.o);
    let n1 = note_as(a, &o.tok, G, "docs/план", "# План\n\nтекст плана").await;
    let n2 = note_as(a, &o.tok, G, "итоги", "# Итоги\n\nитог").await;
    let pic = png(7, 300);
    let up1 = common::attach::upload(a, &o.tok, &n1, "схема 1.png", &pic).await;
    let up2 = common::attach::upload(a, &o.tok, &n2, "схема 1.png", &pic).await;
    let link_md = up1["link"].as_str().unwrap().to_string();
    assert_eq!(up2["link"], up1["link"]);
    // вторая ревизия с ссылкой на вложение в теле
    let r = a
        .tok_h(
            &o.tok,
            Method::PUT,
            &format!("/api/v1/nodes/{n1}"),
            Some(json!({"content": format!("# План\n\n![схема]({link_md})\n")})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK, "правка");
    let ins = entry_as(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Кэш дешевле", "body": "потому что",
               "tags": ["perf"], "data": {"kind": "pattern"}}),
    )
    .await;
    link(a, &o.tok, &format!("n:{n1}"), &format!("e:{ins}")).await;

    let z = Zip::of(&export_tok(a, &o.tok, G, "").await);
    check_fingerprint(&z);
    let m = z.manifest();
    assert_eq!(m.format, 1);
    assert_eq!(m.source_user_id.to_string(), o.id);
    assert_eq!(m.space_kind, "solution");
    assert_eq!(m.solution_guid.as_deref(), Some(G));
    assert!(m.filter.history);
    assert!(m.missing_blobs.is_empty());

    let (f, body) = z.front("docs/план.md");
    assert_eq!(f["id"], json!(n1));
    assert_eq!(f["path"], json!("docs/план"));
    assert_eq!(f["rev"], json!(2));
    assert!(body.contains(&link_md), "{body}");
    // ссылка тела открывается в распакованном архиве: имя без кодирования ссылки Markdown
    let att = z.with_prefix("_attachments/");
    assert_eq!(
        att.len(),
        1,
        "одно вложение двух заметок — один файл: {att:?}"
    );
    let sha16 = &f["attachments"][0]["sha256"].as_str().unwrap()[..16];
    assert_eq!(att[0], &format!("_attachments/{sha16}/схема 1.png"));
    assert_eq!(link_md, format!("_attachments/{sha16}/схема%201.png"));
    assert_eq!(z.files[att[0].as_str()], pic);
    assert_eq!(f["attachments"][0]["file"], json!(att[0]));
    let (f2, _) = z.front("итоги.md");
    assert_eq!(f2["attachments"][0]["file"], json!(att[0]));

    let (e, ebody) = z.front(&format!("_journals/insight/{ins}.md"));
    assert_eq!(e["title"], json!("Кэш дешевле"));
    assert_eq!(e["status"], json!("active"));
    assert_eq!(e["tags"], json!(["perf"]));
    assert_eq!(e["data"]["kind"], json!("pattern"));
    assert_eq!(ebody, "потому что");

    let links = z.text("_links.jsonl");
    let l: Value = serde_json::from_str(links.lines().next().unwrap()).unwrap();
    assert_eq!(l["src"], json!(format!("n:{n1}")));
    assert_eq!(l["dst"], json!(format!("e:{ins}")));
    assert_eq!(l["type"], json!("relates_to"));

    let h = z.text(&format!("_history/{n1}.jsonl"));
    let revs: Vec<Value> = h
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0]["rev"], json!(1));
    assert_eq!(revs[0]["content"], json!("# План\n\nтекст плана"));
    assert!(z.files.contains_key(&format!("_history/{ins}.jsonl")));
}

/// Имена файлов для Windows: запрещённые символы, имена устройств, конечная точка, пара по
/// регистру; исходный путь — во frontmatter. Все имена архива допустимы в Windows и различны
/// без учёта регистра.
#[tokio::test]
async fn export_windows_names() {
    let w = world("export-win").await;
    let (a, o) = (&w.a, &w.o);
    for p in [
        "a:b",
        "CON",
        "x.",
        "Readme",
        "README",
        "_attachments/x",
        "d/aux.txt",
    ] {
        note_as(a, &o.tok, G, p, &format!("текст {p}")).await;
    }
    let z = Zip::of(&export_tok(a, &o.tok, G, "").await);
    let expect = [
        ("a%3Ab.md", "a:b"),
        ("CON_.md", "CON"),
        ("x%2E.md", "x."),
        ("README.md", "README"),
        ("Readme~2.md", "Readme"),
        ("%5Fattachments/x.md", "_attachments/x"),
        ("d/aux_.txt.md", "d/aux.txt"),
    ];
    for (file, path) in expect {
        let (f, body) = z.front(file);
        assert_eq!(f["path"], json!(path), "{file}");
        assert_eq!(body, format!("текст {path}"));
    }
    assert!(z.with_prefix("_attachments/").is_empty());
    let mut seen = HashSet::new();
    for n in &z.names {
        assert!(
            seen.insert(n.to_lowercase()),
            "совпадение без учёта регистра: {n}"
        );
        for seg in n.split('/') {
            assert!(
                !seg.contains([':', '?', '*', '|', '<', '>', '"', '\\']),
                "{n}"
            );
            assert!(!seg.ends_with(['.', ' ']), "{n}");
            let base = seg.split('.').next().unwrap().to_ascii_uppercase();
            let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&base.as_str())
                || ((base.starts_with("COM") || base.starts_with("LPT"))
                    && base.len() == 4
                    && base.as_bytes()[3].is_ascii_digit()
                    && base.as_bytes()[3] != b'0');
            assert!(!reserved, "{n}");
        }
    }
}

/// Не экспортируются Vault (значение и название), карта кода, корзина, сессии и выданное им;
/// другие пространства и другие пользователи — тоже.
#[tokio::test]
async fn export_excludes_vault_codemap_trash_sessions() {
    let w = world("export-excl").await;
    let (a, o) = (&w.a, &w.o);
    note_as(a, &o.tok, G, "живая", "живой текст").await;
    // Vault решения
    let r = a
        .tok(
            &o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/vault"),
            Some(json!({"title": "VAULT-TITLE-MARK",
                        "data": {"type": "token", "description": "VAULT-DESC-MARK"},
                        "secret": "VAULT-SECRET-VALUE-9f3"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "vault");
    // карта кода решения
    let r = a
        .tok(
            &o.tok,
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "commit": "abcdef1", "branch": "main", "machine": "box",
                "files": [{"path": "src/codemapmark.rs", "lang": "rust", "hash": format!("{:064x}", 1),
                    "size": 10, "parse_state": "ok", "refs": [],
                    "symbols": [{"name": "CODEMAP_SYMBOL_MARK", "qual": "CODEMAP_SYMBOL_MARK",
                        "kind": "function", "line": 1, "end_line": 2, "sig": "fn x()",
                        "body_hash": format!("{:016x}", 1), "exported": true}]}]})),
        )
        .await;
    ok(&r, StatusCode::OK, "карта кода");
    // корзина
    let gone = note_as(a, &o.tok, G, "удалённая", "TRASHED-TEXT-MARK").await;
    let r = a
        .tok(
            &o.tok,
            Method::DELETE,
            &format!("/api/v1/nodes/{gone}"),
            None,
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT, "удаление");
    let gone_e = entry_as(
        a,
        &o.tok,
        G,
        json!({"kind": "task", "title": "TRASHED-TASK-MARK", "body": "x"}),
    )
    .await;
    let r = a
        .tok_rev(
            &o.tok,
            Method::DELETE,
            &format!("/api/v1/entries/{gone_e}"),
            None,
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT, "удаление записи");
    // сессия агента с выданным
    let r = a
        .tok_h(
            &o.tok,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "живой"})),
            &[("x-zam-session", "SESSION-ID-MARK-77")],
        )
        .await;
    ok(&r, StatusCode::OK, "поиск в сессии");
    // другое пространство владельца и другой пользователь
    note_as(
        a,
        &o.tok,
        "knowledge",
        "личное",
        "KNOWLEDGE-OTHER-SPACE-MARK",
    )
    .await;
    note_as(a, &w.rw.tok, "knowledge", "чужое", "OTHER-USER-MARK").await;

    for z in [
        Zip::of(&export_tok(a, &o.tok, G, "").await),
        Zip::of(&export_web(a, o, G, "").await),
    ] {
        assert!(z.files.contains_key("живая.md"));
        for mark in [
            "VAULT-TITLE-MARK",
            "VAULT-DESC-MARK",
            "VAULT-SECRET-VALUE-9f3",
            "CODEMAP_SYMBOL_MARK",
            "codemapmark",
            "TRASHED-TEXT-MARK",
            "TRASHED-TASK-MARK",
            &gone,
            &gone_e,
            "SESSION-ID-MARK-77",
            "KNOWLEDGE-OTHER-SPACE-MARK",
            "OTHER-USER-MARK",
        ] {
            assert!(
                z.contains(mark).is_empty(),
                "{mark} в {:?}",
                z.contains(mark)
            );
        }
        check_fingerprint(&z);
    }
    // экспорт knowledge владельца не содержит решения и чужого knowledge
    let z = Zip::of(&export_tok(a, &o.tok, "knowledge", "").await);
    assert!(z.files.contains_key("личное.md"));
    for mark in ["живой текст", "OTHER-USER-MARK", "VAULT-SECRET-VALUE-9f3"] {
        assert!(z.contains(mark).is_empty(), "{mark}");
    }
}

/// Токен: Human — 404, отчёты не экспортируются, связи на Human и отчёт — тоже; сессия WebUI —
/// с Human, отчётами и файлами отчёта.
#[tokio::test]
async fn token_export_no_human_no_reports() {
    let w = world("export-tok").await;
    let (a, o) = (&w.a, &w.o);
    let rep = w.report("Итог эксперимента").await;
    let n = note_as(a, &o.tok, G, "заметка", "текст").await;
    let r = web(
        a,
        &o.web,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": "human", "path": "моё", "kind": "note", "content": "HUMAN-MARK"})),
    )
    .await;
    ok(&r, StatusCode::CREATED, "human");
    let h = r.json()["id"].as_str().unwrap().to_string();
    let kn = note_as(a, &o.tok, "knowledge", "знание", "k").await;
    // связи из решения: на Human (сессией), на knowledge (токеном); отчёт концом связи не
    // бывает (422) — фильтр отчётов в выборке связей экспорта — страховка
    for (dst, by_web) in [(format!("n:{h}"), true), (format!("n:{kn}"), false)] {
        let body = json!({"src": format!("n:{n}"), "type": "relates_to", "dst": dst});
        let r = if by_web {
            web(a, &o.web, Method::POST, "/api/v1/edges", Some(body)).await
        } else {
            a.tok(&o.tok, Method::POST, "/api/v1/edges", Some(body))
                .await
        };
        ok(&r, StatusCode::CREATED, "связь");
    }

    assert_eq!(
        export_tok(a, &o.tok, "human", "").await.status,
        StatusCode::NOT_FOUND
    );
    let z = Zip::of(&export_tok(a, &o.tok, G, "").await);
    assert!(z.with_prefix("_journals/report/").is_empty());
    assert!(z.with_prefix("_attachments/").is_empty(), "{:?}", z.names);
    assert!(z.contains(&rep).is_empty(), "{:?}", z.contains(&rep));
    assert!(z.contains(&h).is_empty());
    let links = z.text("_links.jsonl");
    assert_eq!(links.lines().count(), 1, "{links}");
    assert!(links.contains(&kn));
    // токен с kinds=report — тоже без отчётов
    let z = Zip::of(&export_tok(a, &o.tok, G, "?kinds=report").await);
    assert!(z.with_prefix("_journals/").is_empty());

    // сессия WebUI владельца: отчёт с файлами, связь на Human
    let z = Zip::of(&export_web(a, o, G, "").await);
    let (f, _) = z.front(&format!("_journals/report/{rep}.md"));
    let files = f["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    for x in files {
        let file = x["file"].as_str().unwrap();
        assert!(z.files.contains_key(file), "{file}");
    }
    // файл отчёта — `_attachments/<sha16>/<имя>`, как вложение заметки
    for x in files {
        let (sha, name) = (x["sha256"].as_str().unwrap(), x["name"].as_str().unwrap());
        assert_eq!(
            x["file"],
            json!(format!("_attachments/{}/{name}", &sha[..16]))
        );
    }
    assert_eq!(z.text("_links.jsonl").lines().count(), 2);
    check_fingerprint(&z);
    let z = Zip::of(&export_web(a, o, "human", "").await);
    assert!(z.text("моё.md").contains("HUMAN-MARK"));
}

/// Участник решения (`rw` и `ro`) экспортирует пространство решения; связи — только внутри
/// него (knowledge владельца ему не виден); токену участника — без отчётов, сессии — с ними.
#[tokio::test]
async fn member_can_export() {
    let w = world("export-member").await;
    let (a, o) = (&w.a, &w.o);
    let rep = w.report("Отчёт").await;
    let n = note_as(a, &o.tok, G, "общая", "общий текст").await;
    let kn = note_as(a, &o.tok, "knowledge", "личное", "OWNER-KNOWLEDGE-MARK").await;
    link(a, &o.tok, &format!("n:{n}"), &format!("n:{kn}")).await;
    for m in [&w.rw, &w.ro] {
        let z = Zip::of(&export_tok(a, &m.tok, G, "").await);
        assert!(z.text("общая.md").contains("общий текст"));
        assert_eq!(z.manifest().source_user_id.to_string(), o.id);
        assert!(z.contains(&kn).is_empty(), "связь на knowledge владельца");
        assert!(z.contains("OWNER-KNOWLEDGE-MARK").is_empty());
        assert!(z.with_prefix("_journals/report/").is_empty());
        let z = Zip::of(&export_web(a, m, G, "").await);
        assert!(z.files.contains_key(&format!("_journals/report/{rep}.md")));
        assert!(z.contains(&kn).is_empty());
    }
    // knowledge участника — его собственный, не владельца
    let z = Zip::of(&export_tok(a, &w.rw.tok, "knowledge", "").await);
    assert!(z.contains("OWNER-KNOWLEDGE-MARK").is_empty());
}

/// Каждый экспорт — аудит `space.export {space, user, files, bytes}` без текста.
#[tokio::test]
async fn export_audit_written() {
    let w = world("export-audit").await;
    let (a, o) = (&w.a, &w.o);
    note_as(a, &o.tok, G, "аудит", "AUDIT-TEXT-MARK").await;
    let r = export_tok(a, &o.tok, G, "").await;
    let z = Zip::of(&r);
    let space = z.manifest().space.to_string();
    // до тела — `space.export` с фильтром, по окончании — `space.export.done` с объёмом
    let s = audits(a, "space.export", 1).await.remove(0);
    assert_eq!(s["user_id"], json!(o.id));
    assert_eq!(s["target"], json!(space));
    assert_eq!(s["details"]["space"], json!(space));
    assert_eq!(s["details"]["filter"]["history"], json!(true));
    assert!(s["details"].get("files").is_none());
    let e = done_audits(a, 1).await.remove(0);
    assert!(e["id"].as_i64() > s["id"].as_i64());
    assert_eq!(e["target"], json!(space));
    assert_eq!(e["details"]["user"], json!(o.id));
    assert_eq!(e["details"]["files"], json!(z.names.len() - 1));
    let bytes: usize = z
        .names
        .iter()
        .filter(|n| *n != "manifest.json")
        .map(|n| z.files[n].len())
        .sum();
    assert_eq!(e["details"]["bytes"], json!(bytes));
    assert_eq!(e["details"]["complete"], json!(true));
    assert!(e["details"].get("reason").is_none());
    assert!(!e.to_string().contains("AUDIT-TEXT-MARK"));
    assert!(!s.to_string().contains("AUDIT-TEXT-MARK"));
    // проверка `probe=true` аудита не пишет
    let r = export_tok(a, &o.tok, G, "?probe=true").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert!(r.body.is_empty());
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(audits(a, "space.export", 1).await.len(), 1);
    assert_eq!(done_audits(a, 1).await.len(), 1);
}

async fn done_audits(a: &Api, n: usize) -> Vec<Value> {
    audits(a, "space.export.done", n).await
}

/// Записи аудита `action` (новые первыми); ждёт, пока их станет не меньше `n`
/// (`space.export.done` пишется по окончании потока).
async fn audits(a: &Api, action: &str, n: usize) -> Vec<Value> {
    for _ in 0..100 {
        let r = a.adm(Method::GET, "/api/v1/admin/audit", None, None).await;
        ok(&r, StatusCode::OK, "аудит");
        let all: Vec<Value> = r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["action"] == action)
            .cloned()
            .collect();
        if all.len() >= n {
            return all;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("нет {n} записей {action}");
}

/// Экспорт, тело которого не читается (поток встаёт, когда заполнится канал): ответ с
/// заголовками.
async fn start_export(a: &Api, tok: &str, space: &str) -> axum::response::Response {
    use tower::ServiceExt;
    let r = axum::http::Request::get(format!("/api/v1/spaces/{space}/export"))
        .header("authorization", format!("Bearer {tok}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let r = a.app.clone().oneshot(r).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    r
}

/// Тело ответа целиком: байты или ошибка потока.
async fn finish(r: axum::response::Response) -> Result<Vec<u8>, String> {
    use http_body_util::BodyExt;
    r.into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .map_err(|e| e.to_string())
}

/// Решение с заметкой и вложением 6 МиБ: архив больше канала тела (2 МиБ), непрочитанный
/// поток встаёт.
async fn big_world(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> W {
    let w = world_cfg(name, tweak).await;
    let n = note_as(&w.a, &w.o.tok, G, "большая", "т").await;
    let data: Vec<u8> = (0..6 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    common::attach::upload(&w.a, &w.o.tok, &n, "big.bin", &data).await;
    w
}

/// Архив разбирается целиком.
fn valid_zip(b: &[u8]) -> bool {
    zip::ZipArchive::new(std::io::Cursor::new(b.to_vec()))
        .and_then(|mut z| z.by_name("manifest.json").map(|_| ()))
        .is_ok()
}

/// Слот держится, пока идёт поток: два медленных экспорта (владелец и участник) занимают оба
/// слота — третий (другой пользователь) получает 503; второй экспорт того же пользователя —
/// 503 и при свободном слоте; после чтения тел слоты свободны, архивы целы.
#[tokio::test]
async fn export_slot_held_while_streaming() {
    let w = big_world("export-hold", |_| {}).await;
    let a = &w.a;
    let n = zam_server::api::transfer::export::EXPORT_SLOTS;
    let r1 = start_export(a, &w.o.tok, G).await;
    assert_eq!(a.st.export_slots.available_permits(), n - 1);
    let again = export_tok(a, &w.o.tok, G, "").await;
    assert_eq!(
        again.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "один на пользователя"
    );
    let r2 = start_export(a, &w.rw.tok, G).await;
    assert_eq!(a.st.export_slots.available_permits(), n - 2);
    let third = export_tok(a, &w.ro.tok, G, "").await;
    assert_eq!(
        third.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        third.text()
    );
    assert_eq!(third.header("retry-after"), "30");
    let b1 = finish(r1).await.unwrap();
    let b2 = finish(r2).await.unwrap();
    assert!(valid_zip(&b1) && valid_zip(&b2));
    for _ in 0..200 {
        if a.st.export_slots.available_permits() == n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(a.st.export_slots.available_permits(), n);
    assert!(a.st.export_users.lock().unwrap().is_empty());
    Zip::of(&export_tok(a, &w.ro.tok, G, "").await);
}

/// Экспорт дольше `export.max_secs` обрывается: тело — незавершённый архив, аудит
/// `complete: false, reason: deadline`, долгий читатель базы снят, слот свободен.
#[tokio::test]
async fn export_deadline_aborts() {
    let w = big_world("export-deadline", |c| c.export.max_secs = 1).await;
    let a = &w.a;
    let db = w.db().await;
    let r = start_export(a, &w.o.tok, G).await;
    for _ in 0..200 {
        if db.long_readers() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(db.long_readers(), 1, "экспорт — долгий читатель базы");
    tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
    let body = finish(r).await;
    // тело кончается ошибкой (обрыв соединения), а не штатным концом chunked: браузер не
    // сохранит неполный zip как успешную загрузку
    assert!(
        body.is_err(),
        "тело кончилось штатно: {:?}",
        body.map(|b| b.len())
    );
    let d = done_audits(a, 1).await.remove(0);
    assert_eq!(d["details"]["complete"], json!(false));
    assert_eq!(d["details"]["reason"], json!("deadline"));
    assert_eq!(db.long_readers(), 0);
    assert_eq!(
        a.st.export_slots.available_permits(),
        zam_server::api::transfer::export::EXPORT_SLOTS
    );
}

/// Остановка сервера обрывает идущий экспорт (штатная остановка HTTP ждёт соединения): тело
/// незавершено, аудит `reason: stop`.
#[tokio::test]
async fn export_stops_on_shutdown() {
    let w = big_world("export-stop", |_| {}).await;
    let a = &w.a;
    let r = start_export(a, &w.o.tok, G).await;
    a.st.begin_stop();
    let body = tokio::time::timeout(std::time::Duration::from_secs(20), finish(r))
        .await
        .expect("поток не оборвался");
    // тело кончается ошибкой (обрыв соединения), а не штатным концом chunked: браузер не
    // сохранит неполный zip как успешную загрузку
    assert!(
        body.is_err(),
        "тело кончилось штатно: {:?}",
        body.map(|b| b.len())
    );
    let d = done_audits(a, 1).await.remove(0);
    assert_eq!(d["details"]["complete"], json!(false));
    assert_eq!(d["details"]["reason"], json!("stop"));
    assert!(a.st.wait_exports(std::time::Duration::from_secs(1)).await);
}

/// При остановке новый экспорт — сразу 503 с `Retry-After`: без аудита и без слота.
#[tokio::test]
async fn export_refused_while_stopping() {
    let w = world("export-stopping").await;
    let (a, o) = (&w.a, &w.o);
    a.st.begin_stop();
    for q in ["", "?probe=true"] {
        let r = export_tok(a, &o.tok, G, q).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{q}: {}",
            r.text()
        );
        assert_eq!(r.header("retry-after"), "30");
        assert_eq!(r.json()["type"], json!("/problems/busy"));
    }
    assert_eq!(
        a.st.export_slots.available_permits(),
        zam_server::api::transfer::export::EXPORT_SLOTS
    );
    let all = a
        .adm(Method::GET, "/api/v1/admin/audit", None, None)
        .await
        .json();
    assert!(!all.to_string().contains("space.export"), "{all}");
}

/// Остановка ждёт аудит оборванного экспорта: после `wait_exports` запись `space.export.done`
/// уже есть (так `serve` делает до выхода процесса).
#[tokio::test]
async fn shutdown_waits_export_audit() {
    let w = big_world("export-waitaudit", |_| {}).await;
    let a = &w.a;
    let r = start_export(a, &w.o.tok, G).await;
    a.st.begin_stop();
    assert!(a.st.wait_exports(std::time::Duration::from_secs(5)).await);
    let all = a
        .adm(Method::GET, "/api/v1/admin/audit", None, None)
        .await
        .json();
    assert!(all.to_string().contains("space.export.done"), "{all}");
    drop(r);
}

/// Аудит до тела не записался — 500 без архива и без `space.export.done`.
#[tokio::test]
async fn export_audit_failure_no_body() {
    let w = world("export-auditfail").await;
    let (a, o) = (&w.a, &w.o);
    note_as(a, &o.tok, G, "x", "SECRET-NOTE-TEXT").await;
    zam_server::backup::test_hooks::set_fail_point(&a.st.cfg.data_dir, Some("export_audit"));
    let r = export_tok(a, &o.tok, G, "").await;
    zam_server::backup::test_hooks::set_fail_point(&a.st.cfg.data_dir, None);
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", r.text());
    assert!(!r.text().contains("SECRET-NOTE-TEXT"));
    assert!(!r.body.starts_with(b"PK"));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let all = a
        .adm(Method::GET, "/api/v1/admin/audit", None, None)
        .await
        .json();
    assert!(!all.to_string().contains("space.export"), "{all}");
    assert_eq!(
        a.st.export_slots.available_permits(),
        zam_server::api::transfer::export::EXPORT_SLOTS
    );
    Zip::of(&export_tok(a, &o.tok, G, "").await);
}

/// Слоты экспорта заняты — 503 `/problems/busy` с `Retry-After` (и у `probe`); слот
/// освободился — экспорт идёт. Слот держится до конца потока.
#[tokio::test]
async fn export_busy_when_slots_taken() {
    let w = world("export-busy").await;
    let (a, o) = (&w.a, &w.o);
    note_as(a, &o.tok, G, "x", "x").await;
    let n = zam_server::api::transfer::export::EXPORT_SLOTS as u32;
    let held =
        a.st.export_slots
            .clone()
            .acquire_many_owned(n)
            .await
            .unwrap();
    for q in ["", "?probe=true"] {
        let r = export_tok(a, &o.tok, G, q).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{q}: {}",
            r.text()
        );
        assert_eq!(r.header("retry-after"), "30");
        assert_eq!(r.json()["type"], json!("/problems/busy"));
    }
    drop(held);
    Zip::of(&export_tok(a, &o.tok, G, "").await);
    // поток закончен — все слоты свободны
    for _ in 0..100 {
        if a.st.export_slots.available_permits() == n as usize {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(a.st.export_slots.available_permits(), n as usize);
    // проверка прав — раньше слота: Human токену — 404 и при занятых слотах
    let _held =
        a.st.export_slots
            .clone()
            .acquire_many_owned(n)
            .await
            .unwrap();
    let r = export_tok(a, &o.tok, "human", "?probe=true").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Связь на отчёт API не создаёт (422), но если такое ребро есть в базе, токену оно не
/// выдаётся, сессии WebUI — выдаётся.
#[tokio::test]
async fn export_link_to_report_hidden_from_token() {
    let w = world("export-replink").await;
    let (a, o) = (&w.a, &w.o);
    let rep = w.report("Отчёт").await;
    let n = note_as(a, &o.tok, G, "заметка", "т").await;
    let space = Zip::of(&export_tok(a, &o.tok, G, "").await)
        .manifest()
        .space;
    let ts = zam_store::time::fmt_ts(zam_store::time::now());
    let sql = format!(
        "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id, dst_space_id, type,
                            origin, created_at)
         VALUES ('{}', '{space}', 'node', '{n}', 'entry', '{rep}', '{space}', 'relates_to',
                 'explicit', '{ts}')",
        zam_core::Id::new()
    );
    w.db()
        .await
        .write(move |tx| Ok(tx.execute(&sql, [])?))
        .await
        .unwrap();
    let z = Zip::of(&export_tok(a, &o.tok, G, "").await);
    assert!(!z.files.contains_key("_links.jsonl"), "{:?}", z.names);
    assert!(z.contains(&rep).is_empty());
    let z = Zip::of(&export_web(a, o, G, "").await);
    assert!(z.text("_links.jsonl").contains(&rep));
}

/// Фильтры: `paths` — заметки под префиксами (по целым сегментам), без журналов; `kinds` —
/// записи этих видов, без заметок; оба — обе части; `history=false` — без `_history`.
#[tokio::test]
async fn export_filter_paths_kinds_history() {
    let w = world("export-filter").await;
    let (a, o) = (&w.a, &w.o);
    note_as(a, &o.tok, G, "docs/a", "a").await;
    note_as(a, &o.tok, G, "docs/sub/b", "b").await;
    note_as(a, &o.tok, G, "docsx/c", "c").await;
    note_as(a, &o.tok, G, "other", "o").await;
    let ins = entry_as(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Инсайт", "body": "i", "data": {"kind": "pattern"}}),
    )
    .await;
    let task = entry_as(
        a,
        &o.tok,
        G,
        json!({"kind": "task", "title": "Задача", "body": "t"}),
    )
    .await;

    let md = |z: &Zip| -> Vec<String> {
        z.names
            .iter()
            .filter(|n| n.ends_with(".md") && !n.starts_with('_'))
            .cloned()
            .collect()
    };
    let z = Zip::of(&export_tok(a, &o.tok, G, "?paths=docs").await);
    assert_eq!(md(&z), vec!["docs/a.md", "docs/sub/b.md"]);
    assert!(z.with_prefix("_journals/").is_empty());
    assert_eq!(z.manifest().filter.paths, vec!["docs"]);

    let z = Zip::of(&export_tok(a, &o.tok, G, "?kinds=insight").await);
    assert!(md(&z).is_empty());
    assert_eq!(
        z.with_prefix("_journals/"),
        vec![&format!("_journals/insight/{ins}.md")]
    );

    let z = Zip::of(&export_tok(a, &o.tok, G, "?paths=docs/sub,other&kinds=task,insight").await);
    assert_eq!(md(&z), vec!["docs/sub/b.md", "other.md"]);
    assert_eq!(z.with_prefix("_journals/").len(), 2);
    assert!(!z.with_prefix("_history/").is_empty());

    let z = Zip::of(&export_tok(a, &o.tok, G, "?history=false").await);
    assert!(z.with_prefix("_history/").is_empty());
    assert_eq!(md(&z).len(), 4);
    assert!(z.files.contains_key(&format!("_journals/task/{task}.md")));
    assert!(!z.manifest().filter.history);
    check_fingerprint(&z);

    for bad in ["?kinds=note", "?paths=a//b", "?history=maybe"] {
        let r = export_tok(a, &o.tok, G, bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
}

/// Файла блоба нет на диске — экспорт идёт, пара — в `missing_blobs` описи.
#[tokio::test]
async fn export_missing_blob_listed() {
    let w: W = world("export-missing").await;
    let (a, o) = (&w.a, &w.o);
    let n = note_as(a, &o.tok, G, "с вложением", "т").await;
    let up = common::attach::upload(a, &o.tok, &n, "f.bin", b"payload-123").await;
    let sha = up["sha256"].as_str().unwrap().to_string();
    let path =
        a.st.data_dir
            .blobs_dir()
            .join(&o.id)
            .join(&sha[..2])
            .join(&sha);
    std::fs::remove_file(&path).unwrap();
    let z = Zip::of(&export_tok(a, &o.tok, G, "").await);
    assert_eq!(z.manifest().missing_blobs, vec![format!("{sha}/f.bin")]);
    assert!(z.with_prefix("_attachments/").is_empty());
    let (f, _) = z.front("с вложением.md");
    assert_eq!(f["attachments"][0]["file"], Value::Null);
}
