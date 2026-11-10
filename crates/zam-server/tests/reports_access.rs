//! Чтение, файлы, неизменность и права удаления отчётов (задача 3 M5.5, спецификация §4–5, §8,
//! §12 п. 3 и 5): страница `GET /entries/{id}?format=html`, файлы
//! `GET /entries/{id}/attachments/{sha256}/{имя}`, лента `?kind=report` (и корзина
//! `deleted=only`), 409 `/problems/report-immutable` на изменяющих маршрутах, удаление и
//! восстановление — только владелец решения в своей сессии WebUI.
//!
//! Пользователи: A — владелец решения, B — участник `rw`, C — участник `ro`; у каждого — токен
//! и сессия WebUI. Отчёты пишет агент A (токен). Серверы без моделей (FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::Resp;
use common::reports::*;
use serde_json::{Value, json};
use zam_store::get_entry;

/// Мир задачи 3: каталог данных — `reports-access-<name>`.
async fn world(name: &str) -> W {
    common::reports::world(&format!("reports-access-{name}")).await
}

#[tokio::test]
async fn report_html_renders_with_entry_attachment_urls() {
    let w = world("html").await;
    let id = w.report("Обучение модели").await;
    let loss = w.file(&id, "loss.png").await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "страница");
    let j = r.json();
    let html = j["html"].as_str().unwrap();
    let src = format!(
        "src=\"api/v1/entries/{id}/attachments/{}/loss.png\"",
        loss.sha256
    );
    assert!(html.contains(&src), "{src} в {html}");
    assert!(html.contains("class=\"math\""), "{html}");
    assert!(html.contains("<pre class=\"mermaid-src\""), "{html}");
    assert!(html.contains("<table "), "{html}");
    assert!(html.contains("class=\"hl-"), "{html}");
    // прочий файл — ссылка на скачивание через маршрут записи
    assert!(
        html.contains(&format!("api/v1/entries/{id}/attachments/")) && html.contains("results.csv"),
        "{html}"
    );
    assert!(!html.contains("api/v1/blobs/"), "{html}");
    assert!(j["warnings"].as_array().unwrap().is_empty(), "{j}");
    assert_eq!(j["entry"]["id"], id);
    assert_eq!(j["entry"]["kind"], "report");
    let files = j["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "{j}");
    let f = files.iter().find(|f| f["filename"] == "loss.png").unwrap();
    assert_eq!(f["sha256"], loss.sha256);
    assert_eq!(f["mime"], "image/png");
    assert_eq!(
        f["url"],
        format!("api/v1/entries/{id}/attachments/{}/loss.png", loss.sha256)
    );
    // повтор — из кэша, тот же HTML
    let r2 = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    assert_eq!(r2.json()["html"], j["html"]);

    // администратор от имени владельца — URL с `?user=` (браузер не шлёт X-Zam-User в <img>)
    let r =
        w.a.adm(
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html"),
            None,
            Some(&w.o.id),
        )
        .await;
    ok(&r, StatusCode::OK, "администратор");
    let html = r.json()["html"].as_str().unwrap().to_string();
    let src = format!(
        "src=\"api/v1/entries/{id}/attachments/{}/loss.png?user={}\"",
        loss.sha256, w.o.id
    );
    assert!(html.contains(&src), "{src} в {html}");
    let r =
        w.a.adm(
            Method::GET,
            &format!("{}?user={}", file_uri(&id, &loss), w.o.id),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "файл администратору по ?user=");
}

#[tokio::test]
async fn report_html_other_kind_400() {
    let w = world("html-kind").await;
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "changelog");
    let cl = r.json()["id"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{cl}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "html у changelog");
    // json — как раньше
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{cl}?format=json"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "json у changelog");
    assert_eq!(r.json()["id"], cl);
    let id = w.report("Отчёт").await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=pdf"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "неизвестный формат");
}

#[tokio::test]
async fn report_attachment_download_image_inline_svg_sandbox() {
    let w = world("dl-img").await;
    let id = w.report("Картинки").await;
    let loss = w.file(&id, "loss.png").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &loss), None).await;
    ok(&r, StatusCode::OK, "png");
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.header("content-disposition"), "inline");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("etag"), format!("\"{}\"", loss.sha256));
    assert!(r.header("content-security-policy").contains("sandbox"));
    assert_eq!(&r.body[..], &png(1, 200)[..]);
    // Range
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::GET,
        &file_uri(&id, &loss),
        None,
        &[("range", "bytes=0-3")],
    )
    .await;
    ok(&r, StatusCode::PARTIAL_CONTENT, "range");
    assert_eq!(&r.body[..], b"\x89PNG");
    // If-None-Match
    let etag = format!("\"{}\"", loss.sha256);
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::GET,
        &file_uri(&id, &loss),
        None,
        &[("if-none-match", &etag)],
    )
    .await;
    ok(&r, StatusCode::NOT_MODIFIED, "304");
    assert_eq!(r.header("x-content-type-options"), "nosniff");

    // SVG: тип image/svg+xml, песочница CSP — скрипты при прямом открытии не исполняются
    let svg = w.file(&id, "diag.svg").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &svg), None).await;
    ok(&r, StatusCode::OK, "svg");
    assert_eq!(r.header("content-type"), "image/svg+xml");
    let csp = r.header("content-security-policy");
    assert!(csp.starts_with("sandbox"), "{csp}");
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(!csp.contains("script-src"), "{csp}");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    // прямое открытие SVG — скачивание (как у вложений заметок M5; `<img>` грузит и так)
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename*=UTF-8''diag.svg"
    );
    assert_eq!(&r.body[..], SVG);
}

#[tokio::test]
async fn report_attachment_other_file_attachment_nosniff() {
    let w = world("dl-csv").await;
    let id = w.report("Данные").await;
    let csv = w.file(&id, "results.csv").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &csv), None).await;
    ok(&r, StatusCode::OK, "csv");
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename*=UTF-8''results.csv"
    );
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert!(r.header("content-security-policy").contains("sandbox"));
    assert_eq!(&r.body[..], CSV);
}

#[tokio::test]
async fn report_attachment_wrong_entry_404() {
    let w = world("dl-wrong").await;
    let a_id = w.report("Отчёт A").await;
    let other = png(9, 50);
    let b_id = w
        .report_with("Отчёт B", "![x](b.png)\n", &[("b.png", &other)])
        .await;
    let loss = w.file(&a_id, "loss.png").await;
    let nf = |r: &Resp, ctx: &str| ok(r, StatusCode::NOT_FOUND, ctx);
    // файл отчёта A через отчёт B
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&b_id, &loss), None).await;
    nf(&r, "sha отчёта A через B");
    // то же sha с чужим именем
    let uri = format!(
        "/api/v1/entries/{a_id}/attachments/{}/other.png",
        loss.sha256
    );
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "чужое имя",
    );
    // неизвестный sha
    let uri = format!(
        "/api/v1/entries/{a_id}/attachments/{}/loss.png",
        "0".repeat(64)
    );
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "нет sha",
    );
    // sha не hex — 400
    let uri = format!("/api/v1/entries/{a_id}/attachments/XYZ/loss.png");
    ok(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        StatusCode::BAD_REQUEST,
        "sha не hex",
    );

    // вложение заметки: ни через id заметки, ни через отчёт
    let note = w.note("notes/n1").await;
    let data = png(7, 80);
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{note}\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"n.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let r = Request::post("/api/v1/blobs")
        .header(header::AUTHORIZATION, format!("Bearer {}", w.o.tok))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let r = w.a.send(r).await;
    ok(&r, StatusCode::CREATED, "вложение заметки");
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/entries/{note}/attachments/{sha}/n.png");
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "id заметки",
    );
    let uri = format!("/api/v1/entries/{a_id}/attachments/{sha}/n.png");
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "блоб заметки через отчёт",
    );
    // блоб заметки по своему маршруту доступен, файл отчёта по нему — нет
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/blobs/{sha}/n.png"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "блоб заметки");
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/blobs/{}/loss.png", loss.sha256),
        None,
    )
    .await;
    nf(&r, "файл отчёта через /blobs");

    // маршрут файлов отчёта — только сессиям WebUI: токен получает 404
    let r =
        w.a.tok(&w.o.tok, Method::GET, &file_uri(&a_id, &loss), None)
            .await;
    nf(&r, "токен");
}

#[tokio::test]
async fn report_list_cookie_first_image_author_cursor() {
    let w = world("list").await;
    let first = w.report("Первый").await;
    let (p2, p3) = (png(2, 60), png(3, 70));
    let second = w
        .report_with(
            "Второй",
            "Текст [данные](d.csv)\n\n![a](a.png)\n\n![b](b.png)\n",
            &[("d.csv", CSV), ("a.png", &p2), ("b.png", &p3)],
        )
        .await;
    let third = w
        .report_with(
            "Третий",
            "Без картинок, только [файл](d.csv).\n",
            &[("d.csv", CSV)],
        )
        .await;
    // changelog в той же ленте без вида — отчётов там нет
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "changelog");
    let all = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries"),
        None,
    )
    .await;
    ok(&all, StatusCode::OK, "лента без вида");
    let kinds: Vec<Value> = all.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(kinds, vec![json!("changelog")]);

    let uri = format!("/api/v1/spaces/{G}/entries?kind=report&limit=2");
    let r = web(&w.a, &w.o.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "лента отчётов");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![third.as_str(), second.as_str()], "{j}");
    assert!(items[0]["first_image"].is_null(), "{j}");
    let a = w.file(&second, "a.png").await;
    let fi = &items[1]["first_image"];
    assert_eq!(fi["filename"], "a.png", "{j}");
    assert_eq!(fi["sha256"], a.sha256);
    assert_eq!(fi["mime"], "image/png");
    assert_eq!(
        fi["url"],
        format!("api/v1/entries/{second}/attachments/{}/a.png", a.sha256)
    );
    // серверная миниатюра (M6, задача 15a): лента берёт её вместо исходного файла
    assert_eq!(
        fi["thumb_url"],
        format!("api/v1/previews/{}?kind=thumb&px=256", a.sha256),
        "{j}"
    );
    for e in items {
        assert_eq!(e["kind"], "report");
        assert_eq!(e["author"]["kind"], "agent", "{e}");
        assert_eq!(e["author"]["session_id"], "sess-report-1", "{e}");
        assert_eq!(e["author"]["machine"], "gpu-box", "{e}");
        assert_eq!(e["tags"], json!(["ml"]));
        // тело ленте не нужно (задача 6): его читает страница `format=html`
        assert!(e.get("body").is_none(), "{e}");
    }
    let cursor = j["next_cursor"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("{uri}&cursor={}", common::api::enc(&cursor)),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "вторая страница");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["id"], first);
    assert_eq!(items[0]["first_image"]["filename"], "loss.png");
    assert!(j["next_cursor"].is_null(), "{j}");
    // участник ro видит ленту решения
    let r = web(&w.a, &w.ro.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "участник ro");
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 2);
    // `deleted` — только `only`
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?kind=report&deleted=yes"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "deleted=yes");
    // токену лента отчётов не существует: 400 тем же текстом, что у неизвестного вида
    let detail = |r: &Resp| r.json()["detail"].as_str().unwrap().to_string();
    let r = w.a.tok(&w.o.tok, Method::GET, &uri, None).await;
    ok(&r, StatusCode::BAD_REQUEST, "токен kind=report");
    let unknown =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/entries?kind=xyz"),
            None,
        )
        .await;
    assert_eq!(
        detail(&r).replace("report", "xyz"),
        detail(&unknown),
        "текст как у неизвестного вида"
    );
}

/// M6, задача 15a: `thumb_url` — только у растровой первой картинки (SVG для превью не
/// изображение — лента показывает сам файл); администратору от имени владельца — с `?user=`;
/// адрес из ленты отдаёт PNG сессии и 404 токену.
#[tokio::test]
async fn report_feed_thumb_url_raster_only_and_admin_user() {
    let w = world("feed-thumb").await;
    let svg_only = w
        .report_with("Только SVG", "![схема](diag.svg)\n", &[("diag.svg", SVG)])
        .await;
    let img = {
        use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
        let i = DynamicImage::ImageRgb8(RgbImage::from_fn(320, 160, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 9])
        }));
        let mut out = std::io::Cursor::new(Vec::new());
        i.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    };
    let raster = w
        .report_with("Растр", "![график](plot.png)\n", &[("plot.png", &img)])
        .await;
    let uri = format!("/api/v1/spaces/{G}/entries?kind=report");
    let r = web(&w.a, &w.o.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "лента");
    let j = r.json();
    let item = |id: &str| -> Value {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()
            .clone()
    };
    let s = item(&svg_only);
    assert_eq!(s["first_image"]["mime"], "image/svg+xml", "{s}");
    assert!(s["first_image"]["thumb_url"].is_null(), "{s}");
    assert!(
        s["first_image"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/diag.svg")
    );
    let p = item(&raster);
    let f = w.file(&raster, "plot.png").await;
    let thumb = p["first_image"]["thumb_url"].as_str().unwrap().to_string();
    assert_eq!(
        thumb,
        format!("api/v1/previews/{}?kind=thumb&px=256", f.sha256)
    );
    // адрес из ленты работает: сессии — PNG не больше 256, токену — 404
    let r = web(&w.a, &w.ro.web, Method::GET, &format!("/{thumb}"), None).await;
    ok(&r, StatusCode::OK, "миниатюра участнику");
    let png = image::load_from_memory_with_format(&r.body, image::ImageFormat::Png).unwrap();
    assert_eq!((png.width(), png.height()), (256, 128));
    assert!(r.body.len() < img.len(), "миниатюра меньше исходника");
    let r =
        w.a.tok(&w.o.tok, Method::GET, &format!("/{thumb}"), None)
            .await;
    ok(&r, StatusCode::NOT_FOUND, "миниатюра токену");
    // администратор от имени владельца: адрес с `&user=`, и он отдаёт PNG
    let r = w.a.adm(Method::GET, &uri, None, Some(&w.o.id)).await;
    ok(&r, StatusCode::OK, "лента администратору");
    let j = r.json();
    let p = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == raster.as_str())
        .unwrap();
    let thumb = p["first_image"]["thumb_url"].as_str().unwrap();
    assert_eq!(
        thumb,
        format!(
            "api/v1/previews/{}?kind=thumb&px=256&user={}",
            f.sha256, w.o.id
        )
    );
    let r = w.a.adm(Method::GET, &format!("/{thumb}"), None, None).await;
    ok(&r, StatusCode::OK, "миниатюра администратору по ?user=");
}

/// M6, задача 15a (§16 п. 15): страница отчёта — ссылки пяти схем с `href`, всем внешним —
/// `rel="noopener noreferrer"`; сырой HTML тела проверкой ссылок не виден, но и рендер его
/// не пропускает (ни ссылок, ни картинок из него на странице нет; санитайзер отчёта отдельно
/// проверен в `zam-markdown`); у заметки `vscode:`/`file:` по-прежнему без `href`.
#[tokio::test]
async fn report_html_link_schemes_rel_and_raw_html() {
    let w = world("html-links").await;
    let body = "# Ссылки\n\n[h](https://example.org/a) [p](http://example.org) \
        [m](mailto:a@example.org) [v](vscode://file/C:/proj/src/main.rs:10:5) \
        [f](file:///C:/data/report.csv) <file:///etc/hosts> [я](#ссылки)\n\n\
        <a href=\"javascript:alert(1)\">j</a> <a href=\"JaVaScRiPt:alert(2)\">J</a> \
        <a href=\"java&#9;script:alert(3)\">t</a> <a href=\" javascript:alert(4)\">s</a> \
        <a href=\"ftp://example.org/x\">ftp</a> <a href=\"data:text/html,x\">d</a> \
        <a href=\"vscode://file/raw.rs\">raw</a>\n\n\
        <img src=\"file:///C:/secret.png\" alt=\"i\"> <img src=\"vscode://file/x.png\" alt=\"k\"> \
        <video src=\"file:///C:/v.mp4\"></video>\n";
    let id = w.report_with("Ссылки", body, &[]).await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "страница");
    let html = r.json()["html"].as_str().unwrap().to_string();
    const REL: &str = "rel=\"noopener noreferrer\"";
    for url in [
        "https://example.org/a",
        "http://example.org",
        "mailto:a@example.org",
        "vscode://file/C:/proj/src/main.rs:10:5",
        "file:///C:/data/report.csv",
        "file:///etc/hosts",
    ] {
        let a = format!("href=\"{url}\" {REL}");
        assert!(html.contains(&a), "{a} в {html}");
    }
    // якорь — не внешняя ссылка
    assert!(!html.contains(&format!("\" {REL}>я<")), "{html}");
    // сырой HTML тела рендер не пропускает вовсе: ни ссылок, ни картинок, ни схем из него нет
    let lower = html.to_lowercase();
    for bad in [
        "javascript",
        "ftp:",
        "data:",
        "alert(",
        "src=\"file:",
        "src=\"vscode:",
        "secret.png",
        "v.mp4",
        "raw.rs",
        "<img",
        "<video",
    ] {
        assert!(!lower.contains(bad), "{bad} в {html}");
    }

    // заметка: рендер заметок не менялся — `vscode:` и `file:` без href
    let n =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "ссылки", "kind": "note",
                "content": "[v](vscode://file/x.rs) [f](file:///C:/a.txt) [h](https://example.org)\n"})),
        )
        .await;
    ok(&n, StatusCode::CREATED, "заметка");
    let nid = n.json()["id"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/nodes/{nid}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "заметка html");
    let html = r.json()["html"].as_str().unwrap().to_string();
    assert!(html.contains("href=\"https://example.org\""), "{html}");
    assert!(
        !html.contains("vscode:") && !html.contains("file:"),
        "{html}"
    );
}

/// Правки отчёта: (что, тело).
fn patch_bodies() -> Vec<(&'static str, Value)> {
    vec![
        ("title", json!({"title": "Новый"})),
        ("body", json!({"body": "Новый текст"})),
        ("append_body", json!({"append_body": "\nдописано"})),
        ("tags", json!({"tags": ["x"]})),
        ("area", json!({"area": "ml"})),
        ("area null", json!({"area": null})),
        ("data", json!({"data": {"refs": {"commits": ["abc"]}}})),
        ("refs", json!({"refs": {"files": [{"path": "src/a.rs"}]}})),
        ("certainty", json!({"certainty": "axiom", "reason": "r"})),
        ("status", json!({"status": "active"})),
        (
            "triggers",
            json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
        ),
        ("пусто", json!({})),
    ]
}

#[tokio::test]
async fn report_patch_any_field_409() {
    let w = world("patch").await;
    let id = w.report("Неизменный").await;
    let (rev0, body0) = w.stored(&id).await;
    assert_eq!(rev0, 1);
    let uri = format!("/api/v1/entries/{id}");
    for (what, body) in patch_bodies() {
        // с If-Match и без: неизменность — раньше ревизии
        for extra in [&[("if-match", "1")][..], &[][..]] {
            let r = web_h(
                &w.a,
                &w.o.web,
                Method::PATCH,
                &uri,
                Some(body.clone()),
                extra,
            )
            .await;
            ok(&r, StatusCode::CONFLICT, what);
            assert_eq!(problem(&r), IMMUTABLE, "{what}");
        }
        // администратор (своя сессия от имени владельца) — тоже 409
        let r =
            w.a.adm_h(
                Method::PATCH,
                &uri,
                Some(body.clone()),
                &[("x-zam-user", &w.o.id), ("if-match", "1")],
            )
            .await;
        ok(&r, StatusCode::CONFLICT, what);
        assert_eq!(problem(&r), IMMUTABLE, "{what}");
        // участник rw — 409; ro — 403 (права записи — раньше вида)
        let r = web_h(
            &w.a,
            &w.rw.web,
            Method::PATCH,
            &uri,
            Some(body.clone()),
            &[("if-match", "1")],
        )
        .await;
        ok(&r, StatusCode::CONFLICT, what);
        let r = web_h(
            &w.a,
            &w.ro.web,
            Method::PATCH,
            &uri,
            Some(body.clone()),
            &[("if-match", "1")],
        )
        .await;
        ok(&r, StatusCode::FORBIDDEN, what);
    }
    // токен не узнаёт об отчёте по 409: 404, как у несуществующей записи
    let r =
        w.a.tok_h(
            &w.o.tok,
            Method::PATCH,
            &uri,
            Some(json!({"title": "x"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND, "токен");
    // токен `ro` — тоже 404 (вид проверяется раньше прав записи, задача 4)
    let ro = w.a.issue(&w.o.id, "ro").await;
    let r =
        w.a.tok_h(
            &ro,
            Method::PATCH,
            &uri,
            Some(json!({"title": "x"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND, "токен ro");
    let (rev, body) = w.stored(&id).await;
    assert_eq!((rev, body), (rev0, body0));
}

#[tokio::test]
async fn report_other_mutations_409() {
    let w = world("mutations").await;
    let id = w.report("Неизменный").await;
    let (rev0, body0) = w.stored(&id).await;
    let cases: Vec<(&str, String, Value)> = vec![
        (
            "supersede",
            format!("/api/v1/entries/{id}/supersede"),
            json!({"title": "Новый", "body": "т", "data": {"kind": "gotcha"}}),
        ),
        (
            "retract",
            format!("/api/v1/entries/{id}/retract"),
            json!({"reason": "неверно"}),
        ),
        (
            "answer",
            format!("/api/v1/entries/{id}/answer"),
            json!({"text": "да"}),
        ),
        ("claim", format!("/api/v1/entries/{id}/claim"), json!({})),
        (
            "release",
            format!("/api/v1/entries/{id}/release"),
            json!({}),
        ),
        (
            "feedback",
            format!("/api/v1/entries/{id}/feedback"),
            json!({"value": "useful"}),
        ),
    ];
    for (what, uri, body) in &cases {
        let r = web_h(
            &w.a,
            &w.o.web,
            Method::POST,
            uri,
            Some(body.clone()),
            &[("x-zam-session", "sess-ui")],
        )
        .await;
        ok(&r, StatusCode::CONFLICT, what);
        assert_eq!(problem(&r), IMMUTABLE, "{what}");
        // токен владельца — 404 (существование отчёта не раскрывается)
        let r =
            w.a.tok_h(
                &w.o.tok,
                Method::POST,
                uri,
                Some(body.clone()),
                &[("x-zam-session", "sess-agent")],
            )
            .await;
        ok(&r, StatusCode::NOT_FOUND, &format!("{what} по токену"));
    }
    // замена триггеров
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(json!({"triggers": [{"type": "keyword", "pattern": "x"}]})),
        &[("if-match", "1")],
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "triggers");

    // рёбра: от отчёта — 409, к отчёту — 422
    let note = w.note("notes/n1").await;
    let edge = |src: String, dst: String| json!({"src": src, "type": "relates_to", "dst": dst});
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/edges",
        Some(edge(format!("e:{id}"), format!("n:{note}"))),
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "ребро от отчёта");
    assert_eq!(problem(&r), IMMUTABLE);
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/edges",
        Some(edge(format!("n:{note}"), format!("e:{id}"))),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "ребро к отчёту");
    assert!(
        r.text().contains("на отчёт нельзя ссылаться"),
        "{}",
        r.text()
    );
    // токен: оба конца — 404
    for (src, dst) in [
        (format!("e:{id}"), format!("n:{note}")),
        (format!("n:{note}"), format!("e:{id}")),
    ] {
        let r =
            w.a.tok(
                &w.o.tok,
                Method::POST,
                "/api/v1/edges",
                Some(edge(src, dst)),
            )
            .await;
        ok(&r, StatusCode::NOT_FOUND, "ребро по токену");
    }
    let (rev, body) = w.stored(&id).await;
    assert_eq!((rev, body), (rev0, body0));
    let usage: i64 = w
        .db()
        .await
        .read(|c| Ok(c.query_row("SELECT count(*) FROM usage", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(usage, 0, "оценка отчёта не записана");
}

/// Ответ удаления: 204 или текст отказа.
async fn delete_as(w: &W, who: &str, id: &str) -> Resp {
    let uri = format!("/api/v1/entries/{id}");
    let ifm = [("if-match", "1")];
    match who {
        "owner" => web_h(&w.a, &w.o.web, Method::DELETE, &uri, None, &ifm).await,
        "rw" => web_h(&w.a, &w.rw.web, Method::DELETE, &uri, None, &ifm).await,
        "ro" => web_h(&w.a, &w.ro.web, Method::DELETE, &uri, None, &ifm).await,
        "owner-token" => w.a.tok_h(&w.o.tok, Method::DELETE, &uri, None, &ifm).await,
        "member-token" => w.a.tok_h(&w.rw.tok, Method::DELETE, &uri, None, &ifm).await,
        "admin-for" => {
            w.a.adm_h(
                Method::DELETE,
                &uri,
                None,
                &[("x-zam-user", &w.o.id), ("if-match", "1")],
            )
            .await
        }
        _ => unreachable!(),
    }
}

async fn restore_as(w: &W, who: &str, id: &str) -> Resp {
    let uri = format!("/api/v1/entries/{id}/restore");
    match who {
        "owner" => web(&w.a, &w.o.web, Method::POST, &uri, None).await,
        "rw" => web(&w.a, &w.rw.web, Method::POST, &uri, None).await,
        "ro" => web(&w.a, &w.ro.web, Method::POST, &uri, None).await,
        "owner-token" => w.a.tok(&w.o.tok, Method::POST, &uri, None).await,
        "member-token" => w.a.tok(&w.rw.tok, Method::POST, &uri, None).await,
        "admin-for" => {
            w.a.adm_h(Method::POST, &uri, None, &[("x-zam-user", &w.o.id)])
                .await
        }
        _ => unreachable!(),
    }
}

const NOT_OWNERS: [&str; 5] = ["rw", "ro", "owner-token", "member-token", "admin-for"];

/// Отказ не владельцу: сессиям WebUI — 403, токену — 404 (отчёт агенту не существует, §7;
/// решение задачи 4: 403 выдавал бы существование).
fn denied(who: &str) -> StatusCode {
    if who.ends_with("-token") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::FORBIDDEN
    }
}

#[tokio::test]
async fn report_delete_restore_owner_only() {
    let w = world("trash").await;
    let id = w.report("Удаляемый").await;
    for who in NOT_OWNERS {
        let r = delete_as(&w, who, &id).await;
        ok(&r, denied(who), &format!("удаление: {who}"));
        if denied(who) == StatusCode::FORBIDDEN && who != "ro" {
            // участник ro отбивается правами записи раньше
            assert!(
                r.text().contains("только владелец решения"),
                "{who}: {}",
                r.text()
            );
        }
    }
    let e = get_entry(&w.db().await, id.parse().unwrap()).await.unwrap();
    assert!(e.deleted_at.is_none());
    ok(
        &delete_as(&w, "owner", &id).await,
        StatusCode::NO_CONTENT,
        "владелец удаляет",
    );
    for who in NOT_OWNERS {
        let r = restore_as(&w, who, &id).await;
        ok(&r, denied(who), &format!("восстановление: {who}"));
    }
    let r = restore_as(&w, "owner", &id).await;
    ok(&r, StatusCode::OK, "владелец восстанавливает");
    assert!(r.json()["deleted_at"].is_null());

    // администратор в своей базе — владелец своего решения
    let g2 = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99";
    let r =
        w.a.t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": g2, "name": "Своё"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение администратора");
    let r =
        w.a.t(
            Method::POST,
            &format!("/api/v1/spaces/{g2}/entries"),
            Some(json!({"kind": "report", "title": "Отчёт администратора"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "отчёт администратора");
    let own = r.json()["id"].as_str().unwrap().to_string();
    let r =
        w.a.adm_h(
            Method::DELETE,
            &format!("/api/v1/entries/{own}"),
            None,
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT, "администратор удаляет свой");
    let r =
        w.a.adm(
            Method::POST,
            &format!("/api/v1/entries/{own}/restore"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "администратор восстанавливает свой");
    // а его токен — нет (404: отчёт токену не существует)
    let r =
        w.a.tok_h(
            &w.a.token,
            Method::DELETE,
            &format!("/api/v1/entries/{own}"),
            None,
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND, "токен администратора");

    // прочие записи по-прежнему удаляет участник rw
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    let cl = r.json()["id"].as_str().unwrap().to_string();
    ok(
        &delete_as(&w, "rw", &cl).await,
        StatusCode::NO_CONTENT,
        "changelog участником",
    );
}

#[tokio::test]
async fn report_deleted_feed_owner_only_files_404() {
    let w = world("trash-feed").await;
    let id = w.report("В корзину").await;
    let live = w.report("Живой").await;
    let loss = w.file(&id, "loss.png").await;
    ok(
        &delete_as(&w, "owner", &id).await,
        StatusCode::NO_CONTENT,
        "удаление",
    );
    let trash = format!("/api/v1/spaces/{G}/entries?kind=report&deleted=only");
    let r = web(&w.a, &w.o.web, Method::GET, &trash, None).await;
    ok(&r, StatusCode::OK, "корзина владельца");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["id"], id);
    assert!(!items[0]["deleted_at"].is_null());
    // файлы удалённого не отдаются — миниатюры в корзине нет
    assert!(items[0]["first_image"].is_null(), "{j}");
    for (who, web_) in [("rw", &w.rw.web), ("ro", &w.ro.web)] {
        let r = web(&w.a, web_, Method::GET, &trash, None).await;
        ok(&r, StatusCode::FORBIDDEN, &format!("корзина: {who}"));
    }
    let r = w.a.adm(Method::GET, &trash, None, Some(&w.o.id)).await;
    ok(&r, StatusCode::FORBIDDEN, "корзина: администратор от имени");
    // `deleted=only` — только у ленты отчётов
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?deleted=only"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "deleted=only без вида");
    // живая лента — без удалённого
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?kind=report"),
        None,
    )
    .await;
    let ids: Vec<Value> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(live)]);
    // страница и файлы удалённого — 404
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::NOT_FOUND, "страница удалённого");
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &loss), None).await;
    ok(&r, StatusCode::NOT_FOUND, "файл удалённого");
}

#[tokio::test]
async fn report_member_ro_reads_html_and_files() {
    let w = world("member-ro").await;
    let id = w.report("Для участника").await;
    let loss = w.file(&id, "loss.png").await;
    for (who, u) in [("ro", &w.ro), ("rw", &w.rw)] {
        let r = web(
            &w.a,
            &u.web,
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html"),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("страница: {who}"));
        let html = r.json()["html"].as_str().unwrap().to_string();
        let src = format!(
            "src=\"api/v1/entries/{id}/attachments/{}/loss.png\"",
            loss.sha256
        );
        assert!(html.contains(&src), "{who}: {html}");
        assert!(!html.contains("?user="), "{who}: {html}");
        let r = web(&w.a, &u.web, Method::GET, &file_uri(&id, &loss), None).await;
        ok(&r, StatusCode::OK, &format!("файл: {who}"));
        assert_eq!(&r.body[..], &png(1, 200)[..]);
        let csv = w.file(&id, "results.csv").await;
        let r = web(&w.a, &u.web, Method::GET, &file_uri(&id, &csv), None).await;
        ok(&r, StatusCode::OK, &format!("csv: {who}"));
    }
}

/// Ревью задачи 3: курсор корзины (`deleted=only`, порядок по `deleted_at`) при `limit=1` и
/// двух удалённых отчётах — две страницы по одному, без повторов.
#[tokio::test]
async fn report_trash_cursor_limit_1() {
    let w = world("trash-cursor").await;
    let first = w.report("Первый в корзину").await;
    let second = w.report("Второй в корзину").await;
    let _live = w.report("Живой").await;
    for id in [&first, &second] {
        ok(
            &delete_as(&w, "owner", id).await,
            StatusCode::NO_CONTENT,
            "удаление",
        );
    }
    let uri = format!("/api/v1/spaces/{G}/entries?kind=report&deleted=only&limit=1");
    let r = web(&w.a, &w.o.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "страница 1");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    // удалён последним — первым
    assert_eq!(items[0]["id"], second, "{j}");
    let cursor = j["next_cursor"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("{uri}&cursor={}", common::api::enc(&cursor)),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "страница 2");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["id"], first, "{j}");
    if let Some(c) = j["next_cursor"].as_str() {
        let r = web(
            &w.a,
            &w.o.web,
            Method::GET,
            &format!("{uri}&cursor={}", common::api::enc(c)),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, "страница 3");
        assert!(
            r.json()["items"].as_array().unwrap().is_empty(),
            "{}",
            r.text()
        );
    }
}

/// Ревью задачи 3: `url` файла из ответа страницы (кириллица и пробел в имени) скачивается
/// сквозным запросом.
#[tokio::test]
async fn report_file_url_cyrillic_space_end_to_end() {
    let w = world("file-url").await;
    let p = png(8, 64);
    let id = w
        .report_with(
            "Файлы",
            "![график](<графики/потери модели.png>)\n",
            &[("графики/потери модели.png", &p)],
        )
        .await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "страница");
    let j = r.json();
    let url = j["files"][0]["url"].as_str().unwrap().to_string();
    assert!(!url.contains(' '), "{url}");
    assert!(j["html"].as_str().unwrap().contains(&url), "{j}");
    let r = web(&w.a, &w.o.web, Method::GET, &format!("/{url}"), None).await;
    ok(&r, StatusCode::OK, &format!("скачивание {url}"));
    assert_eq!(&r.body[..], &p[..]);
    assert_eq!(r.header("content-type"), "image/png");
}

/// Ревью задачи 3: `?user=` от обычного пользователя игнорируется (только администратор
/// действует от имени другого): страница и файл — из его собственной области.
#[tokio::test]
async fn report_user_param_ignored_for_plain_user() {
    let w = world("user-param").await;
    let id = w.report("Чужой user").await;
    let loss = w.file(&id, "loss.png").await;
    for (who, u, other) in [("владелец", &w.o, &w.rw.id), ("участник", &w.ro, &w.o.id)]
    {
        let r = web(
            &w.a,
            &u.web,
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html&user={other}"),
            None,
        )
        .await;
        // не «от имени»: страница — своя (у `other` отчёта нет), адреса файлов — без `?user=`
        ok(&r, StatusCode::OK, &format!("{who}: страница с ?user="));
        assert!(!r.text().contains("?user="), "{who}: {}", r.text());
        let r = web(
            &w.a,
            &u.web,
            Method::GET,
            &format!("{}?user={other}", file_uri(&id, &loss)),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("{who}: файл с ?user="));
        assert_eq!(&r.body[..], &png(1, 200)[..]);
    }
}
