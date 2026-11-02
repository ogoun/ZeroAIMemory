//! Создание отчёта (задача 2 M5.5, спецификация §4–6): `POST /spaces/{space}/entries` с
//! multipart (CLI) и JSON с `images` (MCP), правила ссылок тела, переписывание ссылок на
//! `_attachments/…`, лимиты `[reports]`, пространства и права, `Idempotency-Key`.
//!
//! Пользователи: A — владелец решения, B — участник. Серверы без моделей (FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine as _;
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::ServerConfig;
use zam_store::{BlobStore, EntryAttachment, entry_attachments, get_entry};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12";
const BOUNDARY: &str = "zam-report-boundary-41c9";

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
}

/// Владелец A регистрирует решение; участник B получает доступ `access` (если задан).
async fn world_cfg(name: &str, access: Option<&str>, tweak: impl FnOnce(&mut ServerConfig)) -> W {
    let a = api(&format!("reports-{name}"), tweak).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let w = W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
    };
    let reg = json!({"agent_solution_id": G, "name": "Исследование"});
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(reg.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    if let Some(access) = access {
        let r =
            w.a.tok(&w.b_tok, Method::POST, "/api/v1/solutions", Some(reg))
                .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        let r =
            w.a.tok(
                &w.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{}/approve", w.b_id),
                Some(json!({ "access": access })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    w
}

async fn world(name: &str) -> W {
    world_cfg(name, None, |_| {}).await
}

/// Файлы отчёта: (имя, байты).
type Files<'a> = Vec<(&'a str, &'a [u8])>;

/// Часть multipart: (имя поля, имя файла, тип, байты).
type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
    let mut b = Vec::new();
    for (name, file, ct, data) in parts {
        b.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disp = format!("Content-Disposition: form-data; name=\"{name}\"");
        if let Some(f) = file {
            disp.push_str(&format!("; filename=\"{f}\""));
        }
        b.extend_from_slice(disp.as_bytes());
        b.extend_from_slice(b"\r\n");
        if let Some(ct) = ct {
            b.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    b
}

/// PNG-подобные байты (сигнатура и `n` байт содержимого по `seed`).
fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

impl W {
    fn uri(&self, space: &str) -> String {
        format!("/api/v1/spaces/{space}/entries")
    }

    /// Multipart: `meta`, затем `files` (имя, байты) частями `file`.
    async fn mp(
        &self,
        tok: &str,
        space: &str,
        meta: Value,
        files: &[(&str, &[u8])],
        key: Option<&str>,
    ) -> Resp {
        let meta = meta.to_string();
        let mut parts: Vec<Part<'_>> =
            vec![("meta", None, Some("application/json"), meta.as_bytes())];
        for (n, d) in files {
            parts.push(("file", Some(n), Some("application/octet-stream"), d));
        }
        self.mp_raw(tok, space, multipart(&parts), key).await
    }

    async fn mp_raw(&self, tok: &str, space: &str, body: Vec<u8>, key: Option<&str>) -> Resp {
        let mut r = Request::post(self.uri(space))
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            );
        if let Some(k) = key {
            r = r.header("idempotency-key", k);
        }
        self.a.send(r.body(Body::from(body)).unwrap()).await
    }

    /// JSON (как MCP): `kind: report` и поля `v`.
    async fn js(&self, tok: &str, space: &str, mut v: Value, key: Option<&str>) -> Resp {
        v["kind"] = json!("report");
        let extra: Vec<(&str, &str)> = key.map(|k| ("idempotency-key", k)).into_iter().collect();
        self.a
            .tok_h(tok, Method::POST, &self.uri(space), Some(v), &extra)
            .await
    }

    fn owner(&self) -> Id {
        self.a_id.parse().unwrap()
    }

    async fn db(&self) -> zam_store::Db {
        self.a.st.users.get(self.owner()).await.unwrap()
    }

    async fn body_of(&self, id: &str) -> String {
        get_entry(&self.db().await, id.parse().unwrap())
            .await
            .unwrap()
            .body
    }

    async fn files_of(&self, id: &str) -> Vec<EntryAttachment> {
        entry_attachments(&self.db().await, id.parse().unwrap())
            .await
            .unwrap()
    }

    /// Число записей вида report в базе владельца (в том числе удалённых).
    async fn reports(&self) -> i64 {
        self.db()
            .await
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM entries WHERE kind = 'report'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
    }

    fn blob_root(&self) -> std::path::PathBuf {
        BlobStore::new(&self.a.st.data_dir.blobs_dir(), self.owner())
            .root()
            .to_path_buf()
    }
}

/// Файлы под каталогом (рекурсивно; нет каталога — пусто).
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn assert_status(r: &Resp, want: StatusCode, ctx: &str) {
    assert_eq!(r.status, want, "{ctx}: {}", r.text());
}

fn id_of(r: &Resp) -> String {
    r.json()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn create_report_json_returns_id_and_url_only() {
    let w = world_cfg("json-url", None, |c| {
        c.server.public_url = Some("https://zam.example.org/memory/".into());
    })
    .await;
    let img = png(1, 300);
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({
                "title": "Обучение модели X",
                "body": "# Итог\n\nТочность 0,93.\n\n![loss](figs/loss.png)\n",
                "tags": ["ml"],
                "data": {"refs": {"commits": ["abc123"]}},
                "images": [{"name": "figs/loss.png", "data": b64(&img)}],
            }),
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "создание");
    let j = r.json();
    let keys: Vec<&String> = j.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["id", "url"], "{j}");
    let id = id_of(&r);
    assert_eq!(
        j["url"],
        format!(
            "https://zam.example.org/memory/#/u/{}/s/{G}/j/reports/{id}",
            w.a_id
        )
    );
    let text = r.text();
    assert!(!text.contains("Точность") && !text.contains("p:"), "{text}");
    let e = get_entry(&w.db().await, id.parse().unwrap()).await.unwrap();
    assert_eq!(e.kind, zam_store::EntryKind::Report);
    assert_eq!(e.data, json!({"refs": {"commits": ["abc123"]}}));
    assert_eq!(e.tags, vec!["ml".to_string()]);
    let files = w.files_of(&id).await;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].mime, "image/png");
    assert!(
        e.body
            .contains(&format!("_attachments/{}/loss.png", &files[0].sha256[..16]))
    );

    // без public_url — адрес по Host
    let w2 = world("json-url-host").await;
    let r =
        w2.a.tok_h(
            &w2.a_tok,
            Method::POST,
            &w2.uri(G),
            Some(json!({"kind": "report", "title": "Т"})),
            &[("host", "zam.lan:7700")],
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "по Host");
    let url = r.json()["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with(&format!(
            "http://zam.lan:7700/#/u/{}/s/{G}/j/reports/",
            w2.a_id
        )),
        "{url}"
    );
}

#[tokio::test]
async fn create_report_multipart_three_png_rewrites_links() {
    let w = world("mp-three").await;
    let (p1, p2, p3) = (png(1, 100), png(2, 200), png(3, 300));
    let body = "# Сравнение\n\n![loss](figs/loss.png)\n\n![acc](figs/acc.png)\n\n\
                | a | b |\n|---|---|\n| ![t](figs/table.png) | 1 |\n\n$$E=mc^2$$\n";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Три графика", "body": body}),
            &[
                ("figs/loss.png", &p1),
                ("figs/acc.png", &p2),
                ("figs/table.png", &p3),
            ],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "multipart");
    let id = id_of(&r);
    let files = w.files_of(&id).await;
    assert_eq!(files.len(), 3);
    let stored = w.body_of(&id).await;
    for (name, data) in [("loss.png", &p1), ("acc.png", &p2), ("table.png", &p3)] {
        let f = files.iter().find(|f| f.filename == name).unwrap();
        assert_eq!(f.size, data.len() as u64);
        assert_eq!(f.mime, "image/png");
        let link = format!("_attachments/{}/{name}", &f.sha256[..16]);
        assert!(stored.contains(&link), "{link} в {stored}");
        // байты блоба на диске
        let path = BlobStore::new(&w.a.st.data_dir.blobs_dir(), w.owner()).path_of(&f.sha256);
        assert_eq!(&std::fs::read(path).unwrap(), data);
    }
    assert!(!stored.contains("figs/"), "{stored}");
    assert!(stored.contains("$$E=mc^2$$"));
}

#[tokio::test]
async fn create_report_meta_must_be_first_part() {
    let w = world("meta-first").await;
    let img = png(1, 50);
    let meta = json!({"title": "Т", "body": "![a](a.png)"}).to_string();
    let body = multipart(&[
        ("file", Some("a.png"), None, &img),
        ("meta", None, None, meta.as_bytes()),
    ]);
    let r = w.mp_raw(&w.a_tok, G, body, None).await;
    assert_status(&r, StatusCode::BAD_REQUEST, "file первой");
    assert!(r.text().contains("meta"), "{}", r.text());
    let r = w.mp_raw(&w.a_tok, G, multipart(&[]), None).await;
    assert_status(&r, StatusCode::BAD_REQUEST, "без частей");
    let r = w
        .mp_raw(
            &w.a_tok,
            G,
            multipart(&[("meta", None, None, b"{not json")]),
            None,
        )
        .await;
    assert_status(&r, StatusCode::BAD_REQUEST, "meta не JSON");
    assert_eq!(w.reports().await, 0);
    assert!(files_under(&w.blob_root()).is_empty());
}

#[tokio::test]
async fn report_unreferenced_file_422() {
    let w = world("unreferenced").await;
    let img = png(1, 50);
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "Текст без картинок"}),
            &[("lost.png", &img)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "файл без ссылки");
    assert!(r.text().contains("lost.png"), "{}", r.text());
    // отказ до записи байт: ни блоба, ни временного файла в `.incoming`
    let root = w.blob_root();
    assert!(files_under(&root.join(".incoming")).is_empty());
    assert!(files_under(&root).is_empty(), "{:?}", files_under(&root));
    // то же через JSON
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "", "images": [{"name": "lost.png", "data": b64(&img)}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "JSON файл без ссылки");
    assert!(files_under(&root).is_empty());
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_missing_file_for_relative_link_422() {
    let w = world("missing").await;
    let img = png(1, 50);
    let body = "![a](a.png)\n\n[данные](data/results.csv)\n";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body}),
            &[("a.png", &img)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "нет части");
    assert!(r.text().contains("data/results.csv"), "{}", r.text());
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body, "images": [{"name": "a.png", "data": b64(&img)}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "JSON нет файла");
    assert!(r.text().contains("data/results.csv"), "{}", r.text());
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_hostile_names_and_links_422() {
    let w = world("hostile").await;
    // вложение заметки — чужая ссылка `_attachments/…`
    let n = w.a.note("knowledge", "Галерея", "# Г\n").await;
    let up = common::attach::upload(
        &w.a,
        &w.a.token,
        n["id"].as_str().unwrap(),
        "кот.png",
        &png(9, 40),
    )
    .await;
    let note_link = up["link"].as_str().unwrap().to_string();
    let img = png(1, 50);
    let csv = b"a,b\n1,2\n".to_vec();
    let cases: Vec<(&str, String, Files<'_>)> = vec![
        ("..", "![a](../a.png)".into(), vec![]),
        ("абсолютный", "![a](/etc/a.png)".into(), vec![]),
        ("абсолютный Windows", "[d](C:/data/a.csv)".into(), vec![]),
        ("обратная косая", "![a](figs\\a.png)".into(), vec![]),
        ("вложение заметки", format!("![кот]({note_link})"), vec![]),
        (
            "внешняя картинка",
            "![a](https://example.org/a.png)".into(),
            vec![],
        ),
        (
            "data: картинка",
            "![a](data:image/png;base64,iVBORw0KGgo=)".into(),
            vec![],
        ),
        ("javascript:", "[x](javascript:alert(1))".into(), vec![]),
        (
            "картинка → CSV",
            "![t](t.csv)".into(),
            vec![("t.csv", &csv)],
        ),
        ("кавычка в имени", "[d](a\"b.csv)".into(), vec![]),
        ("сегмент .", "![a](figs/./a.png)".into(), vec![]),
    ];
    for (what, body, files) in cases {
        let r = w
            .mp(
                &w.a_tok,
                G,
                json!({"title": "Т", "body": body}),
                &files,
                None,
            )
            .await;
        assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, what);
        assert_eq!(
            r.json()["type"],
            "/problems/unprocessable",
            "{what}: {}",
            r.text()
        );
    }
    // имена частей, которых нет в теле: путь наружу, абсолютный, с кавычкой
    for name in ["../a.png", "/etc/a.png", "C:\\a.png", "a\\\"b.png"] {
        let r = w
            .mp(
                &w.a_tok,
                G,
                json!({"title": "Т", "body": "![a](a.png)"}),
                &[(name, &img), ("a.png", &img)],
                None,
            )
            .await;
        assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, name);
    }
    // лишняя часть не file
    let meta = json!({"title": "Т", "body": "![a](a.png)"}).to_string();
    let body = multipart(&[
        ("meta", None, None, meta.as_bytes()),
        ("file", Some("a.png"), None, &img),
        ("extra", None, None, b"x"),
    ]);
    let r = w.mp_raw(&w.a_tok, G, body, None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "лишняя часть");
    // повтор имени — 400
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "![a](a.png)"}),
            &[("a.png", &img), ("a.png", &img)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::BAD_REQUEST, "повтор имени");
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_other_files_any_mime_ok() {
    let w = world("other-files").await;
    let csv = b"epoch,loss\n1,0.5\n2,0.3\n".to_vec();
    let nb = br#"{"cells": [], "nbformat": 4}"#.to_vec();
    let log = b"step 1 ok\nstep 2 ok\n".to_vec();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#.to_vec();
    let body = "[таблица](results.csv), [ноутбук](nb/train.ipynb), [лог](train.log)\n\n\
                ![схема](arch.svg)\n\n[ещё раз](results.csv) [сайт](https://example.org) \
                [почта](mailto:a@example.org) [раздел](#итог) [[Заметка]]\n";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Файлы", "body": body}),
            &[
                ("results.csv", &csv),
                ("nb/train.ipynb", &nb),
                ("train.log", &log),
                ("arch.svg", &svg),
            ],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "прочие файлы");
    let id = id_of(&r);
    let files = w.files_of(&id).await;
    let mut names: Vec<(&str, &str)> = files
        .iter()
        .map(|f| (f.filename.as_str(), f.mime.as_str()))
        .collect();
    names.sort();
    assert_eq!(names.len(), 4, "{names:?}");
    assert_eq!(names[0], ("arch.svg", "image/svg+xml"));
    assert_eq!(names[1], ("results.csv", "text/csv"));
    assert_eq!(names[2].0, "train.ipynb");
    assert_eq!(names[3].0, "train.log");
    let stored = w.body_of(&id).await;
    let csv_sha = &files
        .iter()
        .find(|f| f.filename == "results.csv")
        .unwrap()
        .sha256;
    assert_eq!(
        stored
            .matches(&format!("_attachments/{}/results.csv", &csv_sha[..16]))
            .count(),
        2,
        "{stored}"
    );
    for keep in [
        "https://example.org",
        "mailto:a@example.org",
        "(#итог)",
        "[[Заметка]]",
    ] {
        assert!(stored.contains(keep), "{keep}: {stored}");
    }
}

#[tokio::test]
async fn report_limits_count_422_total_413_file_413() {
    // при max_files = 2 сумма двух файлов по 512 Б не превысит 1 КиБ — для проверки суммы 3 файла
    let w = world_cfg("limits", None, |c| {
        c.reports.max_files = 3;
        c.reports.max_total_bytes = 1024;
        c.reports.mcp_max_bytes = 1024;
        c.limits.blob_max_bytes = 512;
    })
    .await;
    let small = |s: u8| png(s, 10);
    let (f1, f2, f3, f4) = (small(1), small(2), small(3), small(4));
    let body4 = "![1](1.png) ![2](2.png) ![3](3.png) ![4](4.png)";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body4}),
            &[
                ("1.png", &f1),
                ("2.png", &f2),
                ("3.png", &f3),
                ("4.png", &f4),
            ],
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "число файлов");
    assert!(r.text().contains('3'), "{}", r.text());
    // сумма: 3 × 400 Б > 1 КиБ
    let (b1, b2, b3) = (png(1, 392), png(2, 392), png(3, 392));
    let body3 = "![1](1.png) ![2](2.png) ![3](3.png)";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body3}),
            &[("1.png", &b1), ("2.png", &b2), ("3.png", &b3)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "сумма");
    assert!(r.text().contains("max_total_bytes"), "{}", r.text());
    // файл больше blobs-предела
    let big = png(5, 600);
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "![b](big.png)"}),
            &[("big.png", &big)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "файл");
    assert!(r.text().contains("512"), "{}", r.text());
    // JSON: те же пределы
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "![b](big.png)",
                   "images": [{"name": "big.png", "data": b64(&big)}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "JSON файл");
    let imgs: Vec<Value> = [
        ("1.png", &f1),
        ("2.png", &f2),
        ("3.png", &f3),
        ("4.png", &f4),
    ]
    .iter()
    .map(|(n, d)| json!({"name": n, "data": b64(d)}))
    .collect();
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body4, "images": imgs}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "JSON число файлов");
    assert_eq!(w.reports().await, 0);
    // в пределах — создаётся
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body3}),
            &[("1.png", &f1), ("2.png", &f2), ("3.png", &f3)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "в пределах");
}

#[tokio::test]
async fn report_json_images_limit_413_with_cli_hint() {
    let w = world_cfg("mcp-limit", None, |c| {
        c.reports.mcp_max_bytes = 2000;
    })
    .await;
    let img = png(1, 1500);
    let body = format!("![a](a.png)\n\n{}", "т".repeat(300));
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body, "images": [{"name": "a.png", "data": b64(&img)}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "MCP предел");
    let j = r.json();
    assert_eq!(j["type"], "/problems/too-large");
    assert!(j["detail"].as_str().unwrap().contains("zam report"), "{j}");
    assert!(files_under(&w.blob_root()).is_empty());
    // multipart того же отчёта предел MCP не касается
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": body}),
            &[("a.png", &img)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "multipart");
}

#[tokio::test]
async fn report_json_bad_base64_422() {
    let w = world("bad-b64").await;
    for data in ["@@@не base64@@@", "iVBORw0KGgo", "iVBO RW0K"] {
        let r = w
            .js(
                &w.a_tok,
                G,
                json!({"title": "Т", "body": "![a](a.png)",
                       "images": [{"name": "a.png", "data": data}]}),
                None,
            )
            .await;
        assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, data);
        assert!(r.text().contains("base64"), "{}", r.text());
    }
    // форма images неверна — 400
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "", "images": [{"name": "a.png"}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::BAD_REQUEST, "без data");
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_links_in_code_not_rewritten_not_required() {
    let w = world("code-links").await;
    let body = "Пример: `![x](figs/x.png)` и\n\n```markdown\n![y](figs/y.png)\n[r]: figs/z.png\n```\n\n    \
                ![z](figs/w.png)\n";
    let r = w
        .js(&w.a_tok, G, json!({"title": "Код", "body": body}), None)
        .await;
    assert_status(&r, StatusCode::CREATED, "ссылки в коде");
    let id = id_of(&r);
    assert_eq!(w.body_of(&id).await, body);
    assert!(w.files_of(&id).await.is_empty());
}

#[tokio::test]
async fn report_angle_percent_reference_links_rewritten() {
    let w = world("link-forms").await;
    let (p1, p2) = (png(1, 80), png(2, 90));
    let body = "![a](<figs/my plot.png>)\n\n![b](figs/my%20plot.png \"Подпись\")\n\n\
                ![c][r] и [ссылка][r]\n\n[r]: figs/a.png\n";
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Формы", "body": body}),
            &[("figs/my plot.png", &p1), ("figs/a.png", &p2)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "формы ссылок");
    let id = id_of(&r);
    let files = w.files_of(&id).await;
    let sha = |n: &str| files.iter().find(|f| f.filename == n).unwrap().sha256[..16].to_string();
    let (s1, s2) = (sha("my plot.png"), sha("a.png"));
    assert_eq!(
        w.body_of(&id).await,
        format!(
            "![a](<_attachments/{s1}/my%20plot.png>)\n\n![b](_attachments/{s1}/my%20plot.png \
             \"Подпись\")\n\n![c][r] и [ссылка][r]\n\n[r]: _attachments/{s2}/a.png\n"
        )
    );
    // `./` в теле и в имени части — тот же файл
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Точка", "body": "![a](./figs/a.png)"}),
            &[("figs/a.png", &p2)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "./");
}

#[tokio::test]
async fn report_same_content_two_names_one_blob() {
    let w = world("one-blob").await;
    let img = png(7, 120);
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "![a](a.png) ![b](b/b.png)"}),
            &[("a.png", &img), ("b/b.png", &img)],
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "два имени");
    let id = id_of(&r);
    let files = w.files_of(&id).await;
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].sha256, files[1].sha256);
    let blobs: Vec<_> = files_under(&w.blob_root());
    assert_eq!(blobs.len(), 1, "{blobs:?}");
    let sha = files[0].sha256.clone();
    let refcount = w
        .db()
        .await
        .read(move |c| {
            Ok(
                c.query_row("SELECT refcount FROM blobs WHERE sha256 = ?1", [sha], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(refcount, 2);
}

#[tokio::test]
async fn report_space_kinds() {
    let w = world("spaces").await;
    let meta = json!({"title": "Т", "body": "текст"});
    let r = w.js(&w.a_tok, "knowledge", meta.clone(), None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "knowledge");
    let r = w.mp(&w.a_tok, "knowledge", meta.clone(), &[], None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "knowledge multipart");
    w.a.st
        .system
        .set_setting("rules.agent_write", &true)
        .await
        .unwrap();
    let r = w.js(&w.a_tok, "rules", meta.clone(), None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "rules");
    let r = w.mp(&w.a_tok, "rules", meta.clone(), &[], None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "rules multipart");
    let r = w.js(&w.a_tok, "human", meta.clone(), None).await;
    assert_status(&r, StatusCode::NOT_FOUND, "human");
    let r = w.mp(&w.a_tok, "human", meta.clone(), &[], None).await;
    assert_status(&r, StatusCode::NOT_FOUND, "human multipart");
    assert_eq!(w.reports().await, 0);
    // в решении — создаётся
    let r = w.js(&w.a_tok, G, meta, None).await;
    assert_status(&r, StatusCode::CREATED, "решение");
}

#[tokio::test]
async fn report_member_rw_creates_ro_403() {
    let img = png(3, 60);
    let meta = json!({"title": "Отчёт участника", "body": "![a](a.png)"});
    let w = world_cfg("member-rw", Some("rw"), |_| {}).await;
    let r = w
        .mp(&w.b_tok, G, meta.clone(), &[("a.png", &img)], None)
        .await;
    assert_status(&r, StatusCode::CREATED, "участник rw");
    let j = r.json();
    // запись и блоб — в базе и каталоге владельца
    assert!(
        j["url"]
            .as_str()
            .unwrap()
            .contains(&format!("#/u/{}/s/{G}/", w.a_id)),
        "{j}"
    );
    assert_eq!(w.files_of(&id_of(&r)).await.len(), 1);
    assert_eq!(files_under(&w.blob_root()).len(), 1);
    // токен ro участника и владельца — 403
    let ro = w.a.issue(&w.b_id, "ro").await;
    let r = w.mp(&ro, G, meta.clone(), &[("a.png", &img)], None).await;
    assert_status(&r, StatusCode::FORBIDDEN, "токен ro участника");
    let ro_a = w.a.issue(&w.a_id, "ro").await;
    let r = w.js(&ro_a, G, json!({"title": "Т"}), None).await;
    assert_status(&r, StatusCode::FORBIDDEN, "токен ro владельца");

    let w = world_cfg("member-ro", Some("ro"), |_| {}).await;
    let r = w
        .mp(&w.b_tok, G, meta.clone(), &[("a.png", &img)], None)
        .await;
    assert_status(&r, StatusCode::FORBIDDEN, "участник ro");
    let r = w.js(&w.b_tok, G, json!({"title": "Т"}), None).await;
    assert_status(&r, StatusCode::FORBIDDEN, "участник ro JSON");
    assert_eq!(w.reports().await, 0);
    assert!(files_under(&w.blob_root()).is_empty());
}

#[tokio::test]
async fn report_cookie_post_403() {
    let w = world("cookie").await;
    let meta = json!({"kind": "report", "title": "Из WebUI", "body": "текст"});
    let r =
        w.a.adm(Method::POST, &w.uri(G), Some(meta.clone()), Some(&w.a_id))
            .await;
    assert_status(&r, StatusCode::FORBIDDEN, "cookie JSON");
    assert!(r.text().contains("агент"), "{}", r.text());
    // multipart по cookie
    let m = meta.to_string();
    let req = Request::post(w.uri(G))
        .header(header::COOKIE, &w.a.cookie)
        .header("x-csrf-token", &w.a.csrf)
        .header("x-zam-user", &w.a_id)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(multipart(&[("meta", None, None, m.as_bytes())])))
        .unwrap();
    let r = w.a.send(req).await;
    assert_status(&r, StatusCode::FORBIDDEN, "cookie multipart");
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_idempotency_repeat_200_mismatch_422() {
    let w = world("idem").await;
    let img = png(1, 70);
    let meta = json!({"title": "Т", "body": "![a](a.png)"});
    let r = w
        .mp(&w.a_tok, G, meta.clone(), &[("a.png", &img)], Some("k-1"))
        .await;
    assert_status(&r, StatusCode::CREATED, "первый");
    let id = id_of(&r);
    let r = w
        .mp(&w.a_tok, G, meta.clone(), &[("a.png", &img)], Some("k-1"))
        .await;
    assert_status(&r, StatusCode::OK, "повтор");
    assert_eq!(id_of(&r), id);
    assert_eq!(
        r.json().as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["id", "url"]
    );
    // тот же ключ — другой файл и другой заголовок
    let other = png(2, 70);
    let r = w
        .mp(&w.a_tok, G, meta.clone(), &[("a.png", &other)], Some("k-1"))
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "другой файл");
    assert_eq!(r.json()["type"], "/problems/idempotency-mismatch");
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Другой", "body": "![a](a.png)"}),
            &[("a.png", &img)],
            Some("k-1"),
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "другой заголовок");
    assert_eq!(r.json()["type"], "/problems/idempotency-mismatch");
    // JSON: повтор — 200, расхождение — 422
    let v = json!({"title": "J", "body": "x"});
    let r = w.js(&w.a_tok, G, v.clone(), Some("k-2")).await;
    assert_status(&r, StatusCode::CREATED, "JSON первый");
    let jid = id_of(&r);
    let r = w.js(&w.a_tok, G, v, Some("k-2")).await;
    assert_status(&r, StatusCode::OK, "JSON повтор");
    assert_eq!(id_of(&r), jid);
    let r = w
        .js(&w.a_tok, G, json!({"title": "J", "body": "y"}), Some("k-2"))
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "JSON другое тело");
    assert_eq!(w.reports().await, 2);
}

#[tokio::test]
async fn report_data_refs_validated() {
    let w = world("data-refs").await;
    let long: Vec<String> = (0..201).map(|i| format!("c{i}")).collect();
    for (what, data) in [
        ("лишний ключ data", json!({"kind": "feat"})),
        ("лишний ключ refs", json!({"refs": {"issues": ["1"]}})),
        ("не массив", json!({"refs": {"commits": "abc"}})),
        ("не строки", json!({"refs": {"files": [1]}})),
        ("длина", json!({"refs": {"commits": long}})),
        ("refs не объект", json!({"refs": []})),
        ("data не объект", json!([])),
    ] {
        let r = w
            .js(&w.a_tok, G, json!({"title": "Т", "data": data}), None)
            .await;
        assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, what);
    }
    let r = w
        .mp(
            &w.a_tok,
            G,
            json!({"title": "Т", "data": {"refs": {"tasks": 5}}}),
            &[],
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "multipart data");
    let ok = json!({"refs": {"commits": ["a1"], "files": ["src/a.rs"], "notes": ["n:1"],
                             "tasks": ["k:1"]}});
    let r = w
        .js(&w.a_tok, G, json!({"title": "Т", "data": ok.clone()}), None)
        .await;
    assert_status(&r, StatusCode::CREATED, "все ключи refs");
    let e = get_entry(&w.db().await, id_of(&r).parse().unwrap())
        .await
        .unwrap();
    assert_eq!(e.data, ok);
}

#[tokio::test]
async fn report_request_refs_field_422() {
    let w = world("refs-field").await;
    let refs = json!({"files": [{"path": "src/a.rs"}]});
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "refs": refs.clone()}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "JSON refs");
    assert!(r.text().contains("data.refs"), "{}", r.text());
    let r = w
        .mp(&w.a_tok, G, json!({"title": "Т", "refs": refs}), &[], None)
        .await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "multipart refs");
    // прочие поля записи (links, status, certainty) — неизвестны: 400
    for extra in [
        json!({"title": "Т", "links": []}),
        json!({"title": "Т", "status": "open"}),
        json!({"title": "Т", "certainty": "axiom"}),
    ] {
        let r = w.js(&w.a_tok, G, extra.clone(), None).await;
        assert_status(&r, StatusCode::BAD_REQUEST, &extra.to_string());
    }
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn report_body_too_large_413() {
    let w = world_cfg("body-large", None, |c| {
        c.limits.note_max_bytes = 1000;
    })
    .await;
    let body = "а".repeat(600); // 1200 байт
    let r = w
        .js(&w.a_tok, G, json!({"title": "Т", "body": body}), None)
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "JSON тело");
    let r = w
        .mp(&w.a_tok, G, json!({"title": "Т", "body": body}), &[], None)
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "multipart тело");
    // часть meta больше предела тела записи с запасом — 413 до разбора
    let huge = "x".repeat(2 * 1000 + 64 * 1024 + 64 * 1024 + 10);
    let r = w
        .mp(&w.a_tok, G, json!({"title": "Т", "body": huge}), &[], None)
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "часть meta");
    // заголовок — общая проверка записей (422)
    let r = w.js(&w.a_tok, G, json!({"title": "  "}), None).await;
    assert_status(&r, StatusCode::UNPROCESSABLE_ENTITY, "пустой заголовок");
    assert_eq!(w.reports().await, 0);
}

#[tokio::test]
async fn other_kinds_json_limit_unchanged() {
    let w = world_cfg("json-limit", None, |c| {
        c.limits.note_max_bytes = 1000;
    })
    .await;
    // note_body_max = 2 × 1000 + 64 КиБ; тело changelog больше — 413 до разбора, как раньше
    let pad = "x".repeat(70 * 1024);
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &w.uri(G),
            Some(
                json!({"kind": "changelog", "title": "Т", "data": {"kind": "feat"},
                        "pad": pad}),
            ),
        )
        .await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "changelog");
    // неразборчивое тело того же размера — тоже 413, а не 400
    let raw = Request::post(w.uri(G))
        .header(header::AUTHORIZATION, format!("Bearer {}", w.a_tok))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!("{{{pad}")))
        .unwrap();
    let r = w.a.send(raw).await;
    assert_status(&r, StatusCode::PAYLOAD_TOO_LARGE, "не JSON");
    // обычная запись в пределах — как раньше
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &w.uri(G),
            Some(json!({"kind": "changelog", "title": "Т", "data": {"kind": "feat"}})),
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "changelog в пределах");
    // отчёт MCP того же размера (файл 70 КиБ в base64) — принимается
    let img = png(1, 70 * 1024);
    let r = w
        .js(
            &w.a_tok,
            G,
            json!({"title": "Т", "body": "![a](a.png)",
                   "images": [{"name": "a.png", "data": b64(&img)}]}),
            None,
        )
        .await;
    assert_status(&r, StatusCode::CREATED, "отчёт JSON");
    // JSON больше предела чтения отчёта (≈ 135 КиБ при mcp_max_bytes 4 КиБ) — 413
    let img = png(2, 120 * 1024);
    let w2 = world_cfg("json-limit-report", None, |c| {
        c.limits.note_max_bytes = 1000;
        c.reports.mcp_max_bytes = 4096;
    })
    .await;
    let r = w2
        .js(
            &w2.a_tok,
            G,
            json!({"title": "Т", "body": "![a](a.png)",
                   "images": [{"name": "a.png", "data": b64(&img)}]}),
            None,
        )
        .await;
    assert_status(
        &r,
        StatusCode::PAYLOAD_TOO_LARGE,
        "отчёт больше предела JSON",
    );
}
