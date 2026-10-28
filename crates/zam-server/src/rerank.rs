//! Серверный реранк (`rerank=on`, §9.2 п. 5): ленивая загрузка реранкера и перестановка
//! кандидатов поиска.
//!
//! - **Модель** — по устройству эмбеддера: CPU → `ml.rerank_model_cpu` (`jina-reranker-v2`),
//!   CUDA → `ml.rerank_model_gpu` (`bge-reranker-v2-m3`). Без эмбеддера (FTS-only) — CPU.
//! - **Лениво.** Реранкер грузится при первом запросе с `rerank=on` (в `spawn_blocking`) и
//!   дальше живёт в памяти; без таких запросов память не занимает. Неудачная загрузка
//!   запоминается на минуту: поставленная `models fetch` модель подхватится без перезапуска,
//!   а поток запросов не грузит её заново на каждом.
//! - **Сбой не роняет поиск.** Нет модели — ответ без реранка с предупреждением
//!   [`WARN_NOT_INSTALLED`]; ошибка или паника загрузки и инференса — [`WARN_FAILED`].
//!
//! - **Оценщик** — [`PassageScorer`]: модель zam-ml ([`Reranker`]) или готовый оценщик,
//!   поставленный `RerankSlot::install` (детерминированная заглушка тестов, фича `test-hooks`).
//!
//! В журнал не попадают тексты запроса и фрагментов — только id модели, число пар и время.

use std::sync::Arc;
use std::time::{Duration, Instant};

use zam_ml::{Device, MlConfig, MlError, Reranker};

use crate::config::ServerConfig;

/// Реранкер не установлен (`zam-server models fetch --reranker`; CPU-реранкер jina —
/// ещё с `--accept-nc-license`).
pub const WARN_NOT_INSTALLED: &str = "reranker not installed";
/// Реранкер не загрузился или инференс не удался (подробности — в журнале).
pub const WARN_FAILED: &str = "reranker failed";
/// Через сколько повторять неудачную загрузку.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Оценщик пар «запрос — фрагмент»: баллы релевантности 0..1 в порядке фрагментов.
pub trait PassageScorer: Send + Sync {
    /// Id модели для журнала.
    fn model_id(&self) -> &str;
    /// Баллы `passages` запросу `query`; фрагмент — до `passage_tokens` токенов.
    fn score_capped(
        &self,
        query: &str,
        passages: &[&str],
        passage_tokens: usize,
    ) -> Result<Vec<f32>, MlError>;
}

impl PassageScorer for Reranker {
    fn model_id(&self) -> &str {
        Reranker::model_id(self)
    }

    fn score_capped(
        &self,
        query: &str,
        passages: &[&str],
        passage_tokens: usize,
    ) -> Result<Vec<f32>, MlError> {
        Reranker::score_capped(self, query, passages, passage_tokens)
    }
}

enum State {
    Empty,
    Ready(Arc<dyn PassageScorer>),
    Failed { at: Instant, warning: &'static str },
}

/// Реранкер сервера: загружается при первом `rerank=on`.
pub struct RerankSlot {
    /// Настройки zam-ml (`None` — `[ml]` не разобрался: реранк недоступен).
    ml: Option<MlConfig>,
    model_cpu: String,
    model_gpu: String,
    /// Состояние под асинхронным мьютексом: загрузку держит отдельная задача (см. [`RerankSlot::get`]).
    state: Arc<tokio::sync::Mutex<State>>,
    /// Сколько раз запускалась загрузка (проверка в тестах: одна загрузка на слот).
    loads: Arc<std::sync::atomic::AtomicUsize>,
    /// Пауза перед загрузкой — только тесты (отмена запроса во время загрузки).
    #[cfg(test)]
    delay: Duration,
}

impl std::fmt::Debug for RerankSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RerankSlot")
            .field("model_cpu", &self.model_cpu)
            .field("model_gpu", &self.model_gpu)
            .finish_non_exhaustive()
    }
}

impl RerankSlot {
    pub fn new(cfg: &ServerConfig) -> RerankSlot {
        RerankSlot {
            ml: crate::ml::ml_config(cfg).ok(),
            model_cpu: cfg.ml.rerank_model_cpu.clone(),
            model_gpu: cfg.ml.rerank_model_gpu.clone(),
            state: Arc::new(tokio::sync::Mutex::new(State::Empty)),
            loads: Arc::default(),
            #[cfg(test)]
            delay: Duration::ZERO,
        }
    }

    /// Id реранкера для устройства эмбеддера.
    pub fn model_for(&self, embed_device: Option<Device>) -> &str {
        match embed_device {
            Some(Device::Cuda { .. }) => &self.model_gpu,
            _ => &self.model_cpu,
        }
    }

    /// Ставит готовый оценщик вместо загрузки модели: следующие `rerank=on` идут через него
    /// (детерминированная заглушка интеграционных тестов; модель тогда не грузится). Только
    /// с фичей `test-hooks` — её включает dev-зависимость пакета на себя.
    #[cfg(feature = "test-hooks")]
    pub async fn install(&self, scorer: Arc<dyn PassageScorer>) {
        *self.state.lock().await = State::Ready(scorer);
    }

    /// Реранкер (загружает при первом вызове) или предупреждение для ответа.
    ///
    /// Загрузка идёт в отдельной задаче tokio, которая держит мьютекс состояния и сама
    /// записывает итог: отмена запроса (клиент ушёл) не бросает загруженную модель, а
    /// следующий запрос ждёт ту же загрузку, а не начинает вторую (вторая копия — до 1,1 ГБ
    /// памяти или VRAM).
    pub async fn get(
        &self,
        embed_device: Option<Device>,
    ) -> Result<Arc<dyn PassageScorer>, &'static str> {
        let mut st = self.state.clone().lock_owned().await;
        match &*st {
            State::Ready(r) => return Ok(r.clone()),
            State::Failed { at, warning } if at.elapsed() < RETRY_AFTER => return Err(warning),
            _ => {}
        }
        let Some(mut ml) = self.ml.clone() else {
            return Err(WARN_FAILED);
        };
        let id = self.model_for(embed_device).to_string();
        // реранкер для эмбеддера на CPU не ищет CUDA (и наоборот, GPU-модель — на CUDA)
        if !matches!(embed_device, Some(Device::Cuda { .. })) {
            ml.device = zam_ml::DevicePref::Cpu;
        }
        let loads = self.loads.clone();
        #[cfg(test)]
        let delay = self.delay;
        let task = tokio::spawn(async move {
            let load_id = id.clone();
            let loaded = tokio::task::spawn_blocking(move || {
                loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                #[cfg(test)]
                std::thread::sleep(delay);
                let r = Reranker::load(&ml, &load_id);
                if r.is_ok() {
                    crate::models::warn_if_stale(&ml.models_dir, &load_id);
                }
                r
            })
            .await;
            let warning = match loaded {
                Ok(Ok(r)) => {
                    let r: Arc<dyn PassageScorer> = Arc::new(r);
                    *st = State::Ready(r.clone());
                    return Ok(r);
                }
                Ok(Err(MlError::NoModel(e))) => {
                    tracing::warn!(model = %id, error = %e, hint = "zam-server models fetch --reranker",
                        "реранкер не установлен: поиск без реранка");
                    WARN_NOT_INSTALLED
                }
                Ok(Err(e)) => {
                    tracing::warn!(model = %id, error = %e, "реранкер не загрузился: поиск без реранка");
                    WARN_FAILED
                }
                Err(j) => {
                    tracing::error!(model = %id, panic = j.is_panic(), "загрузка реранкера аварийно завершилась");
                    WARN_FAILED
                }
            };
            *st = State::Failed {
                at: Instant::now(),
                warning,
            };
            Err(warning)
        });
        task.await.unwrap_or(Err(WARN_FAILED))
    }
}

/// Баллы реранка `texts` (в `spawn_blocking`). Ошибка и паника — [`WARN_FAILED`].
pub async fn score(
    r: Arc<dyn PassageScorer>,
    query: String,
    texts: Vec<String>,
    passage_tokens: usize,
) -> Result<Vec<f32>, &'static str> {
    let n = texts.len();
    let model = r.model_id().to_string();
    let run = move || {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        r.score_capped(&query, &refs, passage_tokens)
    };
    run_scorer(run, n, &model).await
}

/// Скоринг `run` в `spawn_blocking` с проверкой числа баллов; сбой — [`WARN_FAILED`].
pub(crate) async fn run_scorer(
    run: impl FnOnce() -> Result<Vec<f32>, MlError> + Send + 'static,
    n: usize,
    model: &str,
) -> Result<Vec<f32>, &'static str> {
    match tokio::task::spawn_blocking(run).await {
        Ok(Ok(s)) if s.len() == n => Ok(s),
        Ok(Ok(s)) => {
            tracing::error!(
                model,
                expected = n,
                got = s.len(),
                "реранкер вернул не то число баллов"
            );
            Err(WARN_FAILED)
        }
        Ok(Err(e)) => {
            tracing::warn!(model, error = %e, "реранк не удался: поиск без реранка");
            Err(WARN_FAILED)
        }
        Err(j) => {
            tracing::error!(
                model,
                panic = j.is_panic(),
                "реранк аварийно завершился: поиск без реранка"
            );
            Err(WARN_FAILED)
        }
    }
}

/// Переставляет первые `scores.len()` элементов по убыванию балла (при равенстве — прежний
/// порядок) и возвращает балл каждого переставленного; хвост остаётся как был. Баллы —
/// реранкера с бустами (§9.2 п. 5).
pub fn reorder<T>(items: Vec<T>, scores: &[f64]) -> Vec<(T, Option<f64>)> {
    let n = scores.len().min(items.len());
    let mut it = items.into_iter();
    let mut head: Vec<(usize, T)> = it.by_ref().take(n).enumerate().collect();
    head.sort_by(|a, b| scores[b.0].total_cmp(&scores[a.0]).then(a.0.cmp(&b.0)));
    head.into_iter()
        .map(|(i, t)| (t, Some(scores[i])))
        .chain(it.map(|t| (t, None)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_head_only_and_stable() {
        let r = reorder(vec!["a", "b", "c", "d"], &[0.1, 0.9, 0.1]);
        let order: Vec<&str> = r.iter().map(|x| x.0).collect();
        assert_eq!(order, ["b", "a", "c", "d"]);
        assert_eq!(r[0].1, Some(0.9));
        assert_eq!(r[3].1, None);
        // баллов больше, чем элементов
        let r = reorder(vec!["x"], &[0.3, 0.4]);
        assert_eq!(r.len(), 1);
        assert!(reorder(Vec::<u8>::new(), &[]).is_empty());
    }

    #[tokio::test]
    async fn scorer_error_panic_and_wrong_count_are_warnings() {
        let r = run_scorer(|| Err(MlError::Ort("сбой".into())), 1, "m").await;
        assert_eq!(r, Err(WARN_FAILED));
        let r = run_scorer(|| panic!("инференс упал"), 1, "m").await;
        assert_eq!(r, Err(WARN_FAILED));
        let r = run_scorer(|| Ok(vec![0.5]), 2, "m").await;
        assert_eq!(r, Err(WARN_FAILED));
        assert_eq!(run_scorer(|| Ok(vec![0.5]), 1, "m").await, Ok(vec![0.5]));
    }

    #[tokio::test]
    async fn missing_model_warns_and_is_remembered() {
        let mut cfg = ServerConfig::default();
        let dir = crate::test_util::dir_in_target("rerank-slot-empty");
        cfg.ml.models_dir = Some(dir);
        let slot = RerankSlot::new(&cfg);
        assert_eq!(slot.model_for(None), "jina-reranker-v2");
        assert_eq!(
            slot.model_for(Some(Device::Cuda { id: 0 })),
            "bge-reranker-v2-m3"
        );
        assert_eq!(slot.get(None).await.err(), Some(WARN_NOT_INSTALLED));
        // повтор в течение минуты — из памяти, без новой загрузки
        assert!(matches!(
            &*slot.state.lock().await,
            State::Failed { warning, .. } if *warning == WARN_NOT_INSTALLED
        ));
        assert_eq!(slot.get(None).await.err(), Some(WARN_NOT_INSTALLED));
        assert_eq!(slot.loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_request_does_not_restart_loading() {
        let mut cfg = ServerConfig::default();
        cfg.ml.models_dir = Some(crate::test_util::dir_in_target("rerank-slot-cancel"));
        let mut slot = RerankSlot::new(&cfg);
        slot.delay = Duration::from_millis(400);
        // клиент ушёл во время загрузки: future запроса брошен
        let r = tokio::time::timeout(Duration::from_millis(50), slot.get(None)).await;
        assert!(r.is_err(), "загрузка должна идти дольше таймаута");
        // следующий запрос ждёт ту же загрузку, вторая не начинается
        assert_eq!(slot.get(None).await.err(), Some(WARN_NOT_INSTALLED));
        assert_eq!(slot.loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
