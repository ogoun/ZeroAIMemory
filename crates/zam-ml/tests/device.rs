//! Откат на CPU на GPU-сборке ORT без библиотек NVIDIA (отдельный процесс: ORT и провайдер
//! CUDA грузятся один раз на процесс). Рядом с ORT библиотек NVIDIA нет (их ставит
//! `zam-server cuda fetch`); системная установка CUDA убирается из PATH этого процесса (Windows).

mod common;

use common::*;
use zam_ml::{Device, DevicePref, Embedder};

const MINILM: &str = "minilm-l6-int8";

/// Убирает из PATH каталоги с библиотеками CUDA/cuDNN (поиск зависимых DLL в Windows идёт по PATH).
fn strip_cuda_from_path() {
    let Some(path) = std::env::var_os("PATH") else {
        return;
    };
    let markers = [
        "cudart64_13.dll",
        "cublas64_13.dll",
        "cudnn64_9.dll",
        "libcudart.so.13",
    ];
    let kept: Vec<_> = std::env::split_paths(&path)
        .filter(|d| !markers.iter().any(|m| d.join(m).is_file()))
        .collect();
    let joined = std::env::join_paths(kept).unwrap();
    // SAFETY: в этой программе тестов один тест, переменные окружения больше никто не читает
    // и не пишет параллельно; вызов идёт до загрузки ORT.
    unsafe { std::env::set_var("PATH", joined) };
}

#[test]
fn cuda_without_nvidia_libs_falls_back_to_cpu() {
    if !have(&ort_gpu(), MINILM) {
        return;
    }
    let cuda_dir = ort_gpu().parent().unwrap().join("cuda");
    if cuda_dir.is_dir() {
        eprintln!("пропуск: {} существует", cuda_dir.display());
        return;
    }
    strip_cuda_from_path();
    for pref in [DevicePref::Cuda, DevicePref::Auto] {
        let cfg = config(ort_gpu(), models_dir(), pref);
        let (e, rep) = Embedder::load(&cfg, MINILM).expect("откат на CPU без паники");
        if rep.device != Device::Cpu {
            // Linux с CUDA в ldconfig: системную установку из процесса не убрать
            eprintln!("CUDA найдена в системе: {:?}", rep.device);
            assert!(rep.reason.is_empty());
            continue;
        }
        assert!(!rep.reason.is_empty(), "{pref:?}: пустая причина");
        eprintln!("{pref:?}: {}", rep.reason);
        let v = e.embed_query("fallback works").unwrap();
        assert!((norm(&v.dense) - 1.0).abs() < 1e-4);
    }
    // явный CPU — причина «выбран CPU»
    let cfg = config(ort_gpu(), models_dir(), DevicePref::Cpu);
    let (_, rep) = Embedder::load(&cfg, MINILM).unwrap();
    assert_eq!(rep.device, Device::Cpu);
    assert!(!rep.reason.is_empty());
}
