//! `index_bench` — замер векторного индекса для ADR 0001 (задача 16 M2): managed Qdrant
//! (процесс под [`Supervisor`], как в сервере) против встроенного Qdrant Edge
//! (`EdgeIndex`, feature `edge`). Запускает `cargo xtask bench --edge`: каждая команда — в
//! отдельном процессе, чтобы RSS одного варианта не смешивался с другим.
//!
//! Команды (аргументы `--ключ значение`; итог — одна строка JSON в stdout):
//! - `sparse --tokenizer <tokenizer.json> --corpus <eval/corpus> --out <файл>` — окна корпуса
//!   по 200 токенов bge-m3 → sparse-векторы (вес растёт с IDF токена; спецтокены отброшены);
//! - `build --backend managed|edge --dir <каталог> --sparse <файл> [--points N] [--dim D]
//!   [--threads T] [--qdrant <бинарник>]` — полная пересборка: пустая коллекция, upsert
//!   пачками по 256 до 8 одновременно (как `reindex`), ожидание конца индексации (managed —
//!   статус коллекции green; Edge — `optimize()`);
//! - `search --backend … --dir … --sparse … [--queries Q] [--warmup W]` — открытие готового
//!   индекса, прогрев, Q гибридных запросов подряд; задержки, RSS, «находит ли себя».
//!
//! Данные синтетические и детерминированные: dense — смесь 1024 кластеров на единичной
//! сфере (точка = центр + шум, нормировано), sparse — окна корпуса с выпадением 20 %
//! токенов и разбросом весов; payload — как у сервера (тенант, 4 пространства, пути, уровни).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde_json::{Value, json};
use uuid::Uuid;
use zam_core::Id;
use zam_index::supervisor::{ManagedConfig, QdrantState, Supervisor, SupervisorConfig, rss_bytes};
use zam_index::{
    COLLECTION, IndexPoint, PointPayload, QdrantIndex, VectorFilter, VectorHit, VectorIndex,
};

const MODEL: &str = "bench/synthetic@1";
const BATCH: usize = 256;
const PARALLEL: usize = 8;
const CLUSTERS: usize = 1024;
/// Шум точки относительно центра кластера (косинус соседей по кластеру ≈ 0,5).
const POINT_NOISE: f32 = 1.0;
/// Шум запроса относительно целевой точки (косинус с целью ≈ 0,9).
const QUERY_NOISE: f32 = 0.35;
const SPACES: u128 = 4;
const WINDOW_TOKENS: usize = 200;
const QUERY_TOKENS: usize = 12;
/// Сколько первых запросов сохраняют выдачу для сверки вариантов между собой.
const KEEP_HITS: usize = 200;

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() -> Res<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZAM_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut argv = std::env::args().skip(1);
    let cmd = argv.next().ok_or("нет команды: sparse | build | search")?;
    let args = Args::parse(argv.collect())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let out = match cmd.as_str() {
        "sparse" => cmd_sparse(&args)?,
        "build" => rt.block_on(cmd_build(&args))?,
        "search" => rt.block_on(cmd_search(&args))?,
        other => return Err(format!("неизвестная команда {other}").into()),
    };
    println!("{out}");
    Ok(())
}

// ---------------------------------------------------------------- аргументы

struct Args(HashMap<String, String>);

impl Args {
    fn parse(v: Vec<String>) -> Res<Args> {
        let mut m = HashMap::new();
        let mut it = v.into_iter();
        while let Some(k) = it.next() {
            let key = k
                .strip_prefix("--")
                .ok_or(format!("ожидался --ключ: {k}"))?;
            let val = it.next().ok_or(format!("нет значения у --{key}"))?;
            m.insert(key.to_string(), val);
        }
        Ok(Args(m))
    }

    fn str(&self, k: &str) -> Res<&str> {
        self.0
            .get(k)
            .map(String::as_str)
            .ok_or_else(|| format!("нужен --{k}").into())
    }

    fn path(&self, k: &str) -> Res<PathBuf> {
        Ok(PathBuf::from(self.str(k)?))
    }

    fn num(&self, k: &str, default: usize) -> Res<usize> {
        match self.0.get(k) {
            Some(v) => Ok(v.parse()?),
            None => Ok(default),
        }
    }
}

// ---------------------------------------------------------------- детерминированные данные

fn mix(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Поток псевдослучайных чисел от зерна.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = mix(self.0);
        self.0
    }

    fn unit(&mut self) -> f32 {
        ((self.next() >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }

    /// Нормальное N(0, 1) (Бокс — Мюллер).
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.unit(), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|a| *a /= n);
    v
}

struct Data {
    dim: usize,
    centroids: Vec<Vec<f32>>,
    windows: Vec<Vec<(u32, f32)>>,
    user: Id,
}

impl Data {
    fn new(dim: usize, windows: Vec<Vec<(u32, f32)>>) -> Data {
        let centroids = (0..CLUSTERS)
            .map(|c| {
                let mut r = Rng(mix(0xC0FF_EE00 + c as u64));
                normalize((0..dim).map(|_| r.normal()).collect())
            })
            .collect();
        Data {
            dim,
            centroids,
            windows,
            user: Id::from(Uuid::from_u128(0x7a6d_0000_0000_0000_0000_0000_0000_0001)),
        }
    }

    fn space(i: u128) -> Id {
        Id::from(Uuid::from_u128(
            0x7a6d_5000_0000_0000_0000_0000_0000_0000 + i,
        ))
    }

    fn spaces() -> Vec<Id> {
        (0..SPACES).map(Data::space).collect()
    }

    /// Плотный вектор точки `i`, возмущённый шумом `noise` (для запросов — от той же точки).
    fn dense(&self, i: u64, extra: Option<(u64, f32)>) -> Vec<f32> {
        let c = &self.centroids[(mix(i) % CLUSTERS as u64) as usize];
        let s = POINT_NOISE / (self.dim as f32).sqrt();
        let mut r = Rng(mix(i ^ 0xD15E_A5E0));
        let mut v: Vec<f32> = c.iter().map(|x| x + s * r.normal()).collect();
        v = normalize(v);
        if let Some((seed, noise)) = extra {
            let s = noise / (self.dim as f32).sqrt();
            let mut r = Rng(mix(seed));
            v = normalize(v.iter().map(|x| x + s * r.normal()).collect());
        }
        v
    }

    fn sparse(&self, i: u64) -> Vec<(u32, f32)> {
        let w = &self.windows[(mix(i ^ 0x5EED) % self.windows.len() as u64) as usize];
        let mut r = Rng(mix(i ^ 0xA11CE));
        w.iter()
            .filter_map(|&(t, wt)| {
                let keep = r.unit() >= 0.2;
                let jitter = 0.8 + 0.4 * r.unit();
                keep.then_some((t, wt * jitter))
            })
            .collect()
    }

    fn query_sparse(&self, i: u64, seed: u64) -> Vec<(u32, f32)> {
        let mut s = self.sparse(i);
        let mut r = Rng(mix(seed));
        // случайные QUERY_TOKENS токенов окна
        for k in 0..s.len().min(QUERY_TOKENS) {
            let j = k + (r.next() as usize) % (s.len() - k);
            s.swap(k, j);
        }
        s.truncate(QUERY_TOKENS);
        s
    }

    fn level(i: u64) -> &'static str {
        match i % 20 {
            0 => "note",
            1 | 2 => "section",
            _ => "chunk",
        }
    }

    fn path(i: u64) -> [String; 3] {
        let h = mix(i ^ 0xFA7);
        let a = format!("p{}", h % 8);
        let b = format!("{a}/q{}", (h >> 8) % 8);
        let c = format!("{b}/r{}", (h >> 16) % 8);
        [a, b, c]
    }

    fn point(&self, i: u64) -> IndexPoint {
        IndexPoint {
            chunk_id: format!("c{i}"),
            dense: self.dense(i, None),
            sparse: self.sparse(i),
            payload: PointPayload {
                user_id: self.user,
                space_id: Data::space(u128::from(i) % SPACES),
                space_kind: "knowledge".into(),
                solution_guid: None,
                node_id: Id::from(Uuid::from_u128(
                    0x7a6d_4000_0000_0000_0000_0000_0000_0000 + u128::from(i / 4),
                )),
                path_ancestors: Data::path(i).to_vec(),
                area: None,
                level: Data::level(i).into(),
                kind: "note".into(),
                tags: vec!["bench".into()],
                updated_at: 1_760_000_000 + i as i64,
                rev: 1,
                text_hash: format!("h{i}"),
            },
        }
    }
}

fn read_windows(path: &Path) -> Res<Vec<Vec<(u32, f32)>>> {
    let raw = std::fs::read(path)?;
    let mut p = 0usize;
    let mut u32_at = |raw: &[u8]| -> Res<u32> {
        let b: [u8; 4] = raw.get(p..p + 4).ok_or("файл sparse обрезан")?.try_into()?;
        p += 4;
        Ok(u32::from_le_bytes(b))
    };
    let n = u32_at(&raw)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let k = u32_at(&raw)? as usize;
        let mut w = Vec::with_capacity(k);
        for _ in 0..k {
            let t = u32_at(&raw)?;
            let v = f32::from_bits(u32_at(&raw)?);
            w.push((t, v));
        }
        out.push(w);
    }
    if out.is_empty() {
        return Err("в файле sparse нет окон".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- sparse из корпуса

fn cmd_sparse(a: &Args) -> Res<Value> {
    let tok = tokenizers::Tokenizer::from_file(a.path("tokenizer")?)
        .map_err(|e| format!("токенизатор: {e}"))?;
    let mut files = Vec::new();
    collect_md(&a.path("corpus")?, &mut files)?;
    files.sort();
    // Окна по WINDOW_TOKENS токенов без спецтокенов (XLM-R: 0..=3 — <s>, <pad>, </s>, <unk>).
    let mut windows: Vec<Vec<u32>> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        let enc = tok
            .encode(text.as_str(), false)
            .map_err(|e| format!("токенизация: {e}"))?;
        let ids: Vec<u32> = enc.get_ids().iter().copied().filter(|&t| t > 3).collect();
        for w in ids.chunks(WINDOW_TOKENS) {
            if w.len() >= 20 {
                windows.push(w.to_vec());
            }
        }
    }
    // Документная частота токенов по окнам → IDF.
    let mut df: HashMap<u32, u32> = HashMap::new();
    for w in &windows {
        let mut u = w.clone();
        u.sort_unstable();
        u.dedup();
        for t in u {
            *df.entry(t).or_default() += 1;
        }
    }
    let n = windows.len() as f32;
    let idf_max = (n + 1.0).ln();
    let mut nnz = 0usize;
    let mut buf = Vec::new();
    buf.extend_from_slice(&(windows.len() as u32).to_le_bytes());
    for w in &windows {
        // Вес токена как у bge-m3 по форме: один вес на уникальный токен, 0,02–0,35.
        let mut u = w.clone();
        u.sort_unstable();
        u.dedup();
        nnz += u.len();
        buf.extend_from_slice(&(u.len() as u32).to_le_bytes());
        for t in u {
            let idf = ((n + 1.0) / (df[&t] as f32 + 0.5)).ln().max(0.0);
            let wt = 0.02 + 0.33 * (idf / idf_max);
            buf.extend_from_slice(&t.to_le_bytes());
            buf.extend_from_slice(&wt.to_bits().to_le_bytes());
        }
    }
    let out = a.path("out")?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, buf)?;
    Ok(json!({
        "files": files.len(),
        "windows": windows.len(),
        "vocab": df.len(),
        "mean_nnz": nnz as f64 / windows.len().max(1) as f64,
    }))
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) -> Res<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            collect_md(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- открытие вариантов

/// Открытый индекс варианта и то, что держит его живым.
struct Opened {
    idx: Arc<dyn VectorIndex>,
    /// managed: супервизор процесса Qdrant и клиент для статуса коллекции.
    managed: Option<(Supervisor, qdrant_client::Qdrant)>,
    #[cfg(feature = "edge")]
    edge: Option<Arc<zam_index::EdgeIndex>>,
}

impl Opened {
    fn qdrant_pid(&self) -> Option<u32> {
        self.managed.as_ref().and_then(|(s, _)| s.pid())
    }

    async fn close(self) {
        let Opened { idx, managed, .. } = self;
        drop(idx);
        if let Some((sup, client)) = managed {
            drop(client);
            sup.shutdown().await;
        }
        // Edge закрывается при освобождении последней ссылки (flush в Drop шарда).
    }
}

fn free_port() -> Res<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

async fn open(a: &Args, dir: &Path) -> Res<Opened> {
    let threads = a.num("threads", 2)?;
    match a.str("backend")? {
        "managed" => {
            let mut cfg = ManagedConfig::new(a.path("qdrant")?);
            cfg.http_port = free_port()?;
            cfg.grpc_port = free_port()?;
            cfg.optimizer_cpu_budget = threads as u32;
            cfg.ready_timeout = Duration::from_secs(1800);
            let sup = Supervisor::start(SupervisorConfig::Managed(cfg), dir).await?;
            let mut st = sup.state();
            let t = Instant::now();
            loop {
                match &*st.borrow_and_update() {
                    QdrantState::Ready { .. } => break,
                    QdrantState::Broken { reason } => {
                        return Err(format!("Qdrant не запустился: {reason}").into());
                    }
                    _ => {}
                }
                if t.elapsed() > Duration::from_secs(1800) {
                    return Err("Qdrant не готов за 30 мин".into());
                }
                let _ = tokio::time::timeout(Duration::from_millis(200), st.changed()).await;
            }
            let key = sup.api_key();
            let idx = QdrantIndex::connect(&sup.grpc_url(), key.as_deref())?;
            let client = qdrant_client::Qdrant::from_url(&sup.grpc_url())
                .api_key(key)
                .timeout(Duration::from_secs(60))
                .skip_compatibility_check()
                .build()?;
            Ok(Opened {
                idx: Arc::new(idx),
                managed: Some((sup, client)),
                #[cfg(feature = "edge")]
                edge: None,
            })
        }
        #[cfg(feature = "edge")]
        "edge" => {
            let opts = zam_index::EdgeOptions {
                indexing_threads: threads,
                // фоновый проход не вмешивается в замер: оптимизация — явным вызовом
                optimize_debounce: Duration::from_secs(24 * 3600),
                ..zam_index::EdgeOptions::default()
            };
            let d = dir.join("edge");
            let e =
                tokio::task::spawn_blocking(move || zam_index::EdgeIndex::open(&d, opts)).await??;
            let e = Arc::new(e);
            Ok(Opened {
                idx: e.clone(),
                managed: None,
                edge: Some(e),
            })
        }
        #[cfg(not(feature = "edge"))]
        "edge" => Err("собрано без feature edge".into()),
        other => Err(format!("неизвестный вариант {other}").into()),
    }
}

/// Фоновый замер пикового RSS процесса бенчмарка и (для managed) Qdrant.
struct RssSampler {
    stop: Arc<AtomicBool>,
    peak_self: Arc<AtomicU64>,
    peak_qdrant: Arc<AtomicU64>,
    peak_sum: Arc<AtomicU64>,
    thread: std::thread::JoinHandle<()>,
}

impl RssSampler {
    fn start(qdrant: Option<u32>) -> RssSampler {
        let stop = Arc::new(AtomicBool::new(false));
        let (ps, pq, psum) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        let (s2, ps2, pq2, psum2) = (stop.clone(), ps.clone(), pq.clone(), psum.clone());
        let thread = std::thread::spawn(move || {
            let me = std::process::id();
            while !s2.load(Ordering::Relaxed) {
                let a = rss_bytes(me).unwrap_or(0);
                let b = qdrant.and_then(rss_bytes).unwrap_or(0);
                ps2.fetch_max(a, Ordering::Relaxed);
                pq2.fetch_max(b, Ordering::Relaxed);
                psum2.fetch_max(a + b, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        RssSampler {
            stop,
            peak_self: ps,
            peak_qdrant: pq,
            peak_sum: psum,
            thread,
        }
    }

    fn finish(self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
        json!({
            "self": self.peak_self.load(Ordering::Relaxed),
            "qdrant": self.peak_qdrant.load(Ordering::Relaxed),
            "total": self.peak_sum.load(Ordering::Relaxed),
        })
    }
}

fn rss_now(qdrant: Option<u32>) -> Value {
    let me = rss_bytes(std::process::id()).unwrap_or(0);
    let q = qdrant.and_then(rss_bytes).unwrap_or(0);
    json!({"self": me, "qdrant": q, "total": me + q})
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

// ---------------------------------------------------------------- build

async fn cmd_build(a: &Args) -> Res<Value> {
    let dir = a.path("dir")?;
    let points = a.num("points", 1_000_000)? as u64;
    let dim = a.num("dim", 1024)?;
    let data = Arc::new(Data::new(dim, read_windows(&a.path("sparse")?)?));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;

    let t_open = Instant::now();
    let o = open(a, &dir).await?;
    let open_ms = t_open.elapsed().as_millis() as u64;
    let sampler = RssSampler::start(o.qdrant_pid());
    let t0 = Instant::now();
    o.idx.drop_all().await?;
    let meta = o.idx.ensure(MODEL, dim).await?;
    if !meta.created {
        return Err("коллекция не создана заново".into());
    }
    let ensure_ms = t0.elapsed().as_millis() as u64;

    // Пересборка: пачки по 256 точек, до 8 записей одновременно (как reindex из кэша).
    let t_up = Instant::now();
    let batches = points.div_ceil(BATCH as u64);
    let done = Arc::new(AtomicU64::new(0));
    let idx = o.idx.clone();
    stream::iter((0..batches).map(|b| {
        let (data, idx, done) = (data.clone(), idx.clone(), done.clone());
        async move {
            let lo = b * BATCH as u64;
            let hi = (lo + BATCH as u64).min(points);
            let pts = tokio::task::spawn_blocking(move || {
                (lo..hi).map(|i| data.point(i)).collect::<Vec<_>>()
            })
            .await
            .map_err(|e| zam_index::IndexError::Other(e.to_string()))?;
            idx.upsert(pts).await?;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 400 == 0 {
                eprintln!("upsert {} / {}", n * BATCH as u64, points);
            }
            Ok::<(), zam_index::IndexError>(())
        }
    }))
    .buffer_unordered(PARALLEL)
    .try_collect::<Vec<()>>()
    .await?;
    let upsert_ms = t_up.elapsed().as_millis() as u64;
    eprintln!("upsert готов за {upsert_ms} мс; ожидание индексации");

    // Ожидание конца индексации.
    let t_ix = Instant::now();
    let mut indexed = Value::Null;
    if let Some((_, client)) = &o.managed {
        indexed = wait_green(client).await?;
    }
    #[cfg(feature = "edge")]
    if let Some(e) = &o.edge {
        e.optimize_now().await?;
        let (segments, indexed_vectors) = e.segments().await?;
        indexed = json!({"segments": segments, "indexed_vectors": indexed_vectors});
    }
    let index_ms = t_ix.elapsed().as_millis() as u64;
    let total_ms = t0.elapsed().as_millis() as u64;
    let count = o
        .idx
        .count(&VectorFilter {
            user_id: data.user,
            space_ids: vec![],
            path_prefix: None,
            levels: vec![],
        })
        .await?;
    let rss_after = rss_now(o.qdrant_pid());
    let peak = sampler.finish();
    o.close().await;
    let disk = dir_size(&dir);
    Ok(json!({
        "backend": a.str("backend")?,
        "points": points,
        "count": count,
        "dim": dim,
        "threads": a.num("threads", 2)?,
        "open_ms": open_ms,
        "ensure_ms": ensure_ms,
        "upsert_ms": upsert_ms,
        "index_ms": index_ms,
        "total_ms": total_ms,
        "indexed": indexed,
        "rss_after": rss_after,
        "rss_peak": peak,
        "disk_bytes": disk,
    }))
}

/// Ждёт статуса green коллекции три опроса подряд (индексация и слияние сегментов закончены).
async fn wait_green(client: &qdrant_client::Qdrant) -> Res<Value> {
    use qdrant_client::qdrant::CollectionStatus;
    let mut green = 0;
    loop {
        let info = client
            .collection_info(COLLECTION)
            .await?
            .result
            .ok_or("collection_info без результата")?;
        if info.status == CollectionStatus::Green as i32 {
            green += 1;
            if green >= 3 {
                return Ok(json!({
                    "segments": info.segments_count,
                    "indexed_vectors": info.indexed_vectors_count,
                }));
            }
        } else {
            green = 0;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ---------------------------------------------------------------- search

struct Q {
    target: u64,
    dense: Vec<f32>,
    sparse: Vec<(u32, f32)>,
    filter: VectorFilter,
}

fn queries(data: &Data, points: u64, n: usize, seed: u64) -> Vec<Q> {
    let mut r = Rng(mix(seed));
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let target = r.next() % points;
        if Data::level(target) != "chunk" {
            continue;
        }
        let k = out.len() as u64;
        // четверть запросов — с фильтром по папке (1/8 корпуса)
        let path_prefix = (k % 4 == 3).then(|| Data::path(target)[0].clone());
        out.push(Q {
            target,
            dense: data.dense(target, Some((seed ^ k, QUERY_NOISE))),
            sparse: data.query_sparse(target, seed ^ k),
            filter: VectorFilter {
                user_id: data.user,
                space_ids: Data::spaces(),
                path_prefix,
                levels: vec![],
            },
        });
    }
    out
}

fn stats(mut v: Vec<f64>) -> Value {
    v.sort_by(f64::total_cmp);
    let q = |p: f64| {
        if v.is_empty() {
            return f64::NAN;
        }
        let i = ((v.len() - 1) as f64 * p).round() as usize;
        v[i]
    };
    json!({
        "n": v.len(),
        "mean": v.iter().sum::<f64>() / v.len().max(1) as f64,
        "p50": q(0.5),
        "p95": q(0.95),
        "p99": q(0.99),
        "max": v.last().copied().unwrap_or(f64::NAN),
    })
}

async fn cmd_search(a: &Args) -> Res<Value> {
    let dir = a.path("dir")?;
    let points = a.num("points", 1_000_000)? as u64;
    let dim = a.num("dim", 1024)?;
    let n = a.num("queries", 1000)?;
    let warm = a.num("warmup", 100)?;
    let data = Data::new(dim, read_windows(&a.path("sparse")?)?);
    let qs = queries(&data, points, n, 0x5EA2C4);
    let wq = queries(&data, points, warm, 0x3A4E);

    let t_open = Instant::now();
    let o = open(a, &dir).await?;
    let meta = o.idx.ensure(MODEL, dim).await?;
    let open_ms = t_open.elapsed().as_millis() as u64;
    if meta.points != points {
        return Err(format!("в индексе {} точек, ожидалось {points}", meta.points).into());
    }
    let rss_open = rss_now(o.qdrant_pid());
    let t = Instant::now();
    o.idx
        .search(&wq[0].dense, &wq[0].sparse, &wq[0].filter)
        .await?;
    let first_ms = t.elapsed().as_secs_f64() * 1e3;
    for q in &wq[1..] {
        o.idx.search(&q.dense, &q.sparse, &q.filter).await?;
    }
    let sampler = RssSampler::start(o.qdrant_pid());
    let mut lat = Vec::with_capacity(n);
    let mut lat_filtered = Vec::new();
    let (mut found1, mut found10) = (0usize, 0usize);
    let mut kept: Vec<Vec<String>> = Vec::new();
    for (k, q) in qs.iter().enumerate() {
        let t = Instant::now();
        let hits: Vec<VectorHit> = o.idx.search(&q.dense, &q.sparse, &q.filter).await?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        lat.push(ms);
        if q.filter.path_prefix.is_some() {
            lat_filtered.push(ms);
        }
        let want = format!("c{}", q.target);
        if hits.first().is_some_and(|h| h.chunk_id == want) {
            found1 += 1;
        }
        if hits.iter().take(10).any(|h| h.chunk_id == want) {
            found10 += 1;
        }
        if k < KEEP_HITS {
            kept.push(hits.iter().take(10).map(|h| h.chunk_id.clone()).collect());
        }
    }
    let rss_after = rss_now(o.qdrant_pid());
    let peak = sampler.finish();
    o.close().await;
    Ok(json!({
        "backend": a.str("backend")?,
        "points": points,
        "open_ms": open_ms,
        "first_query_ms": first_ms,
        "latency_ms": stats(lat),
        "latency_filtered_ms": stats(lat_filtered),
        "self_top1": found1 as f64 / n as f64,
        "self_top10": found10 as f64 / n as f64,
        "rss_open": rss_open,
        "rss_after": rss_after,
        "rss_peak": peak,
        "hits": kept,
    }))
}
