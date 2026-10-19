//! Эмбеддер: две ORT-сессии (индексация и запросы), пулинг dense и sparse в Rust.
//!
//! Модель выдаёт `last_hidden_state` (или `token_embeddings`) `[B, T, dim]`; dense — CLS или
//! среднее под маской с L2-нормировкой, sparse — голова [`SparseHead`]. Один путь обслуживает
//! int8, fp16, fp32 и тестовую MiniLM (исследование 05 §3.3).

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use lru::LruCache;
use ort::environment::Environment;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Encoding;

use crate::device::build_sessions;
use crate::models::{MODEL_FILE, TOKENIZER_FILE, model_dir, require_file, sha8};
use crate::runtime::environment;
use crate::sparse::SparseHead;
use crate::spec::{ModelSpec, Pooling};
use crate::tokenize::{BgeTokenCounter, Tok};
use crate::{DeviceReport, MlConfig, MlError};

/// Предел токенов в батче индексации (с паддингом): ограничивает пик памяти активаций.
const MAX_BATCH_TOKENS: usize = 8192;
/// Длина батча округляется вверх до кратного 8.
const PAD_MULTIPLE: usize = 8;
/// Размер LRU-кэша эмбеддингов запросов.
const QUERY_CACHE: usize = 1024;
/// Имена выхода со скрытыми состояниями токенов (Xenova / BAAI).
const HIDDEN_OUTPUTS: [&str; 2] = ["last_hidden_state", "token_embeddings"];

/// Вектор текста.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// Плотный вектор, L2-норма = 1.
    pub dense: Vec<f32>,
    /// Разреженный вектор: (id токена, вес), по возрастанию id, без спецтокенов 0..=3.
    /// Пуст у моделей без sparse-головы.
    pub sparse: Vec<(u32, f32)>,
}

/// Сессия с разобранными именами входов и выхода.
struct Runner {
    session: Session,
    output: String,
    /// Подаётся ли `token_type_ids` (есть в `session.inputs()`, как у BERT/MiniLM).
    token_types: bool,
}

impl Runner {
    fn new(session: Session, dim: usize) -> Result<Runner, MlError> {
        let has = |n: &str| session.inputs().iter().any(|i| i.name() == n);
        if !has("input_ids") || !has("attention_mask") {
            return Err(MlError::Spec(
                "у графа нет входов input_ids и attention_mask".into(),
            ));
        }
        let token_types = has("token_type_ids");
        let outs = session.outputs();
        let output = HIDDEN_OUTPUTS
            .iter()
            .find(|n| outs.iter().any(|o| o.name() == **n))
            .map(|n| n.to_string())
            .or_else(|| (outs.len() == 1).then(|| outs[0].name().to_string()))
            .ok_or_else(|| {
                MlError::Spec(format!(
                    "у графа нет выхода {} (выходы: {})",
                    HIDDEN_OUTPUTS.join(" / "),
                    outs.iter().map(|o| o.name()).collect::<Vec<_>>().join(", ")
                ))
            })?;
        // последняя размерность выхода, если граф её знает
        if let Some(shape) = outs
            .iter()
            .find(|o| o.name() == output)
            .and_then(|o| o.dtype().tensor_shape())
            && let Some(&last) = shape.last()
            && last > 0
            && last as usize != dim
        {
            return Err(MlError::Spec(format!(
                "выход {output}: размерность {last}, в model.toml dim = {dim}"
            )));
        }
        Ok(Runner {
            session,
            output,
            token_types,
        })
    }
}

/// Эмбеддер модели. `Send + Sync`: сессии под своими мьютексами (`Session::run` требует `&mut`),
/// поэтому запросы не ждут батч индексации.
pub struct Embedder {
    spec: ModelSpec,
    model_id: String,
    tok: Tok,
    sparse: Option<SparseHead>,
    index: Mutex<Runner>,
    query: Mutex<Runner>,
    cache: Mutex<LruCache<String, Embedding>>,
    report: DeviceReport,
    /// Окружение ORT живёт не меньше сессий.
    _env: Environment,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > f32::MIN_POSITIVE {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

impl Embedder {
    /// Загружает модель `<models_dir>/<model_id>`: описатель, файлы, ORT, токенизатор,
    /// sparse-голову и две сессии на выбранном устройстве (с откатом на CPU).
    pub fn load(cfg: &MlConfig, model_id: &str) -> Result<(Embedder, DeviceReport), MlError> {
        let t0 = Instant::now();
        let spec = ModelSpec::load(&cfg.models_dir, model_id)?;
        let dir = model_dir(&cfg.models_dir, model_id)?;
        let model = dir.join(MODEL_FILE);
        let tokenizer = dir.join(TOKENIZER_FILE);
        require_file(&model)?;
        require_file(&tokenizer)?;
        if let Some(p) = &spec.sparse_head {
            require_file(p)?;
        }
        let env = environment(&cfg.ort_dylib)?;
        let tok = Tok::load(&tokenizer)?;
        let sparse = spec
            .sparse_head
            .as_deref()
            .map(|p| SparseHead::load(p, spec.dim))
            .transpose()?;
        let full_id = format!("{}@{}", spec.id, sha8(&model)?);
        let s = build_sessions(
            &env,
            cfg.device,
            &cfg.ort_dylib,
            &model,
            cfg.index_threads,
            cfg.query_threads,
        )?;
        let index = Runner::new(s.index, spec.dim)?;
        let query = Runner::new(s.query, spec.dim)?;
        let report = s.report;
        tracing::info!(
            model = %full_id,
            device = %report.device,
            reason = %report.reason,
            ms = t0.elapsed().as_millis() as u64,
            "модель загружена"
        );
        let e = Embedder {
            spec,
            model_id: full_id,
            tok,
            sparse,
            index: Mutex::new(index),
            query: Mutex::new(query),
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(QUERY_CACHE).expect("ненулевой размер"),
            )),
            report: report.clone(),
            _env: env,
        };
        Ok((e, report))
    }

    /// `"<id>@<sha8 model.onnx>"` — меняется при замене файла модели.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Размерность dense-вектора.
    pub fn dim(&self) -> usize {
        self.spec.dim
    }

    /// Описатель модели.
    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// Устройство и причина выбора.
    pub fn device(&self) -> &DeviceReport {
        &self.report
    }

    /// Эмбеддинги чанков (сессия индексации). Тексты сортируются по длине и режутся на батчи
    /// не больше 8192 токенов с паддингом до кратного 8; порядок результата — порядок `texts`.
    /// Модели с `dynamic_quant` (CPU int8) прогоняются по одному тексту без паддинга: масштаб
    /// квантования активаций считается по всему батчу, и соседи сдвигали бы вектор (косинус ≈ 0,99).
    pub fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Embedding>, MlError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let t0 = Instant::now();
        let encs = self.tok.encode(texts, self.spec.max_tokens_passage)?;
        let mut order: Vec<usize> = (0..encs.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(encs[i].len()));
        let mut out: Vec<Option<Embedding>> = vec![None; encs.len()];
        let mut runner = lock(&self.index);
        let mut start = 0;
        while start < order.len() {
            // самый длинный текст батча — первый (сортировка по убыванию)
            let width = pad_len(encs[order[start]].len());
            let rows = if self.spec.dynamic_quant {
                1
            } else {
                (MAX_BATCH_TOKENS / width).max(1)
            };
            let end = (start + rows).min(order.len());
            let batch: Vec<&Encoding> = order[start..end].iter().map(|&i| &encs[i]).collect();
            for (k, e) in self.run(&mut runner, &batch)?.into_iter().enumerate() {
                out[order[start + k]] = Some(e);
            }
            start = end;
        }
        drop(runner);
        tracing::debug!(
            model = %self.model_id,
            texts = texts.len(),
            ms = t0.elapsed().as_millis() as u64,
            "эмбеддинги чанков"
        );
        Ok(out
            .into_iter()
            .map(|e| e.expect("каждый текст в батче"))
            .collect())
    }

    /// Эмбеддинг запроса (сессия запросов, LRU-кэш на 1024 запроса).
    pub fn embed_query(&self, text: &str) -> Result<Embedding, MlError> {
        if let Some(e) = lock(&self.cache).get(text) {
            return Ok(e.clone());
        }
        let encs = self.tok.encode(&[text], self.spec.max_tokens_query)?;
        let e = {
            let mut runner = lock(&self.query);
            let refs: Vec<&Encoding> = encs.iter().collect();
            self.run(&mut runner, &refs)?
                .pop()
                .expect("один текст — один вектор")
        };
        lock(&self.cache).put(text.to_string(), e.clone());
        Ok(e)
    }

    /// Число токенов текста без спецтокенов (для `token_count` чанков).
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tok.count(text)
    }

    /// Счётчик токенов для чанкера (общий токенизатор, без копии).
    pub fn token_counter(&self) -> BgeTokenCounter {
        BgeTokenCounter::new(self.tok.inner.clone())
    }

    /// Один прогон модели на батче и пулинг.
    fn run(&self, runner: &mut Runner, batch: &[&Encoding]) -> Result<Vec<Embedding>, MlError> {
        let b = batch.len();
        let longest = batch.iter().map(|e| e.len()).max().unwrap_or(0);
        // один текст — без паддинга (паддинг влияет на масштаб динамического квантования)
        let t = if b == 1 {
            longest
        } else {
            pad_len(longest).min(longest.max(self.spec.max_tokens_passage))
        };
        let pad = i64::from(self.tok.pad_id);
        let mut ids = vec![pad; b * t];
        let mut mask = vec![0i64; b * t];
        let mut types = vec![0i64; b * t];
        for (r, e) in batch.iter().enumerate() {
            let row = r * t;
            for (k, ((&id, &m), &ty)) in e
                .get_ids()
                .iter()
                .zip(e.get_attention_mask())
                .zip(e.get_type_ids())
                .enumerate()
            {
                ids[row + k] = i64::from(id);
                mask[row + k] = i64::from(m);
                types[row + k] = i64::from(ty);
            }
        }
        let shape = [b, t];
        let mut inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = vec![
            ("input_ids".into(), Tensor::from_array((shape, ids))?.into()),
            (
                "attention_mask".into(),
                Tensor::from_array((shape, mask))?.into(),
            ),
        ];
        if runner.token_types {
            inputs.push((
                "token_type_ids".into(),
                Tensor::from_array((shape, types))?.into(),
            ));
        }
        let outputs = runner.session.run(inputs)?;
        let value = outputs
            .get(&runner.output)
            .ok_or_else(|| MlError::Ort(format!("нет выхода {}", runner.output)))?;
        let (oshape, hidden) = value.try_extract_tensor::<f32>()?;
        let h = self.spec.dim;
        if oshape.len() != 3
            || oshape[0] as usize != b
            || oshape[1] as usize != t
            || oshape[2] as usize != h
        {
            return Err(MlError::Spec(format!(
                "выход {}: форма {:?}, ожидалось [{b}, {t}, {h}]",
                runner.output,
                &oshape[..]
            )));
        }
        let mut res = Vec::with_capacity(b);
        for (r, e) in batch.iter().enumerate() {
            let len = e.len();
            let rows = &hidden[r * t * h..(r * t + len) * h];
            let mut dense = match self.spec.pooling {
                Pooling::Cls => rows[..h].to_vec(),
                Pooling::Mean => {
                    let mut acc = vec![0f32; h];
                    let mut n = 0f32;
                    for (tok, &m) in rows.chunks_exact(h).zip(e.get_attention_mask()) {
                        if m != 0 {
                            acc.iter_mut().zip(tok).for_each(|(a, x)| *a += x);
                            n += 1.0;
                        }
                    }
                    if n > 0.0 {
                        acc.iter_mut().for_each(|a| *a /= n);
                    }
                    acc
                }
            };
            l2_normalize(&mut dense);
            let sparse = match &self.sparse {
                Some(head) => head.pool(rows, e.get_ids(), e.get_attention_mask()),
                None => Vec::new(),
            };
            res.push(Embedding { dense, sparse });
        }
        Ok(res)
    }
}

/// Длина батча: вверх до кратного 8.
fn pad_len(n: usize) -> usize {
    n.max(1).div_ceil(PAD_MULTIPLE) * PAD_MULTIPLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedder_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<Embedder>();
        check::<BgeTokenCounter>();
    }

    #[test]
    fn pad_len_rounds_to_8() {
        assert_eq!(pad_len(0), 8);
        assert_eq!(pad_len(1), 8);
        assert_eq!(pad_len(8), 8);
        assert_eq!(pad_len(9), 16);
    }

    #[test]
    fn normalize_unit() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        let mut z = vec![0.0; 3];
        l2_normalize(&mut z);
        assert_eq!(z, vec![0.0; 3]);
    }
}
