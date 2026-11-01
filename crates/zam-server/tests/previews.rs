//! Превью вложений (M5, задача 8): `GET /api/v1/previews/{sha256}?kind=sheet|thumb|text`.
//!
//! Лист (`SheetPage` по номеру листа, имя с секретом — маской), миниатюры PDF (pdfium из
//! `target/deps`; без него часть PDF пропускается) и изображений, текст слайдов PPTX и
//! подсветка кода; права — как у скачивания блоба, недоступное неотличимо от несуществующего;
//! кэш в `previews/<владелец>/<sha>/` (атомарная запись, ключ — параметры и версии, ограничение
//! размера); враждебные изображения и попытки обхода пути. Сервер без моделей; файлы
//! разбираются в рабочих процессах превью.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::attach::{pptx, upload, upload_adm, wait_done, xlsx};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use zam_docs::test_pdf::{Page, build, pdfium_path};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e92";
const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

async fn world(name: &str) -> (Api, bool) {
    world_with(name, |_| {}).await
}

async fn world_with(
    name: &str,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> (Api, bool) {
    let pdfium = pdfium_path(env!("CARGO_MANIFEST_DIR"));
    let pdf = pdfium.is_some();
    let a = api(name, |c| {
        c.docs.pdfium_dylib = pdfium;
        tweak(c);
    })
    .await;
    (a, pdf)
}

async fn note(a: &Api, token: &str, space: &str, path: &str) -> String {
    let r = a
        .tok(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note",
                        "content": "# Заметка\n\nТекст.\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

fn uri(sha: &str, q: &str) -> String {
    format!("/api/v1/previews/{sha}?{q}")
}

async fn get(a: &Api, token: &str, sha: &str, q: &str) -> Resp {
    a.tok(token, Method::GET, &uri(sha, q), None).await
}

async fn ok_json(a: &Api, sha: &str, q: &str) -> Value {
    let r = get(a, &a.token, sha, q).await;
    assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
    r.json()
}

fn sha_of(v: &Value) -> String {
    v["sha256"].as_str().unwrap().to_string()
}

fn png(img: &DynamicImage, f: ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, f).unwrap();
    out.into_inner()
}

fn gradient(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 7])
    }))
}

fn dims(b: &[u8]) -> (u32, u32) {
    let i = image::load_from_memory_with_format(b, ImageFormat::Png).unwrap();
    (i.width(), i.height())
}

/// Заголовки безопасности и кэширования ответа превью.
fn check_headers(r: &Resp, ctype: &str) {
    assert!(r.header("content-type").starts_with(ctype), "{ctype}");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    let cc = r.header("cache-control");
    assert!(cc.starts_with("private") && cc.contains("max-age="), "{cc}");
    let csp = r.header("content-security-policy");
    assert!(
        csp.contains("sandbox") && csp.contains("default-src 'none'"),
        "{csp}"
    );
    assert!(r.header("etag").starts_with('"'));
}

/// Файлы каталога `dir` (рекурсивно).
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Ждёт `n` файлов кэша в `dir` (запись кэша — фоновая).
async fn wait_files(dir: &Path, n: usize) -> Vec<PathBuf> {
    let t = Instant::now();
    loop {
        let f = files_under(dir);
        if f.len() >= n || t.elapsed() > Duration::from_secs(20) {
            assert_eq!(f.len(), n, "{f:?}");
            return f;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn previews_dir(a: &Api) -> PathBuf {
    a.st.data_dir.previews_dir()
}

/// Лист постранично: окно строк, следующая страница, лист по номеру, имя листа с секретом —
/// маской (номер `sheet:#N` — как в `/read` и поиске), неверные параметры — 400.
#[tokio::test]
async fn preview_sheet_paging() {
    let (a, _) = world("prev-sheet").await;
    let owner = a.st.admin_id;
    let n = note(&a, &a.token, "knowledge", "p/таблица").await;
    let rows: Vec<Vec<String>> = std::iter::once(vec!["Имя".to_string(), "Сумма".into()])
        .chain((1..=50).map(|i| vec![format!("строка {i}"), format!("{}", i * 10)]))
        .collect();
    let refs: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    let rr: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    let book = xlsx(&[
        ("Итоги", rr.as_slice()),
        (SECRET, &[&["ключ"], &["значение"]]),
    ]);
    let sha = sha_of(&upload(&a, &a.token, &n, "book.xlsx", &book).await);
    // номер листа с секретом выводится из чанков документа: ждём извлечения
    wait_done(&a, owner, &n, "book.xlsx").await;

    let p = ok_json(&a, &sha, "kind=sheet&rows=10").await;
    assert_eq!(p["kind"], "sheet");
    assert_eq!(p["sheet"], 1);
    assert_eq!(p["name"], "Итоги");
    assert_eq!(p["header"], json!(["Имя", "Сумма"]));
    assert_eq!(p["first_row"], 2);
    assert_eq!(p["total_rows"], 50);
    assert_eq!(p["cols"], 2);
    assert_eq!(p["rows"].as_array().unwrap().len(), 10);
    assert_eq!(p["rows"][0], json!(["строка 1", "10"]));
    assert_eq!(p["sheets"][0]["no"], 1);
    assert_eq!(p["sheets"][0]["name"], "Итоги");
    assert_eq!(p["sheets"][0]["secret"], false);
    // вторая страница
    let p2 = ok_json(&a, &sha, "kind=sheet&sheet=1&from=12&rows=10").await;
    assert_eq!(p2["first_row"], 12);
    assert_eq!(p2["rows"][0], json!(["строка 11", "110"]));
    assert_eq!(p2["rows"].as_array().unwrap().len(), 10);
    // последняя неполная
    let p3 = ok_json(&a, &sha, "kind=sheet&from=48&rows=10").await;
    assert_eq!(p3["rows"].as_array().unwrap().len(), 4);
    // лист с секретом: по номеру, имя — маска, в ответе нет секрета
    let r = get(&a, &a.token, &sha, "kind=sheet&sheet=2").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(SECRET), "{}", r.text());
    let s = r.json();
    assert_eq!(s["sheet"], 2);
    assert_eq!(s["sheets"][1]["secret"], true);
    assert_eq!(s["sheets"][1]["ref_no"], 2, "{s}");
    assert_eq!(s["name"], s["sheets"][1]["name"]);
    assert_eq!(s["rows"], json!([["значение"]]));
    check_headers(&r, "application/json");
    // нет листа — 422; неверные параметры — 400
    let r = get(&a, &a.token, &sha, "kind=sheet&sheet=3").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    for q in [
        "kind=sheet&rows=0",
        "kind=sheet&rows=100000",
        "kind=sheet&sheet=0",
        "kind=sheet&sheet=Итоги",
        "kind=sheet&from=-1",
        "kind=bogus",
        "",
        "kind=thumb&px=0",
        "kind=thumb&px=1025",
        "kind=thumb&page=0",
    ] {
        let r = get(&a, &a.token, &sha, q).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
    }
    let r = get(&a, &a.token, &sha.to_uppercase(), "kind=sheet").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // лист у не-таблицы — 422
    let doc = sha_of(&upload(&a, &a.token, &n, "s.pptx", &pptx(&[("Слайд", "текст")])).await);
    let r = get(&a, &a.token, &doc, "kind=sheet").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

/// ТЗ §17: первый экран листа XLSX на 10 000 строк — не дольше 1 с (холодный кэш, первый запуск
/// рабочего процесса превью). Порог 1 с — для release; в debug — 15 с (разбор без оптимизаций в
/// разы медленнее; ловит только патологию). Замер в release:
/// `cargo test -p zam-server --release --test previews -- sheet_10k --nocapture`.
#[tokio::test]
async fn preview_sheet_10k_first_screen_under_1s() {
    let (a, _) = world("prev-10k").await;
    let n = note(&a, &a.token, "knowledge", "p/большая").await;
    let mut rows: Vec<Vec<String>> = vec![(0..8).map(|c| format!("Столбец {c}")).collect()];
    for r in 2..=10_001 {
        rows.push((0..8).map(|c| format!("{}", r * 10 + c)).collect());
    }
    let refs: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    let rr: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    let sha = sha_of(
        &upload(
            &a,
            &a.token,
            &n,
            "big.xlsx",
            &xlsx(&[("Данные", rr.as_slice())]),
        )
        .await,
    );
    let t = Instant::now();
    let p = ok_json(&a, &sha, "kind=sheet&rows=100").await;
    let cold = t.elapsed();
    let t = Instant::now();
    let again = ok_json(&a, &sha, "kind=sheet&rows=100").await;
    let warm = t.elapsed();
    let t = Instant::now();
    let deep = ok_json(&a, &sha, "kind=sheet&from=9000&rows=100").await;
    let other = t.elapsed();
    println!("превью 10k×8: холодный {cold:?}, из кэша {warm:?}, другое окно {other:?}");
    assert_eq!(p["total_rows"], 10_000);
    assert_eq!(p["rows"].as_array().unwrap().len(), 100);
    assert_eq!(p, again);
    assert_eq!(deep["rows"][0][0], "90000");
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(1)
    };
    assert!(cold <= limit, "первый экран {cold:?} > {limit:?}");
    assert!(other <= limit, "окно {other:?} > {limit:?}");
}

/// Миниатюры: PDF (страница, pdfium) и изображения PNG/JPEG/GIF/WebP — PNG не больше `px`;
/// враждебные изображения (бомба размеров) и не-изображение — 422, сервер работает.
#[tokio::test]
async fn preview_thumb_pdf_and_image() {
    let (a, pdf) = world("prev-thumb").await;
    let n = note(&a, &a.token, "knowledge", "p/картинки").await;
    for (name, f) in [
        ("a.png", ImageFormat::Png),
        ("a.jpg", ImageFormat::Jpeg),
        ("a.gif", ImageFormat::Gif),
        ("a.webp", ImageFormat::WebP),
    ] {
        let sha = sha_of(&upload(&a, &a.token, &n, name, &png(&gradient(400, 200), f)).await);
        let r = get(&a, &a.token, &sha, "kind=thumb&px=64").await;
        assert_eq!(r.status, StatusCode::OK, "{name}: {}", r.text());
        check_headers(&r, "image/png");
        assert_eq!(dims(&r.body), (64, 32), "{name}");
        // по умолчанию — 256
        let r = get(&a, &a.token, &sha, "kind=thumb").await;
        assert_eq!(dims(&r.body), (256, 128), "{name}");
    }
    // бомба размеров: PNG 100000×100000 в сотне байт
    let mut bomb = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&100_000u32.to_be_bytes());
    ihdr.extend_from_slice(&100_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    bomb.extend_from_slice(&13u32.to_be_bytes());
    bomb.extend_from_slice(&ihdr);
    bomb.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    bomb.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    let bomb = sha_of(&upload(&a, &a.token, &n, "bomb.png", &bomb).await);
    let t = Instant::now();
    let r = get(&a, &a.token, &bomb, "kind=thumb&px=128").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    // не изображение: SVG (сценарий) и текст
    let svg = sha_of(
        &upload(
            &a,
            &a.token,
            &n,
            "x.svg",
            b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>",
        )
        .await,
    );
    let r = get(&a, &a.token, &svg, "kind=thumb").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // сервер жив: следующая миниатюра строится
    let ok = sha_of(
        &upload(
            &a,
            &a.token,
            &n,
            "b.png",
            &png(&gradient(10, 10), ImageFormat::Png),
        )
        .await,
    );
    let r = get(&a, &a.token, &ok, "kind=thumb&px=1024").await;
    assert_eq!(dims(&r.body), (10, 10));
    if !pdf {
        eprintln!("pdfium не найден (cargo xtask fetch-deps) — миниатюра PDF пропущена");
        return;
    }
    let doc = build(&[
        Page::default().line(72.0, 700.0, "Первая страница"),
        Page::default().line(72.0, 700.0, "Вторая страница"),
    ]);
    let sha = sha_of(&upload(&a, &a.token, &n, "d.pdf", &doc).await);
    let r = get(&a, &a.token, &sha, "kind=thumb&page=2&px=100").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    check_headers(&r, "image/png");
    let (w, h) = dims(&r.body);
    assert!(w.max(h) <= 100 && w.max(h) >= 90, "{w}×{h}");
    let r = get(&a, &a.token, &sha, "kind=thumb&page=3").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Текст: PPTX — слайды с заголовками; код — HTML syntect (только классы, разметка из файла
/// экранирована); простой текст; двоичный файл — 422.
#[tokio::test]
async fn preview_text_pptx_and_code() {
    let (a, _) = world("prev-text").await;
    let n = note(&a, &a.token, "knowledge", "p/тексты").await;
    let deck = sha_of(
        &upload(
            &a,
            &a.token,
            &n,
            "deck.pptx",
            &pptx(&[("Введение", "первый слайд"), ("Итоги", "второй слайд")]),
        )
        .await,
    );
    let r = get(&a, &a.token, &deck, "kind=text").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    check_headers(&r, "application/json");
    let j = r.json();
    assert_eq!(j["format"], "slides");
    let slides = j["slides"].as_array().unwrap();
    assert_eq!(slides.len(), 2, "{j}");
    assert_eq!(slides[0]["slide"], 1);
    assert!(
        slides[1]["text"].as_str().unwrap().contains("второй слайд"),
        "{j}"
    );

    let code = "fn main() {\n    println!(\"<script>alert('x')</script>\");\n}\n";
    let rs = sha_of(&upload(&a, &a.token, &n, "main.rs", code.as_bytes()).await);
    let j = ok_json(&a, &rs, "kind=text").await;
    assert_eq!(j["format"], "html");
    assert_eq!(j["syntax"], "Rust");
    let html = j["html"].as_str().unwrap();
    assert!(html.contains("hl-"), "{html}");
    assert!(
        !html.contains("<script") && html.contains("&lt;script&gt;"),
        "{html}"
    );
    // единственные теги — `span` с классом: ни атрибутов-обработчиков, ни стилей
    let tags: Vec<&str> = html
        .split('<')
        .skip(1)
        .map(|t| t.split('>').next().unwrap())
        .collect();
    assert!(
        tags.iter()
            .all(|t| *t == "/span"
                || (t.starts_with("span class=\"") && t.matches('=').count() == 1)),
        "{tags:?}"
    );
    assert_eq!(j["truncated"], false);

    let yaml = sha_of(&upload(&a, &a.token, &n, "c.yaml", b"a: 1\nb: [2, 3]\n").await);
    assert!(ok_json(&a, &yaml, "kind=text").await["syntax"].is_string());
    let txt = sha_of(
        &upload(
            &a,
            &a.token,
            &n,
            "readme",
            "просто <текст> & всё".as_bytes(),
        )
        .await,
    );
    let j = ok_json(&a, &txt, "kind=text").await;
    assert_eq!(j["html"], "просто &lt;текст&gt; &amp; всё");
    // большой текст обрезается, ответ ограничен
    let big = "строка текста\n".repeat(200_000);
    let bsha = sha_of(&upload(&a, &a.token, &n, "big.log", big.as_bytes()).await);
    let j = ok_json(&a, &bsha, "kind=text").await;
    assert_eq!(j["truncated"], true);
    assert!(j["html"].as_str().unwrap().len() < 4 * 1024 * 1024);
    let bin = sha_of(&upload(&a, &a.token, &n, "x.bin", &[0u8, 1, 2, 0, 255, 0]).await);
    let r = get(&a, &a.token, &bin, "kind=text").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

/// Права — как у скачивания блоба: Human по токену, Knowledge владельца участнику решения,
/// посторонний, неодобренный запрос доступа — ответ тот же, что у несуществующего sha
/// (статус и тело); участник видит вложение пространства решения, администратор — Human.
#[tokio::test]
async fn preview_access_denied_for_foreign() {
    let (a, _) = world("prev-access").await;
    let tok = a.token.clone();
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_c_id, c_tok) = a.new_user("Вера").await;
    let (_d_id, d_tok) = a.new_user("Глеб").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // Глеб запросил доступ, но его не одобрили
    let r = a
        .tok(&d_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());

    let k = note(&a, &tok, "knowledge", "p/скрытая").await;
    let s = note(&a, &tok, G, "p/общая").await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "p/дневник", "kind": "note",
                        "content": "# Д\n\nД.\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h = r.json()["id"].as_str().unwrap().to_string();
    let img = |c: u8| {
        png(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(20, 20, Rgb([c, c, c]))),
            ImageFormat::Png,
        )
    };
    let sk = sha_of(&upload(&a, &tok, &k, "k.png", &img(1)).await);
    let ss = sha_of(&upload(&a, &tok, &s, "s.png", &img(2)).await);
    let sh = sha_of(&upload_adm(&a, &h, "h.png", &img(3)).await);
    let missing = "0".repeat(64);
    let q = "kind=thumb&px=16";
    let base = get(&a, &tok, &missing, q).await;
    assert_eq!(base.status, StatusCode::NOT_FOUND, "{}", base.text());
    let cases = [
        (&tok, &sh, "Human по токену"),
        (&b_tok, &sk, "Knowledge владельца участнику"),
        (&b_tok, &sh, "Human владельца участнику"),
        (&c_tok, &sk, "постороннему Knowledge"),
        (&c_tok, &ss, "постороннему решение"),
        (&d_tok, &ss, "неодобренному решение"),
    ];
    for (who, sha, what) in cases {
        for kind in [q, "kind=sheet", "kind=text"] {
            let r = get(&a, who, sha, kind).await;
            let m = get(&a, who, &missing, kind).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "{what} {kind}: {}",
                r.text()
            );
            assert_eq!(r.body, m.body, "{what} {kind}: тело отличается");
        }
    }
    // доступное — читается
    let r = get(&a, &b_tok, &ss, q).await;
    assert_eq!(r.status, StatusCode::OK, "участник: {}", r.text());
    let r = a.adm(Method::GET, &uri(&sh, q), None, None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "администратор Human: {}",
        r.text()
    );
    let r = get(&a, &tok, &sk, q).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Кэш: второй запрос берёт файл из `previews/<владелец>/<sha>/` (подмена файла кэша видна в
/// ответе), `If-None-Match` — 304; ключ различает параметры; имена файлов кэша — только hex,
/// цифры и вид, путь из запроса в них не попадает (обход пути — 400).
#[tokio::test]
async fn preview_cache_reused() {
    let (a, _) = world("prev-cache").await;
    let owner = a.st.admin_id;
    let n = note(&a, &a.token, "knowledge", "p/кэш").await;
    let book = xlsx(&[("Лист", &[&["a"], &["1"], &["2"]])]);
    let sha = sha_of(&upload(&a, &a.token, &n, "c.xlsx", &book).await);
    let first = get(&a, &a.token, &sha, "kind=sheet").await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let dir = previews_dir(&a).join(owner.to_string()).join(&sha);
    let files = wait_files(&dir, 1).await;
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        name.starts_with("sheet-1-") && name.ends_with(".json"),
        "{name}"
    );
    assert!(
        name.contains(&format!("-v{}.", zam_docs::EXTRACTOR_VERSION)),
        "версия извлечения в ключе: {name}"
    );
    // подмена содержимого кэша видна во втором ответе: файл не разбирается заново
    let mut cached: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
    cached["header"] = json!(["ИЗ_КЭША"]);
    std::fs::write(&files[0], serde_json::to_vec(&cached).unwrap()).unwrap();
    let second = ok_json(&a, &sha, "kind=sheet").await;
    assert_eq!(second["header"], json!(["ИЗ_КЭША"]));
    // ETag → 304 без тела
    let etag = first.header("etag").to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri(&sha, "kind=sheet"),
            None,
            &[("if-none-match", &etag)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());
    assert_eq!(r.header("etag"), etag);
    // другие параметры — другой ключ и другой ETag
    let r = get(&a, &a.token, &sha, "kind=sheet&rows=1").await;
    assert_ne!(r.header("etag"), etag);
    wait_files(&dir, 2).await;
    // обход пути через параметры и sha невозможен: только числа и hex
    for q in [
        "kind=sheet&sheet=..%2F..%2Fx",
        "kind=sheet&sheet=1%2F..",
        "kind=..%2Fsheet",
        "kind=sheet&from=1e3",
    ] {
        let r = get(&a, &a.token, &sha, q).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
    }
    let r = a
        .tok(
            &a.token,
            Method::GET,
            "/api/v1/previews/..%2F..%2Fusers?kind=sheet",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // все файлы кэша — под каталогом превью, имена из разрешённых символов
    for f in files_under(&previews_dir(&a)) {
        let rel = f.strip_prefix(previews_dir(&a)).unwrap();
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(parts.len(), 3, "{rel:?}");
        assert_eq!(parts[0], owner.to_string());
        assert_eq!(parts[1], sha);
        assert!(
            parts[2]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-.".contains(c)),
            "{rel:?}"
        );
    }
}

/// Размер кэша ограничен `docs.preview_cache_mb`: старые файлы вытесняются.
#[tokio::test]
async fn preview_cache_bounded() {
    let (a, _) = world_with("prev-cache-cap", |c| c.docs.preview_cache_mb = 1).await;
    let n = note(&a, &a.token, "knowledge", "p/много").await;
    // шум сжимается плохо: миниатюра 256² ≈ 200 КиБ, восемь — больше предела
    let noisy = |seed: u32| {
        let mut s = seed.wrapping_mul(2_654_435_761);
        DynamicImage::ImageRgb8(RgbImage::from_fn(512, 512, |_, _| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            Rgb([s as u8, (s >> 8) as u8, (s >> 16) as u8])
        }))
    };
    for i in 0..8 {
        let sha = sha_of(
            &upload(
                &a,
                &a.token,
                &n,
                &format!("n{i}.png"),
                &png(&noisy(i + 1), ImageFormat::Png),
            )
            .await,
        );
        let r = get(&a, &a.token, &sha, "kind=thumb&px=256").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    // вытеснение — фоновое: ждём
    let total = || -> u64 {
        files_under(&previews_dir(&a))
            .iter()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum()
    };
    let t = Instant::now();
    while total() > 1024 * 1024 && t.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(total() <= 1024 * 1024, "кэш {} байт", total());
    assert!(total() > 0);
}
