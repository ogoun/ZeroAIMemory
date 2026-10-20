//! Эмбеддер на тестовой модели MiniLM (CPU-сборка ORT), sparse-голова, ошибки загрузки.
//! Вся программа тестов работает с одной библиотекой ORT: она грузится один раз на процесс.

mod common;

use std::time::Instant;

use common::*;
use zam_markdown::TokenCounter;
use zam_ml::{Device, DevicePref, Embedder, MlError, SparseHead};

const MINILM: &str = "minilm-l6-int8";

fn minilm() -> Option<Embedder> {
    if !have(&ort_cpu(), MINILM) {
        return None;
    }
    let cfg = config(ort_cpu(), models_dir(), DevicePref::Cpu);
    let (e, rep) = Embedder::load(&cfg, MINILM).expect("загрузка MiniLM");
    assert_eq!(rep.device, Device::Cpu);
    Some(e)
}

/// Каталог модели с описателем и пустыми файлами (для ошибок до запуска ORT).
fn fake_model(name: &str) -> std::path::PathBuf {
    let root = tmp_dir(name);
    let dir = root.join("fake");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("model.toml"),
        "id = \"fake\"\ndim = 4\npooling = \"mean\"\nmax_tokens_passage = 16\nmax_tokens_query = 8\n",
    )
    .unwrap();
    std::fs::write(dir.join("model.onnx"), b"").unwrap();
    std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
    root
}

#[test]
fn missing_runtime_is_no_runtime() {
    let root = fake_model("no-runtime");
    let ort = root.join("нет").join(ort_name());
    let cfg = config(ort, root, DevicePref::Auto);
    match Embedder::load(&cfg, "fake") {
        Err(MlError::NoRuntime(why)) => assert!(!why.is_empty()),
        Err(e) => panic!("ожидался NoRuntime, получено {e:?}"),
        Ok(_) => panic!("ожидался NoRuntime"),
    }
    // init_runtime на несуществующем пути — тоже NoRuntime, без паники
    assert!(matches!(
        zam_ml::init_runtime(&tmp_dir("no-runtime-2").join(ort_name())),
        Err(MlError::NoRuntime(_))
    ));
}

#[test]
fn missing_model_is_no_model() {
    let root = tmp_dir("no-model");
    let cfg = config(ort_cpu(), root.clone(), DevicePref::Cpu);
    assert!(matches!(
        Embedder::load(&cfg, "нет-такой"),
        Err(MlError::NoModel(_))
    ));
    // каталог есть, файла модели нет
    let dir = root.join("partial");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("model.toml"),
        "id = \"partial\"\ndim = 4\npooling = \"cls\"\nmax_tokens_passage = 16\nmax_tokens_query = 8\n",
    )
    .unwrap();
    assert!(matches!(
        Embedder::load(&cfg, "partial"),
        Err(MlError::NoModel(_))
    ));
}

#[test]
fn bad_spec_is_spec_error() {
    let root = tmp_dir("bad-spec");
    let dir = root.join("bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.toml"), "id = \"bad\"\npooling = \"max\"\n").unwrap();
    let cfg = config(ort_cpu(), root, DevicePref::Cpu);
    assert!(matches!(Embedder::load(&cfg, "bad"), Err(MlError::Spec(_))));
}

#[test]
fn minilm_dim_norm_and_model_id() {
    let Some(e) = minilm() else { return };
    assert_eq!(e.dim(), 384);
    let id = e.model_id();
    let (name, sha) = id.split_once('@').expect("формат <id>@<sha8>");
    assert_eq!(name, MINILM);
    assert_eq!(sha.len(), 8);
    assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
    let v = e.embed_query("Bash scripts on Windows").unwrap();
    assert_eq!(v.dense.len(), 384);
    assert!((norm(&v.dense) - 1.0).abs() < 1e-4);
    assert!(v.sparse.is_empty(), "у MiniLM нет sparse-головы");
    let p = e.embed_passages(&["one", "two words"]).unwrap();
    for x in &p {
        assert!((norm(&x.dense) - 1.0).abs() < 1e-4);
    }
    assert!(e.embed_passages(&[]).unwrap().is_empty());
}

#[test]
fn minilm_semantic_order() {
    let Some(e) = minilm() else { return };
    let q = e
        .embed_query("How do I write bash scripts on Windows?")
        .unwrap();
    let p = e
        .embed_passages(&[
            "Bash scripts on Windows need LF line endings",
            "Rust borrow checker rules",
        ])
        .unwrap();
    let near = cosine(&q.dense, &p[0].dense);
    let far = cosine(&q.dense, &p[1].dense);
    assert!(near > far, "near {near} <= far {far}");
}

/// 17 текстов разной длины (от 1 до 113 слов).
fn texts17() -> Vec<String> {
    let words = "the quick brown fox jumps over the lazy dog while bash scripts need LF endings";
    let w: Vec<&str> = words.split(' ').collect();
    (0..17)
        .map(|i| {
            (0..(1 + i * 7))
                .map(|k| w[(k * 3 + i) % w.len()])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Копия MiniLM (см. `common::minilm_copy`), загруженная на CPU-сборке ORT.
fn minilm_copy(name: &str, extra: &str) -> Option<Embedder> {
    if !have(&ort_cpu(), MINILM) {
        return None;
    }
    let root = common::minilm_copy(name, extra);
    let cfg = config(ort_cpu(), root, DevicePref::Cpu);
    Some(Embedder::load(&cfg, name).unwrap().0)
}

#[test]
fn minilm_batch_matches_single() {
    // MiniLM int8 с dynamic_quant: тексты идут по одному, батч не влияет на вектор
    let Some(e) = minilm() else { return };
    assert!(e.spec().dynamic_quant);
    let texts = texts17();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let batch = e.embed_passages(&refs).unwrap();
    assert_eq!(batch.len(), 17);
    for (t, b) in refs.iter().zip(&batch) {
        let single = &e.embed_passages(&[t]).unwrap()[0];
        let c = cosine(&single.dense, &b.dense);
        assert!(c >= 0.9999, "косинус {c} для текста длиной {}", t.len());
    }
}

#[test]
fn multi_row_batches_keep_order() {
    // та же модель, объявленная без dynamic_quant: настоящие батчи с паддингом до кратного 8.
    // Динамическое квантование даёт косинус ≈ 0,99 к одиночному прогону, но каждый вектор
    // батча ближе всего к своему тексту — порядок, паддинг и маска собраны верно.
    let Some(e) = minilm_copy("minilm-batched", "") else {
        return;
    };
    let texts = texts17();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let batch = e.embed_passages(&refs).unwrap();
    let singles: Vec<_> = refs
        .iter()
        .map(|t| e.embed_passages(&[t]).unwrap().remove(0))
        .collect();
    for (i, b) in batch.iter().enumerate() {
        assert!((norm(&b.dense) - 1.0).abs() < 1e-4);
        let own = cosine(&singles[i].dense, &b.dense);
        assert!(own >= 0.97, "текст {i}: косинус {own}");
        for (j, s) in singles.iter().enumerate() {
            if j != i {
                assert!(cosine(&s.dense, &b.dense) < own, "текст {i} ближе к {j}");
            }
        }
    }
}

#[test]
fn minilm_query_cache_is_stable() {
    let Some(e) = minilm() else { return };
    let a = e.embed_query("cache me").unwrap();
    let b = e.embed_query("cache me").unwrap();
    assert_eq!(a.dense, b.dense);
}

#[test]
fn token_counter_has_no_special_tokens() {
    let Some(e) = minilm() else { return };
    // WordPiece: "hello world" — два токена, [CLS]/[SEP] не считаются
    assert_eq!(e.count_tokens("hello world"), 2);
    assert_eq!(e.count_tokens(""), 0);
    let c = e.token_counter();
    assert!(c.is_exact());
    assert_eq!(c.count("hello world"), 2);
    // аддитивность на границе слов (чанкер складывает счётчики частей)
    assert_eq!(
        c.count("hello world hello"),
        c.count("hello world") + c.count("hello")
    );
    // длинный текст не усекается (в tokenizer.json MiniLM зашито усечение до 128)
    let long = "word ".repeat(500);
    assert_eq!(c.count(&long), 500);
    // счётчик без ORT: только tokenizer.json
    let c2 = zam_ml::BgeTokenCounter::load(&models_dir(), MINILM).unwrap();
    assert_eq!(c2.count(&long), 500);
}

#[test]
fn sparse_head_synthetic() {
    let dir = tmp_dir("sparse-head");
    let p = dir.join("sparse_linear.pt");
    // w = (1, -1, 0.5, 0), b = 0.25
    write_sparse_pt(&p, &[1.0, -1.0, 0.5, 0.0], 0.25);
    let head = SparseHead::load(&p, 4).unwrap();
    // h0 = (1,0,0,0) → 1.25; h1 = (0,1,0,0) → relu(-0.75) = 0; h2 = (0,0,2,0) → 1.25;
    // h3 = (0.5,0,0,0) → 0.75 (тот же id, что h0, берётся max); h4 — спецтокен; h5 — маска 0;
    // h6 → 5e-5 < 1e-4 — отбрасывается
    let hidden: Vec<f32> = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 2.0, 0.0],
        [0.5, 0.0, 0.0, 0.0],
        [9.0, 0.0, 0.0, 0.0],
        [9.0, 0.0, 0.0, 0.0],
        [0.0, 0.24995, 0.0, 0.0],
    ]
    .concat();
    let ids = [10u32, 11, 7, 10, 2, 12, 13];
    let mask = [1u32, 1, 1, 1, 1, 0, 1];
    let sp = head.pool(&hidden, &ids, &mask);
    assert_eq!(sp.len(), 2, "{sp:?}");
    assert_eq!(sp[0].0, 7);
    assert!((sp[0].1 - 1.25).abs() < 1e-3);
    assert_eq!(sp[1].0, 10);
    assert!((sp[1].1 - 1.25).abs() < 1e-3);
    // размерность не совпала → ошибка Spec
    assert!(matches!(SparseHead::load(&p, 8), Err(MlError::Spec(_))));
}

#[test]
fn sparse_head_through_embedder() {
    // копия MiniLM со случайной sparse-головой
    let Some(e) = minilm_copy(
        "minilm-sparse",
        "sparse_head = \"sparse_linear.pt\"
",
    ) else {
        return;
    };
    let v = e
        .embed_passages(&["sparse lexical weights for bash scripts"])
        .unwrap();
    let sp = &v[0].sparse;
    assert!(!sp.is_empty());
    assert!(
        sp.windows(2).all(|p| p[0].0 < p[1].0),
        "отсортировано по id без повторов"
    );
    assert!(sp.iter().all(|&(id, x)| id > 3 && x >= 1e-4));
    assert!(!e.embed_query("sparse lexical").unwrap().sparse.is_empty());
}

/// bge-m3 int8 на CPU: размерность, sparse, счётчик, RSS и скорость (100 чанков по ~400 токенов).
/// Запуск: `cargo test --release -p zam-ml -- --ignored bge_m3_smoke --nocapture`.
#[test]
#[ignore = "нужна bge-m3 (cargo xtask fetch-models --id bge-m3-int8), долго"]
fn bge_m3_smoke() {
    const BGE: &str = "bge-m3-int8";
    if !have(&ort_cpu(), BGE) {
        return;
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut cfg = config(ort_cpu(), models_dir(), DevicePref::Cpu);
    cfg.index_threads = threads;
    let rss0 = rss_bytes();
    let t = Instant::now();
    let (e, rep) = Embedder::load(&cfg, BGE).unwrap();
    let load = t.elapsed();
    let rss1 = rss_bytes();
    assert_eq!(rep.device, Device::Cpu);
    assert_eq!(e.dim(), 1024);
    assert!(e.count_tokens("привет мир") > 0);
    let q = e
        .embed_query("как настроить окончания строк LF в bash на Windows?")
        .unwrap();
    assert_eq!(q.dense.len(), 1024);
    assert!((norm(&q.dense) - 1.0).abs() < 1e-4);
    assert!(!q.sparse.is_empty());
    assert!(q.sparse.iter().all(|&(id, _)| id > 3));

    // 100 чанков по ~400 токенов (ru/en/код)
    let base = [
        "Скрипты bash в Windows должны сохраняться с окончаниями строк LF, иначе интерпретатор видит лишний символ \\r. ",
        "The borrow checker enforces that references never outlive the data they point to. ",
        "fn main() { let v: Vec<u32> = (0..10).collect(); println!(\"{:?}\", v); } ",
        "Qdrant хранит плотные и разреженные векторы; запись идёт с wait=true. ",
    ];
    let mut chunks = Vec::new();
    for i in 0..100 {
        let mut s = format!("Чанк {i}. ");
        let mut k = i;
        while e.count_tokens(&s) < 400 {
            s.push_str(base[k % base.len()]);
            k += 1;
        }
        chunks.push(s);
    }
    let toks: usize = chunks.iter().map(|c| e.count_tokens(c)).sum();
    let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    // прогрев
    e.embed_passages(&refs[..4]).unwrap();
    let t = Instant::now();
    let out = e.embed_passages(&refs).unwrap();
    let dt = t.elapsed();
    let rss2 = rss_bytes();
    assert_eq!(out.len(), 100);
    assert!(out.iter().all(|x| !x.sparse.is_empty()));
    // запросы
    let t = Instant::now();
    for i in 0..20 {
        e.embed_query(&format!("запрос номер {i} про bash и LF"))
            .unwrap();
    }
    let qdt = t.elapsed() / 20;
    let mb = |x: Option<u64>| x.map_or(-1.0, |b| b as f64 / 1048576.0);
    eprintln!(
        "bge-m3 int8 CPU ({threads} потоков): загрузка {load:?}; RSS до {:.0} МБ, после загрузки {:.0} МБ, \
         после 100 чанков {:.0} МБ; 100 чанков ({toks} токенов) за {dt:?} = {:.2} чанка/с; запрос {qdt:?}",
        mb(rss0),
        mb(rss1),
        mb(rss2),
        100.0 / dt.as_secs_f64()
    );
}

/// zip-файл с заданными записями (без сжатия).
fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    use std::io::Write as _;
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opt =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        z.start_file(*name, opt).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

#[test]
fn corrupt_sparse_head_is_spec_error() {
    let dir = tmp_dir("sparse-corrupt");
    let spec = |p: &std::path::Path| match SparseHead::load(p, 4) {
        Err(MlError::Spec(m)) => m,
        other => panic!("{}: ожидался Spec, получено {other:?}", p.display()),
    };
    let w = [0u8; 8];
    let b = [0u8; 2];
    // не zip
    let p = dir.join("not-zip.pt");
    std::fs::write(&p, b"not a zip at all").unwrap();
    spec(&p);
    // обрезанный файл
    let good = dir.join("good.pt");
    write_sparse_pt(&good, &[1.0, 2.0, 3.0, 4.0], 0.5);
    let bytes = std::fs::read(&good).unwrap();
    let p = dir.join("truncated.pt");
    std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
    spec(&p);
    // нет записи */data/0 и две записи */data/0
    let p = dir.join("none.pt");
    write_zip(&p, &[("s/data/1", &b)]);
    assert!(spec(&p).contains("найдено 0"));
    let p = dir.join("two.pt");
    write_zip(&p, &[("a/data/0", &w), ("b/data/0", &w), ("a/data/1", &b)]);
    assert!(spec(&p).contains("найдено 2"));
    // запись не того размера
    let p = dir.join("short.pt");
    write_zip(&p, &[("s/data/0", &w[..6]), ("s/data/1", &b)]);
    spec(&p);
    // заголовок с огромным размером (центральный каталог правится вручную): ни паники, ни OOM
    let mut bytes = std::fs::read(&good).unwrap();
    let name = b"sparse_linear/data/0";
    let cd = (0..bytes.len() - 46)
        .find(|&i| bytes[i..i + 4] == *b"PK\x01\x02" && bytes[i + 46..].starts_with(name))
        .expect("запись центрального каталога");
    bytes[cd + 24..cd + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    let p = dir.join("huge.pt");
    std::fs::write(&p, &bytes).unwrap();
    spec(&p);
}

#[test]
fn sparse_pool_tolerates_inconsistent_lengths() {
    let dir = tmp_dir("sparse-lengths");
    let p = dir.join("h.pt");
    write_sparse_pt(&p, &[1.0, 0.0], 0.0);
    let head = SparseHead::load(&p, 2).unwrap();
    // hidden на 1 токен, ids на 3, mask на 2 — берётся общая часть (1 токен)
    let sp = head.pool(&[2.0, 0.0, 5.0], &[10, 11, 12], &[1, 1]);
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].0, 10);
    assert!(head.pool(&[], &[10], &[1]).is_empty());
}

#[test]
fn empty_token_sequence_is_spec_error() {
    if !have(&ort_cpu(), MINILM) {
        return;
    }
    // токенизатор без пост-процессора: пустой текст даёт пустую последовательность
    let root = common::minilm_copy("minilm-nopost", "");
    let tj = root.join("minilm-nopost").join("tokenizer.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tj).unwrap()).unwrap();
    v["post_processor"] = serde_json::Value::Null;
    std::fs::write(&tj, serde_json::to_string(&v).unwrap()).unwrap();
    let (e, _) =
        Embedder::load(&config(ort_cpu(), root, DevicePref::Cpu), "minilm-nopost").unwrap();
    assert!(matches!(e.embed_passages(&[""]), Err(MlError::Spec(_))));
    assert!(matches!(e.embed_query(""), Err(MlError::Spec(_))));
    assert!(matches!(
        e.embed_passages(&["ok", ""]),
        Err(MlError::Spec(_))
    ));
    assert_eq!(e.embed_passages(&["ok"]).unwrap().len(), 1);
}

#[test]
fn huge_texts_are_capped_before_tokenization() {
    let Some(e) = minilm() else { return };
    let huge = "word ".repeat(1_000_000); // 5 МБ
    let t = Instant::now();
    let q = e.embed_query(&huge).unwrap();
    let p = e.embed_passages(&[&huge]).unwrap().remove(0);
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    // запрос усекается до 128 токенов: префикс 2048 байт даёт тот же вектор
    let prefix = &huge[..2048];
    let (e2, _) =
        Embedder::load(&config(ort_cpu(), models_dir(), DevicePref::Cpu), MINILM).unwrap();
    assert!(cosine(&q.dense, &e2.embed_query(prefix).unwrap().dense) > 0.9999);
    assert!((norm(&p.dense) - 1.0).abs() < 1e-4);
    // кириллица: обрезка по границе символа, без паники
    e.embed_query(&"я".repeat(100_000)).unwrap();
}

#[test]
fn relative_missing_runtime_is_no_runtime() {
    let root = fake_model("rel-runtime");
    let cfg = config(
        std::path::PathBuf::from("нет-такого").join(ort_name()),
        root,
        DevicePref::Auto,
    );
    assert!(matches!(
        Embedder::load(&cfg, "fake"),
        Err(MlError::NoRuntime(_))
    ));
}

/// Уступка индексации запросам (задача 15 M2): пока запрос в инференсе (слот занят),
/// `embed_passages` ждёт; ожидание учитывается отдельно (`yield_wait`), а не как инференс.
/// Без запросов уступки нет. `embed_query` держит счётчик запросов только на время
/// инференса.
#[test]
fn passages_yield_to_running_query_and_query_holds_counter() {
    let Some(e) = minilm() else { return };
    let texts = ["первый текст для индексации", "second passage to embed"];
    // без запросов — уступки нет
    let y0 = e.yield_wait();
    e.embed_passages(&texts).unwrap();
    assert_eq!(e.yield_wait(), y0, "уступка без запросов");
    // запрос «в работе» 60 мс: индексация ждёт его конца, ожидание — в yield_wait
    let waited = std::thread::scope(|sc| {
        let slot = e.hold_query_slot();
        assert_eq!(e.queries_in_flight(), 1);
        sc.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(slot);
        });
        let t = std::time::Instant::now();
        e.embed_passages(&["третий текст, пока идёт запрос"])
            .unwrap();
        t.elapsed()
    });
    let y = e.yield_wait() - y0;
    assert!(waited >= std::time::Duration::from_millis(50), "{waited:?}");
    assert!(
        y >= std::time::Duration::from_millis(40),
        "yield_wait {y:?}"
    );
    assert_eq!(e.queries_in_flight(), 0);
    // embed_query держит счётчик во время инференса: наблюдатель видит его ненулевым
    let seen = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                seen.fetch_max(e.queries_in_flight(), std::sync::atomic::Ordering::Relaxed);
            }
        });
        for i in 0..200 {
            // разные тексты — мимо кэша запросов
            e.embed_query(&format!("запрос номер {i}")).unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
    });
    assert!(
        seen.load(std::sync::atomic::Ordering::Relaxed) >= 1,
        "счётчик не наблюдался"
    );
    assert_eq!(e.queries_in_flight(), 0, "счётчик освобождён");
}
