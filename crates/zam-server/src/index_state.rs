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
use zam_index::{QdrantIndex, VectorIndex, WorkerConfig, WorkerDeps, WorkerHandle};
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
    /// Qdrant запускается, перезапускается, недоступен или коллекция ещё не проверена.
    QdrantDown,
    /// Qdrant выключен супервизором (circuit breaker, чужая версия, нет бинарника).
    Broken,
    /// `model_id` коллекции не совпадает с текущей моделью: нужен reindex.
    ModelMismatch,
}

impl DegradedReason {
    /// Значение `degraded_reason` в API.
    pub fn as_str(self) -> &'static str {
        match self {
            DegradedReason::NoRuntime => "no_runtime",
            DegradedReason::NoModel => "no_model",
            DegradedReason::QdrantDown => "qdrant_down",
            DegradedReason::Broken => "broken",
            DegradedReason::ModelMismatch => "model_mismatch",
        }
    }

    /// Что сделать администратору.
    pub fn hint(self) -> &'static str {
        match self {
            DegradedReason::NoRuntime => {
                "положите ONNX Runtime рядом с zam-server (комплект поставки) или задайте ml.ort_dylib"
            }
            DegradedReason::NoModel => "выполните `zam-server models fetch` и перезапустите сервер",
            DegradedReason::QdrantDown => "Qdrant запускается или недоступен; очередь догонит сама",
            DegradedReason::Broken => {
                "Qdrant выключен супервизором: см. журнал и `zam-server qdrant status`"
            }
            DegradedReason::ModelMismatch => {
                "индекс построен другой моделью: выполните reindex (векторы разных моделей не смешиваются)"
            }
        }
    }
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
            supervisor: Mutex::new(None),
            qdrant_state: None,
            worker: WorkerHandle::idle(),
        }
    }

    /// Запускает Qdrant (если модель загружена) и воркер. Ошибки пишутся в лог и в
    /// состояние, сервер стартует в любом случае.
    pub async fn start(
        cfg: &ServerConfig,
        ml: MlLoad,
        system: &SystemStore,
        users: &UserDbs,
    ) -> IndexRuntime {
        let Some(embedder) = ml.embedder else {
            let (reason, err) = ml
                .error
                .unwrap_or((DegradedReason::NoModel, "модель не загружена".into()));
            let mut rt = IndexRuntime::disabled(reason, err);
            rt.device = ml.device;
            tracing::info!("Qdrant не запускается: без модели векторный индекс не нужен");
            return rt;
        };
        let mut rt = IndexRuntime {
            embedder: Some(embedder.clone()),
            device: ml.device,
            ml_error: None,
            ml_reason: None,
            vector: None,
            qdrant_error: None,
            supervisor: Mutex::new(None),
            qdrant_state: None,
            worker: WorkerHandle::idle(),
        };
        let sup_cfg = crate::qdrant::exe_dir()
            .map_err(|e| e.to_string())
            .and_then(|d| cfg.qdrant.supervisor_config(&d).map_err(|e| e.to_string()));
        let sup = match sup_cfg {
            Ok(Some(sc)) => match Supervisor::start(sc, &cfg.data_dir).await {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!(error = %e, "супервизор Qdrant не запущен: векторный поиск выключен");
                    rt.qdrant_error = Some(e.to_string());
                    return rt;
                }
            },
            Ok(None) => {
                let msg = "qdrant.mode = \"edge\" пока не поддерживается сервером".to_string();
                tracing::warn!("{msg}: векторный поиск выключен");
                rt.qdrant_error = Some(msg);
                return rt;
            }
            Err(e) => {
                tracing::warn!(error = %e, "конфигурация Qdrant: векторный поиск выключен");
                rt.qdrant_error = Some(e);
                return rt;
            }
        };
        let api_key = sup.api_key();
        let vector: Arc<dyn VectorIndex> =
            match QdrantIndex::connect(&sup.grpc_url(), api_key.as_deref()) {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    tracing::warn!(error = %e, "клиент Qdrant: векторный поиск выключен");
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

    /// Будит воркер после записи узла.
    pub fn notify(&self) {
        self.worker.notify();
    }

    /// Можно ли идти в векторный канал; иначе — причина деградации.
    pub fn vector_available(&self) -> Result<(), DegradedReason> {
        if self.embedder.is_none() {
            return Err(self.ml_reason.unwrap_or(DegradedReason::NoModel));
        }
        if self.vector.is_none() {
            return Err(DegradedReason::QdrantDown);
        }
        match self.qdrant_state() {
            Some(QdrantState::Broken { .. }) => return Err(DegradedReason::Broken),
            Some(QdrantState::Ready { .. } | QdrantState::External) | None => {}
            Some(_) => return Err(DegradedReason::QdrantDown),
        }
        let st = self.worker.status();
        if st.model_mismatch.is_some() {
            return Err(DegradedReason::ModelMismatch);
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
                Ok(s) => s.shutdown().await,
                // клон ещё жив (например, задача воркера прервана): Drop супервизора шлёт стоп
                Err(s) => {
                    tracing::warn!("супервизор Qdrant ещё используется: остановка без ожидания");
                    drop(s);
                }
            }
        }
    }
}
