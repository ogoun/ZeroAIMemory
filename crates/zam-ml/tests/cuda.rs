//! Настоящий прогон на CUDA (GPU-сборка ORT + библиотеки NVIDIA из `<ORT>/cuda/` или системы).
//! Без GPU или без CUDA тест сообщает о пропуске: откат на CPU проверяет `tests/device.rs`.

mod common;

use std::time::Instant;

use common::*;
use zam_ml::{Device, DevicePref, Embedder};

const MINILM: &str = "minilm-l6-int8";

#[test]
fn minilm_on_cuda_matches_cpu() {
    if !have(&ort_gpu(), MINILM) {
        return;
    }
    let (gpu, rep) = Embedder::load(&config(ort_gpu(), models_dir(), DevicePref::Cuda), MINILM)
        .expect("загрузка без паники");
    if rep.device == Device::Cpu {
        eprintln!("пропуск: CUDA недоступна ({})", rep.reason);
        return;
    }
    assert!(rep.reason.is_empty());
    let (cpu, _) =
        Embedder::load(&config(ort_gpu(), models_dir(), DevicePref::Cpu), MINILM).unwrap();
    for t in [
        "Bash scripts on Windows need LF line endings",
        "Rust borrow checker rules",
    ] {
        let a = gpu.embed_passages(&[t]).unwrap().remove(0);
        let b = cpu.embed_passages(&[t]).unwrap().remove(0);
        let c = cosine(&a.dense, &b.dense);
        eprintln!("CUDA/CPU косинус {c}");
        assert!(c > 0.99, "{c}");
    }
}

/// bge-m3 fp16 на CUDA: скорость на 100 чанках по ~400 токенов.
/// Запуск: `cargo test --release -p zam-ml --test cuda -- --ignored --nocapture`.
#[test]
#[ignore = "нужны GPU, CUDA и bge-m3-fp16 (cargo xtask fetch-models --id bge-m3-fp16)"]
fn bge_m3_fp16_cuda_throughput() {
    const BGE: &str = "bge-m3-fp16";
    if !have(&ort_gpu(), BGE) {
        return;
    }
    let rss0 = rss_bytes();
    let t = Instant::now();
    let (e, rep) = Embedder::load(&config(ort_gpu(), models_dir(), DevicePref::Cuda), BGE).unwrap();
    let load = t.elapsed();
    let rss1 = rss_bytes();
    assert!(
        matches!(rep.device, Device::Cuda { .. }),
        "CUDA недоступна: {}",
        rep.reason
    );
    assert!(!e.spec().dynamic_quant);
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
    let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    e.embed_passages(&refs[..20]).unwrap(); // прогрев (выделение памяти CUDA)
    let t = Instant::now();
    let out = e.embed_passages(&refs).unwrap();
    let dt = t.elapsed();
    assert_eq!(out.len(), 100);
    assert!(
        out.iter()
            .all(|x| (norm(&x.dense) - 1.0).abs() < 1e-3 && !x.sparse.is_empty())
    );
    // батч против одиночного прогона (fp16 без динамического квантования)
    let single = e.embed_passages(&refs[..1]).unwrap().remove(0);
    let c = cosine(&single.dense, &out[0].dense);
    let t = Instant::now();
    for i in 0..20 {
        e.embed_query(&format!("запрос номер {i} про bash и LF"))
            .unwrap();
    }
    let qdt = t.elapsed() / 20;
    let mb = |x: Option<u64>| x.map_or(-1.0, |b| b as f64 / 1048576.0);
    eprintln!(
        "bge-m3 fp16 {}: загрузка {load:?}; RSS до {:.0} МБ, после загрузки {:.0} МБ; \
         100 чанков за {dt:?} = {:.1} чанка/с; запрос {qdt:?}; батч/одиночный косинус {c}",
        rep.device,
        mb(rss0),
        mb(rss1),
        100.0 / dt.as_secs_f64()
    );
    assert!(c > 0.999, "{c}");
}

#[test]
fn cpu_model_stays_on_cpu_with_cuda_pref() {
    if !have(&ort_gpu(), MINILM) {
        return;
    }
    // вариант модели для CPU (device = "cpu") не уходит на GPU ни при cuda, ни при auto
    let root = common::minilm_copy("minilm-cpu-only", "device = \"cpu\"\n");
    for pref in [DevicePref::Cuda, DevicePref::Auto] {
        let (e, rep) =
            Embedder::load(&config(ort_gpu(), root.clone(), pref), "minilm-cpu-only").unwrap();
        assert_eq!(rep.device, Device::Cpu, "{pref:?}");
        assert!(rep.reason.contains("для CPU"), "{}", rep.reason);
        assert_eq!(e.device(), &rep);
        e.embed_query("cpu only").unwrap();
    }
}
