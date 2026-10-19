//! Реранкер (cross-encoder, §9.2 п. 5): пара «запрос — фрагмент» → балл релевантности.
//!
//! Модели — `jina-reranker-v2` (CPU, int8; CC-BY-NC-4.0, загрузка только с согласием) и
//! `bge-reranker-v2-m3` (GPU, fp16; Apache-2.0). Обе — XLM-RoBERTa: вход `input_ids` и
//! `attention_mask` (`token_type_ids` подаётся, только если он есть у графа), выход `logits`
//! `[B, 1]` (или `[B]`), f32 или f16. Балл — `sigmoid(logit)` в 0..1, порядок — по логиту.
//! Пары кодируются токенизатором модели (`<s> запрос </s></s> фрагмент </s>`).
//!
//! Одна сессия на модель (реранк — по запросу, без фоновой нагрузки), под мьютексом. Вариант
//! для CPU (`device = "cpu"`, int8) на GPU не запускается — как у эмбеддера.

use std::borrow::Cow;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use ort::environment::Environment;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType};
use tokenizers::Encoding;

use crate::device::build_session;
use crate::models::{MODEL_FILE, TOKENIZER_FILE, model_dir, require_file, sha8};
use crate::runtime::{environment, resolve_dylib};
use crate::spec::{ModelDevice, RerankSpec};
use crate::tokenize::Tok;
use crate::{DevicePref, DeviceReport, MlConfig, MlError};

/// Имя выхода с логитами (экспорт optimum / onnx-community).
const LOGITS: &str = "logits";
/// Предел токенов в батче (с паддингом): 20 пар × 256 укладываются в один прогон.
const MAX_BATCH_TOKENS: usize = 16384;

struct Runner {
    session: Session,
    output: String,
    token_types: bool,
}

/// Реранкер модели. `Send + Sync`: сессия под мьютексом.
pub struct Reranker {
    spec: RerankSpec,
    model_id: String,
    tok: Tok,
    runner: Mutex<Runner>,
    report: DeviceReport,
    /// Окружение ORT живёт не меньше сессии.
    _env: Environment,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

impl Reranker {
    /// Загружает реранкер `<models_dir>/<id>`: описатель (`role = "rerank"`), файлы, ORT,
    /// токенизатор и сессию на выбранном устройстве (с откатом на CPU). Потоки — как у
    /// сессии индексации (`ml.index_threads`, 0 — все физические ядра): 20 × 256 токенов на
    /// двух потоках запросов шли бы секунды.
    pub fn load(cfg: &MlConfig, id: &str) -> Result<Reranker, MlError> {
        let t0 = Instant::now();
        let spec = RerankSpec::load(&cfg.models_dir, id)?;
        let dir = model_dir(&cfg.models_dir, id)?;
        let model = dir.join(MODEL_FILE);
        let tokenizer = dir.join(TOKENIZER_FILE);
        require_file(&model)?;
        require_file(&tokenizer)?;
        let ort_dylib = resolve_dylib(&cfg.ort_dylib)?;
        let env = environment(&ort_dylib)?;
        let tok = Tok::load(&tokenizer)?;
        let full_id = format!("{}@{}", spec.id, sha8(&model)?);
        let cpu_only = spec.device == ModelDevice::Cpu && cfg.device != DevicePref::Cpu;
        let pref = if cpu_only {
            DevicePref::Cpu
        } else {
            cfg.device
        };
        let (session, mut report) =
            build_session(&env, pref, &ort_dylib, &model, cfg.index_threads)?;
        let runner = Runner::new(session)?;
        if cpu_only {
            report.reason = format!(
                "модель {} для CPU (device = \"cpu\" в манифесте), для GPU нужен её GPU-вариант",
                spec.id
            );
        }
        tracing::info!(
            model = %full_id,
            device = %report.device,
            reason = %report.reason,
            ms = t0.elapsed().as_millis() as u64,
            "реранкер загружен"
        );
        Ok(Reranker {
            spec,
            model_id: full_id,
            tok,
            runner: Mutex::new(runner),
            report,
            _env: env,
        })
    }

    /// `"<id>@<sha8 model.onnx>"`.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Описатель модели.
    pub fn spec(&self) -> &RerankSpec {
        &self.spec
    }

    /// Устройство и причина выбора.
    pub fn device(&self) -> &DeviceReport {
        &self.report
    }

    /// Баллы релевантности (0..1) фрагментов `passages` запросу `query`, в порядке
    /// `passages`. Фрагмент усекается так, чтобы пара уложилась в `max_tokens_passage` модели.
    pub fn score(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>, MlError> {
        self.score_capped(query, passages, self.spec.max_tokens_passage)
    }

    /// [`Reranker::score`] с пределом фрагмента `passage_tokens` (без спецтокенов; `[ml]
    /// rerank_tokens` сервера, по умолчанию 256).
    pub fn score_capped(
        &self,
        query: &str,
        passages: &[&str],
        passage_tokens: usize,
    ) -> Result<Vec<f32>, MlError> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let t0 = Instant::now();
        let encs = self.tok.encode_pairs(
            query,
            passages,
            self.spec.max_tokens_query,
            passage_tokens.max(1),
            self.spec.max_tokens_passage,
        )?;
        let mut order: Vec<usize> = (0..encs.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(encs[i].len()));
        let mut out = vec![0f32; encs.len()];
        let mut runner = lock(&self.runner);
        let mut start = 0;
        while start < order.len() {
            let width = encs[order[start]].len().max(1);
            let rows = if self.spec.dynamic_quant {
                1
            } else {
                (MAX_BATCH_TOKENS / width).max(1)
            };
            let end = (start + rows).min(order.len());
            let batch: Vec<&Encoding> = order[start..end].iter().map(|&i| &encs[i]).collect();
            for (k, logit) in self.run(&mut runner, &batch)?.into_iter().enumerate() {
                out[order[start + k]] = sigmoid(logit);
            }
            start = end;
        }
        drop(runner);
        tracing::debug!(
            model = %self.model_id,
            pairs = passages.len(),
            ms = t0.elapsed().as_millis() as u64,
            "реранк"
        );
        Ok(out)
    }

    /// Один прогон: логиты пар батча.
    fn run(&self, runner: &mut Runner, batch: &[&Encoding]) -> Result<Vec<f32>, MlError> {
        let b = batch.len();
        let t = batch.iter().map(|e| e.len()).max().unwrap_or(0);
        if t == 0 {
            return Err(MlError::Spec("пустая пара токенов".into()));
        }
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
        let logits: Vec<f32> = match value.try_extract_tensor::<f32>() {
            Ok((shape, data)) => check_shape(&shape[..], b)
                .map(|step| data.iter().step_by(step).copied().collect())?,
            Err(_) => {
                let (shape, data) = value.try_extract_tensor::<half::f16>()?;
                let step = check_shape(&shape[..], b)?;
                data.iter().step_by(step).map(|x| x.to_f32()).collect()
            }
        };
        if logits.len() != b || logits.iter().any(|x| !x.is_finite()) {
            return Err(MlError::Spec(format!(
                "выход {}: {} логитов на {b} пар или не конечные значения",
                runner.output,
                logits.len()
            )));
        }
        Ok(logits)
    }
}

/// Форма логитов `[B]` или `[B, k]`: шаг до логита следующей пары (берётся первый класс).
fn check_shape(shape: &[i64], b: usize) -> Result<usize, MlError> {
    match shape {
        [n] if *n as usize == b => Ok(1),
        [n, k] if *n as usize == b && *k >= 1 => Ok(*k as usize),
        _ => Err(MlError::Spec(format!(
            "выход реранкера: форма {shape:?}, ожидалось [{b}, 1]"
        ))),
    }
}

impl Runner {
    fn new(session: Session) -> Result<Runner, MlError> {
        let has = |n: &str| session.inputs().iter().any(|i| i.name() == n);
        if !has("input_ids") || !has("attention_mask") {
            return Err(MlError::Spec(
                "у графа реранкера нет входов input_ids и attention_mask".into(),
            ));
        }
        let token_types = has("token_type_ids");
        let outs = session.outputs();
        let output = outs
            .iter()
            .find(|o| o.name() == LOGITS)
            .or_else(|| (outs.len() == 1).then(|| &outs[0]))
            .map(|o| o.name().to_string())
            .ok_or_else(|| {
                MlError::Spec(format!(
                    "у графа реранкера нет выхода {LOGITS} (выходы: {})",
                    outs.iter().map(|o| o.name()).collect::<Vec<_>>().join(", ")
                ))
            })?;
        if let Some(ty) = outs
            .iter()
            .find(|o| o.name() == output)
            .and_then(|o| o.dtype().tensor_type())
            && !matches!(ty, TensorElementType::Float32 | TensorElementType::Float16)
        {
            return Err(MlError::Spec(format!(
                "выход {output}: тип {ty:?}, ожидался f32 или f16"
            )));
        }
        Ok(Runner {
            session,
            output,
            token_types,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reranker_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<Reranker>();
    }

    #[test]
    fn logits_shape() {
        assert_eq!(check_shape(&[3], 3).unwrap(), 1);
        assert_eq!(check_shape(&[3, 1], 3).unwrap(), 1);
        assert_eq!(check_shape(&[2, 2], 2).unwrap(), 2);
        assert!(check_shape(&[2, 1], 3).is_err());
        assert!(check_shape(&[3, 1, 1], 3).is_err());
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6 && sigmoid(10.0) > sigmoid(-10.0));
    }
}
