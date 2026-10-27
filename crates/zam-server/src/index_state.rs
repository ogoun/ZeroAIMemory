//! Состояние векторного индекса в сервере (§4.1 п. 6, §4.3, §6.4, §8.1): модель
//! эмбеддингов, устройство, Qdrant (супервизор и клиент), воркер индексации и причина
//! деградации.
//!
//! Любая часть может отсутствовать — сервер всё равно стартует: запись и FTS работают,
//! задания индексации копятся, поиск отвечает `degraded: true` с причиной
//! ([`IndexRuntime::vector_available`]).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use zam_index::supervisor::{QdrantState, Supervisor};
use zam_index::{IndexError, VectorIndex, WorkerConfig, WorkerDeps, WorkerHandle};
use zam_ml::{DeviceReport, Embedder};
use zam_store::{SystemStore, UserDbs};

use crate::config::ServerConfig;
use crate::ml::{EmbedderAdapter, MlLoad};

/// Почему векторный канал выключен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    /// Нет библиотеки ONNX Runtime.
    NoRuntime,
    /// Модель не загружена (нет каталога, файлов или ошибка загрузки).
    NoModel,
    /// Установлена только модель для GPU (fp16), а CUDA недоступна или `ml.device = "cpu"`.
    GpuModelOnCpu,
    /// Qdrant запускается, перезапускается, недоступен или коллекция ещё не проверена.
    QdrantDown,
    /// Qdrant выключен супервизором (circuit breaker, чужая версия, нет бинарника) или не
    /// запущен из-за ошибки конфигурации, режима `edge`, клиента.
    Broken,
    /// Каталог `<data_dir>/qdrant/` занят другим экземпляром zam-server (лок супервизора).
    QdrantLocked,
    /// `model_id` коллекции не совпадает с текущей моделью: нужен reindex.
    ModelMismatch,
    /// Эмбеддинг запроса не удался (ошибка или паника инференса) — только в ответе поиска.
    EmbedFailed,
    /// Запрос к Qdrant не удался не из-за недоступности (ответ в неожиданном виде,
    /// ключ, прочее) — только в ответе поиска.
    SearchFailed,
}

impl DegradedReason {
    /// Значение `degraded_reason` в API.
    pub fn as_str(self) -> &'static str {
        match self {
            DegradedReason::NoRuntime => "no_runtime",
            DegradedReason::NoModel => "no_model",
            DegradedReason::GpuModelOnCpu => "gpu_model_on_cpu",
            DegradedReason::QdrantDown => "qdrant_down",
            DegradedReason::Broken => "broken",
            DegradedReason::QdrantLocked => "qdrant_locked",
            DegradedReason::ModelMismatch => "model_mismatch",
            DegradedReason::EmbedFailed => "embed_failed",
            DegradedReason::SearchFailed => "search_failed",
        }
    }

    /// Причина по-русски для человека (WebUI, CLI, строка `> degraded:` в Markdown) —
    /// единый словарь: клиенты берут текст из ответа (`degraded_text`, `vector.text`).
    pub fn text(self) -> &'static str {
        match self {
            DegradedReason::NoRuntime => "модели не загружены: не найдена библиотека ONNX Runtime",
            DegradedReason::NoModel => "модели не загружены",
            DegradedReason::GpuModelOnCpu => {
                "модели не загружены: модель для GPU, а CUDA недоступна"
            }
            DegradedReason::QdrantDown => "векторная база Qdrant запускается или недоступна",
            DegradedReason::Broken => "векторная база Qdrant выключена или не запущена",
            DegradedReason::QdrantLocked => "каталог Qdrant занят другим экземпляром сервера",
            DegradedReason::ModelMismatch => "индекс построен другой моделью, нужна переиндексация",
            DegradedReason::EmbedFailed => "не удалось вычислить эмбеддинг запроса",
            DegradedReason::SearchFailed => "запрос к векторной базе не удался",
        }
    }

    /// Что сделать администратору (команды — в обратных кавычках: WebUI выделяет их).
    pub fn hint(self) -> &'static str {
        match self {
            DegradedReason::NoRuntime => {
                "положите ONNX Runtime рядом с zam-server (комплект поставки) или задайте `ml.ort_dylib`, затем выполните `zam-server models fetch` и перезапустите сервер"
            }
            DegradedReason::NoModel => "выполните `zam-server models fetch` и перезапустите сервер",
            DegradedReason::GpuModelOnCpu => {
                "модель для GPU на CPU не запускается: установите модель для CPU `zam-server models fetch --device cpu` (или уберите `ml.embed_model`) и перезапустите сервер"
            }
            DegradedReason::QdrantDown => "Qdrant запускается или недоступен; очередь догонит сама",
            DegradedReason::Broken => {
                "Qdrant выключен супервизором или не запущен: см. журнал и `zam-server qdrant status`"
            }
            DegradedReason::QdrantLocked => {
                "каталог Qdrant занят другим экземпляром zam-server: остановите его и перезапустите сервер"
            }
            DegradedReason::ModelMismatch => {
                "индекс построен другой моделью: выполните reindex (векторы разных моделей не смешиваются)"
            }
            DegradedReason::EmbedFailed => {
                "эмбеддинг запроса не удался: см. журнал (ERROR/WARN `search`), FTS работает"
            }
            DegradedReason::SearchFailed => {
                "запрос к Qdrant не удался: см. журнал и `zam-server qdrant status`, FTS работает"
            }
        }
    }
}

/// Предупреждение `/search`: индекс пересобирается, векторная выдача может быть неполной.
pub const WARN_REINDEXING: &str = "index rebuilding";

/// См. [`IndexRuntime::reindexing`]; `storage` — флаг стёртого хранилища супервизора.
fn reindexing(ws: &zam_index::WorkerStatus, storage: bool) -> bool {
    ws.running && (ws.reindex_running || ws.reindex_queued > 0 || ws.rebuilding || storage)
}

/// Модель, Qdrant и воркер индексации (в `AppState` за `Arc`).
pub struct IndexRuntime {
    pub embedder: Option<Arc<Embedder>>,
    pub device: Option<DeviceReport>,
    /// Ошибка загрузки модели или ORT (текст без секретов).
    pub ml_error: Option<String>,
    ml_reason: Option<DegradedReason>,
    pub vector: Option<Arc<dyn VectorIndex>>,
    /// Ошибка запуска супервизора или подключения к Qdrant.
    pub qdrant_error: Option<String>,
    /// Причина деградации, если Qdrant не запущен (клиента нет).
    qdrant_reason: Option<DegradedReason>,
    /// Супервизор (managed / external); забирается при остановке.
    supervisor: Mutex<Option<Arc<Supervisor>>>,
    qdrant_state: Option<tokio::sync::watch::Receiver<QdrantState>>,
    pub worker: WorkerHandle,
}

impl std::fmt::Debug for IndexRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexRuntime")
            .field("model", &self.model_id())
            .field("device", &self.device)
            .field("ml_error", &self.ml_error)
            .field("qdrant", &self.qdrant_state())
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

impl IndexRuntime {
    /// Без модели и Qdrant (режим FTS-only).
    pub fn disabled(reason: DegradedReason, error: String) -> IndexRuntime {
        IndexRuntime {
            embedder: None,
            device: None,
            ml_error: Some(error),
            ml_reason: Some(reason),
            vector: None,
            qdrant_error: None,
            qdrant_reason: None,
            supervisor: Mutex::new(None),
            qdrant_state: None,
            worker: WorkerHandle::idle(),
        }
    }

    /// Супервизор Qdrant по `[qdrant]` (готовности не ждёт). Ошибка — причина деградации и
    /// текст для статуса (уже в журнале).
    pub async fn start_supervisor(
        cfg: &ServerConfig,
    ) -> Result<Arc<Supervisor>, (DegradedReason, String)> {
        let sup_cfg = crate::qdrant::exe_dir()
            .map_err(|e| e.to_string())
            .and_then(|d| cfg.qdrant.supervisor_config(&d).map_err(|e| e.to_string()));
        match sup_cfg {
            Ok(Some(sc)) => match Supervisor::start(sc, &cfg.data_dir).await {
                Ok(s) => Ok(Arc::new(s)),
                Err(e) => {
                    tracing::warn!(error = %e, "супервизор Qdrant не запущен: векторный поиск выключен");
                    let reason = match e {
                        IndexError::Locked(_) => DegradedReason::QdrantLocked,
                        _ => DegradedReason::Broken,
                    };
                    Err((reason, e.to_string()))
                }
            },
            Ok(None) => {
                let msg = "qdrant.mode = \"edge\" пока не поддерживается сервером".to_string();
                tracing::warn!("{msg}: векторный поиск выключен");
                Err((DegradedReason::Broken, msg))
            }
            Err(e) => {
                tracing::warn!(error = %e, "конфигурация Qdrant: векторный поиск выключен");
                Err((DegradedReason::Broken, e))
            }
        }
    }

    /// Запускает Qdrant (если модель загружена) и воркер. `early` — супервизор, запущенный
    /// параллельно с загрузкой модели ([`IndexRuntime::start_supervisor`]): Qdrant
    /// поднимается, пока модель читается с диска (§17: готовность Qdrant ≤ 5 с от старта).
    /// Модель не загрузилась — такой супервизор останавливается. Ошибки пишутся в лог и в
    /// состояние, сервер стартует в любом случае.
    pub async fn start(
        cfg: &ServerConfig,
        ml: MlLoad,
        early: Option<Result<Arc<Supervisor>, (DegradedReason, String)>>,
        system: &SystemStore,
        users: &UserDbs,
    ) -> IndexRuntime {
        let Some(embedder) = ml.embedder else {
            let (reason, err) = ml
                .error
                .unwrap_or((DegradedReason::NoModel, "модель не загружена".into()));
            let mut rt = IndexRuntime::disabled(reason, err);
            rt.device = ml.device;
            if let Some(Ok(sup)) = early {
                tracing::info!("модель не загрузилась: Qdrant останавливается");
                match Arc::try_unwrap(sup) {
                    Ok(s) => {
                        s.shutdown().await;
                    }
                    // клонов здесь нет (супервизор ещё никому не отдан); если появятся — Drop
                    // последнего клона шлёт стоп, но без ожидания остановки процесса
                    Err(s) => {
                        tracing::warn!(
                            refs = Arc::strong_count(&s),
                            "супервизор Qdrant ещё используется: остановка без ожидания (Drop)"
                        );
                        drop(s);
                    }
                }
            } else {
                tracing::info!("Qdrant не запускается: без модели векторный индекс не нужен");
            }
            return rt;
        };
        let mut rt = IndexRuntime {
            embedder: Some(embedder.clone()),
            device: ml.device,
            ml_error: None,
            ml_reason: None,
            vector: None,
            qdrant_error: None,
            qdrant_reason: None,
            supervisor: Mutex::new(None),
            qdrant_state: None,
            worker: WorkerHandle::idle(),
        };
        let started = match early {
            Some(r) => r,
            None => Self::start_supervisor(cfg).await,
        };
        let sup = match started {
            Ok(s) => s,
            Err((reason, msg)) => {
                rt.qdrant_reason = Some(reason);
                rt.qdrant_error = Some(msg);
                return rt;
            }
        };
        let vector: Arc<dyn VectorIndex> = match sup.connect_index() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                tracing::warn!(error = %e, "клиент Qdrant: векторный поиск выключен");
                rt.qdrant_reason = Some(DegradedReason::Broken);
                rt.qdrant_error = Some(e.to_string());
                *rt.supervisor.get_mut().unwrap_or_else(|p| p.into_inner()) = Some(sup);
                return rt;
            }
        };
        rt.qdrant_state = Some(sup.state());
        let wcfg = WorkerConfig {
            batch_tokens: cfg.index.batch_tokens,
            yield_ms: cfg.index.yield_ms,
            rollup_debounce: Duration::from_millis(cfg.index.rollup_debounce_ms),
            cache_new_sources_threshold: cfg.cache.new_sources_threshold,
            ..WorkerConfig::default()
        };
        rt.worker = WorkerHandle::spawn(
            WorkerDeps {
                system: system.clone(),
                users: users.clone(),
                embedder: Arc::new(EmbedderAdapter(embedder)),
                vector: vector.clone(),
                supervisor: Some(sup.clone()),
            },
            wcfg,
        );
        rt.vector = Some(vector);
        *rt.supervisor.get_mut().unwrap_or_else(|p| p.into_inner()) = Some(sup);
        rt
    }

    /// `model_id` текущей модели.
    pub fn model_id(&self) -> Option<&str> {
        self.embedder.as_deref().map(Embedder::model_id)
    }

    /// Состояние Qdrant (`None` — супервизор не запущен).
    pub fn qdrant_state(&self) -> Option<QdrantState> {
        self.qdrant_state.as_ref().map(|r| r.borrow().clone())
    }

    /// Супервизор Qdrant (pid, ключ, состояние для `/index/status`).
    pub fn supervisor(&self) -> Option<Arc<Supervisor>> {
        self.supervisor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// `(модель коллекции, текущая модель)`, если не совпадают (Review Focus 3).
    pub fn model_mismatch(&self) -> Option<(String, String)> {
        self.worker.status().model_mismatch
    }

    /// Старый родитель перенесённого узла: пересчитать его свёртку L0 (воркер знает
    /// только новое положение узла).
    pub fn rollup_hint(&self, user: zam_core::Id, folder: zam_core::Id) {
        self.worker.rollup_hint(user, folder);
    }

    /// Будит воркер после записи узла.
    pub fn notify(&self) {
        self.worker.notify();
    }

    /// Индекс пересобирается: reindex по запросу (из кэша или полный) выполняется или ждёт,
    /// пересборка по всем базам не закончила постановку заданий, или хранилище Qdrant стёрто
    /// и ещё не восстановлено (`Ready { rebuilt }`). Векторная выдача в это время неполна —
    /// `/search` добавляет предупреждение [`WARN_REINDEXING`].
    pub fn reindexing(&self) -> bool {
        let storage = self.supervisor().is_some_and(|s| s.storage_rebuilt());
        reindexing(&self.worker.status(), storage)
    }

    /// Можно ли идти в векторный канал; иначе — причина деградации.
    pub fn vector_available(&self) -> Result<(), DegradedReason> {
        if self.embedder.is_none() {
            return Err(self.ml_reason.unwrap_or(DegradedReason::NoModel));
        }
        if self.vector.is_none() {
            return Err(self.qdrant_reason.unwrap_or(DegradedReason::QdrantDown));
        }
        match self.qdrant_state() {
            Some(QdrantState::Broken { .. }) => return Err(DegradedReason::Broken),
            // процесс уже мёртв, а супервизор ещё не сменил состояние — сразу FTS-only
            Some(QdrantState::Ready { .. }) => {
                if self.supervisor().is_some_and(|s| !s.process_alive()) {
                    return Err(DegradedReason::QdrantDown);
                }
            }
            Some(QdrantState::External) | None => {}
            Some(_) => return Err(DegradedReason::QdrantDown),
        }
        let st = self.worker.status();
        if st.model_mismatch.is_some() {
            return Err(DegradedReason::ModelMismatch);
        }
        // Воркер завершился (паника): сам не поднимется, «очередь догонит» неверно.
        if !st.running {
            return Err(DegradedReason::Broken);
        }
        if !st.vector_ready {
            return Err(DegradedReason::QdrantDown);
        }
        Ok(())
    }

    /// Остановка: воркер, разовая перечанковка R1 (ждём не дольше 10 с: она идемпотентна
    /// и продолжится при следующем открытии), затем Qdrant (`wait=true`-записи завершены).
    pub async fn shutdown(&self, system: &SystemStore, users: &UserDbs) {
        self.worker.shutdown().await;
        if let Ok(list) = system.list_users().await {
            let wait = async {
                for u in &list {
                    users.wait_rechunk(u.id).await;
                }
            };
            if tokio::time::timeout(Duration::from_secs(10), wait)
                .await
                .is_err()
            {
                tracing::warn!(
                    "перечанковка после миграции не завершена: продолжится при следующем запуске"
                );
            }
        }
        let sup = self
            .supervisor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(sup) = sup {
            match Arc::try_unwrap(sup) {
                Ok(s) => {
                    s.shutdown().await;
                }
                // клон ещё жив (например, задача воркера прервана): Drop супервизора шлёт стоп
                Err(s) => {
                    tracing::warn!("супервизор Qdrant ещё используется: остановка без ожидания");
                    drop(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindexing_from_worker_status_and_storage_flag() {
        let idle = zam_index::WorkerStatus {
            running: true,
            ..Default::default()
        };
        assert!(!reindexing(&idle, false));
        assert!(
            reindexing(&idle, true),
            "хранилище стёрто, ещё не восстановлено"
        );
        for ws in [
            zam_index::WorkerStatus {
                reindex_running: true,
                ..idle.clone()
            },
            zam_index::WorkerStatus {
                reindex_queued: 1,
                ..idle.clone()
            },
            zam_index::WorkerStatus {
                rebuilding: true,
                ..idle.clone()
            },
        ] {
            assert!(reindexing(&ws, false), "{ws:?}");
        }
        // воркера нет (FTS-only): предупреждать не о чем, причина — в degraded
        assert!(!reindexing(&zam_index::WorkerStatus::default(), true));
    }
}
