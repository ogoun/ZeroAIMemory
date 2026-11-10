//! Конфигурация сервера. Слои: умолчания, TOML-файл, `ZAM_*`, CLI.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString, rand_core::OsRng};
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::{Table, Value};
use zam_search::KindBoostMode;
use zam_search::recall::CoverageRule;

/// Переменные `ZAM_*`, которые не являются слоем конфигурации:
/// секреты и адрес клиента, переменные клиента `zam` (сессия, признак агента, имя машины, сессия `zam mcp` —
/// `zam_core::CLIENT_ENV_VARS`: без этого `ZAM_MACHINE` в окружении пользователя ломала бы
/// ручной запуск сервера), а также сборочные (build.rs, xtask), попадающие в окружение CI.
const RESERVED_ENV: [&str; 13] = [
    "ZAM_ADMIN_PASSWORD",
    "ZAM_URL",
    "ZAM_TOKEN",
    "ZAM_MASTER_KEY",
    "ZAM_GIT_SHA",
    "ZAM_WEB_REBUILD",
    "ZAM_WEB_INDEX",
    "ZAM_VCRUNTIME_DLL",
    "ZAM_VC_CRT_DIR",
    zam_core::CLIENT_ENV_VARS[0],
    zam_core::CLIENT_ENV_VARS[1],
    zam_core::CLIENT_ENV_VARS[2],
    zam_core::CLIENT_ENV_VARS[3],
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerSection,
    pub admin: AdminSection,
    pub data_dir: PathBuf,
    pub log: LogSection,
    pub qdrant: QdrantSection,
    pub ml: MlSection,
    pub index: IndexSection,
    pub search: SearchSection,
    pub insights: InsightsSection,
    pub read: ReadSection,
    pub cache: CacheSection,
    pub recall: RecallSection,
    pub limits: LimitsSection,
    pub vault: VaultSection,
    pub docs: DocsSection,
    pub reports: ReportsSection,
    pub code: CodeSection,
    pub ideas: IdeasSection,
    pub backup: BackupSection,
    pub export: ExportSection,
    pub import: ImportSection,
    pub snapshot: SnapshotSection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
    /// "" или "/memory": с ведущим "/", без завершающего.
    pub base_path: String,
    #[serde(serialize_with = "ser_nets", deserialize_with = "de_nets")]
    pub trusted_proxies: Vec<IpNet>,
    /// Каталог клиентов `zam` для `GET /downloads/zam/{os}-{arch}` (§15.5): `zam[.exe]` и
    /// `downloads/<платформа>/zam[.exe]`; по умолчанию — каталог исполняемого файла сервера.
    pub downloads_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    pub login: String,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    pub level: String,
    pub format: LogFormat,
    pub max_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QdrantMode {
    Managed,
    External,
    Edge,
}

/// Секция `[qdrant]` (§4.3). Супервизор строится из неё через [`QdrantSection::supervisor_config`].
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QdrantSection {
    pub mode: QdrantMode,
    /// gRPC-адрес внешнего Qdrant (`mode = "external"`), например `http://127.0.0.1:6334`.
    pub url: Option<String>,
    pub http_port: u16,
    pub grpc_port: u16,
    /// Бинарник управляемого Qdrant; по умолчанию `<exe_dir>/qdrant/<ver>/qdrant[.exe]`.
    pub binary: Option<PathBuf>,
    /// `storage.performance.max_search_threads` (0 — авто): не отбирать CPU у инференса.
    pub max_search_threads: u32,
    /// `storage.performance.optimizer_cpu_budget` (0 — авто).
    pub optimizer_cpu_budget: u32,
    /// API-ключ внешнего Qdrant. Для `managed` не задаётся: ключ случайный при каждом старте.
    pub api_key: Option<String>,
    /// `optimizers.flush_interval_sec` коллекции managed Qdrant (1..=60): как часто Qdrant
    /// сбрасывает сегменты и подтверждает WAL; меньше — короче повтор WAL после жёсткой
    /// остановки (задача 19 M3).
    pub flush_interval_sec: u64,
    /// `wal.wal_capacity_mb` новой коллекции, МБ (1..=1024); у существующей — после
    /// `zam-server qdrant reset`.
    pub wal_capacity_mb: u64,
    /// Предел ожидания сброса WAL при штатной остановке после записи, мс (0..=60000;
    /// 0 — не ждать).
    pub shutdown_flush_wait_ms: u64,
    /// Точный поиск без HNSW и квантования (`SearchParams.exact`): только для прогонов гейта
    /// качества `cargo xtask eval` (задача 15 M4, §18.2), в продакшне — `false` (HNSW).
    pub exact_search: bool,
}

// Вручную: ключ внешнего Qdrant не должен попадать в отладочный вывод.
impl std::fmt::Debug for QdrantSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QdrantSection")
            .field("mode", &self.mode)
            .field("url", &self.url)
            .field("http_port", &self.http_port)
            .field("grpc_port", &self.grpc_port)
            .field("binary", &self.binary)
            .field("max_search_threads", &self.max_search_threads)
            .field("optimizer_cpu_budget", &self.optimizer_cpu_budget)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("flush_interval_sec", &self.flush_interval_sec)
            .field("wal_capacity_mb", &self.wal_capacity_mb)
            .field("shutdown_flush_wait_ms", &self.shutdown_flush_wait_ms)
            .field("exact_search", &self.exact_search)
            .finish()
    }
}

impl QdrantSection {
    /// Бинарник управляемого Qdrant: из конфигурации или из комплекта рядом с `exe_dir`.
    pub fn binary_path(&self, exe_dir: &Path) -> PathBuf {
        self.binary
            .clone()
            .unwrap_or_else(|| zam_index::supervisor::default_binary(exe_dir))
    }

    /// Конфигурация супервизора; `None` — режим `edge` (экспериментальный, без процесса).
    pub fn supervisor_config(
        &self,
        exe_dir: &Path,
    ) -> Result<Option<zam_index::supervisor::SupervisorConfig>, ConfigError> {
        use zam_index::supervisor::{ManagedConfig, SupervisorConfig};
        let invalid = |key: &str, message: &str| ConfigError::Invalid {
            key: key.into(),
            source_name: "итог".into(),
            message: message.into(),
        };
        match self.mode {
            QdrantMode::Managed => {
                if self.api_key.is_some() {
                    return Err(invalid(
                        "qdrant.api_key",
                        "задаётся только для mode = \"external\": управляемый Qdrant получает случайный ключ при каждом старте",
                    ));
                }
                if self.http_port == self.grpc_port {
                    return Err(invalid("qdrant.grpc_port", "совпадает с http_port"));
                }
                if !(1..=60).contains(&self.flush_interval_sec) {
                    return Err(invalid("qdrant.flush_interval_sec", "от 1 до 60"));
                }
                if !(1..=1024).contains(&self.wal_capacity_mb) {
                    return Err(invalid("qdrant.wal_capacity_mb", "от 1 до 1024"));
                }
                if self.shutdown_flush_wait_ms > 60_000 {
                    return Err(invalid("qdrant.shutdown_flush_wait_ms", "от 0 до 60000"));
                }
                let mut m = ManagedConfig::new(self.binary_path(exe_dir));
                m.http_port = self.http_port;
                m.grpc_port = self.grpc_port;
                m.max_search_threads = self.max_search_threads;
                m.optimizer_cpu_budget = self.optimizer_cpu_budget;
                m.flush_interval_sec = self.flush_interval_sec;
                m.wal_capacity_mb = self.wal_capacity_mb;
                m.shutdown_flush_wait =
                    std::time::Duration::from_millis(self.shutdown_flush_wait_ms);
                m.exact_search = self.exact_search;
                Ok(Some(SupervisorConfig::Managed(m)))
            }
            QdrantMode::External => {
                let url = self
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .ok_or_else(|| {
                        invalid(
                            "qdrant.url",
                            "обязателен для mode = \"external\" (gRPC-адрес)",
                        )
                    })?;
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(invalid("qdrant.url", "ожидается http:// или https://"));
                }
                let mut cfg =
                    SupervisorConfig::external(url, self.api_key.clone().filter(|k| !k.is_empty()));
                if let SupervisorConfig::External(x) = &mut cfg {
                    x.exact_search = self.exact_search;
                }
                Ok(Some(cfg))
            }
            QdrantMode::Edge => Ok(None),
        }
    }
}

/// Устройство инференса (`ml.device`, §8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MlDevice {
    /// CUDA, если доступна, иначе CPU.
    Auto,
    Cpu,
    Cuda,
}

impl From<MlDevice> for zam_ml::DevicePref {
    fn from(d: MlDevice) -> Self {
        match d {
            MlDevice::Auto => zam_ml::DevicePref::Auto,
            MlDevice::Cpu => zam_ml::DevicePref::Cpu,
            MlDevice::Cuda => zam_ml::DevicePref::Cuda,
        }
    }
}

/// Секция `[ml]` (§8.4): модель эмбеддингов и ONNX Runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlSection {
    pub device: MlDevice,
    /// Каталог моделей; по умолчанию `<data_dir>/models`.
    pub models_dir: Option<PathBuf>,
    /// Модель эмбеддингов; по умолчанию `bge-m3-fp16` в GPU-комплекте при доступной CUDA,
    /// иначе `bge-m3-int8`.
    pub embed_model: Option<String>,
    /// Библиотека ONNX Runtime; по умолчанию рядом с исполняемым файлом.
    pub ort_dylib: Option<PathBuf>,
    /// Потоки сессии индексации (0 — авто).
    pub index_threads: usize,
    /// Потоки сессии запросов.
    pub query_threads: usize,
    /// Выгрузка модели после простоя: `""` — выключено, иначе `<число>s|m|h`.
    pub unload_after: String,
    /// Реранкер при эмбеддере на CPU (`rerank=on`, §9.2 п. 5); `jina-reranker-v2` —
    /// CC-BY-NC-4.0, ставится только `models fetch --reranker --accept-nc-license`.
    pub rerank_model_cpu: String,
    /// Реранкер при эмбеддере на CUDA (Apache-2.0, входит в `zam-models`).
    pub rerank_model_gpu: String,
    /// Сколько первых кандидатов слияния переранжируется (1..=100).
    pub rerank_top: usize,
    /// Предел токенов фрагмента в паре реранкера (8..=1024).
    pub rerank_tokens: usize,
    /// Параллельных сессий реранкера с динамическим int8 (jina): 0 — авто (до 2, не меньше
    /// двух физических ядер на сессию), иначе 1..=16. Ядра `index_threads` делятся между
    /// сессиями; каждая сессия держит свою копию весов (~0,3 ГБ у jina).
    pub rerank_sessions: usize,
}

impl MlSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("ml.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        self.unload_after()?;
        for (k, v) in [
            ("rerank_model_cpu", &self.rerank_model_cpu),
            ("rerank_model_gpu", &self.rerank_model_gpu),
        ] {
            let ok = !v.is_empty()
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                && v != "."
                && v != "..";
            if !ok {
                return Err(bad(k, "id модели: латиница, цифры, '-', '_', '.'"));
            }
        }
        if !(1..=100).contains(&self.rerank_top) {
            return Err(bad("rerank_top", "от 1 до 100"));
        }
        if !(8..=1024).contains(&self.rerank_tokens) {
            return Err(bad("rerank_tokens", "от 8 до 1024"));
        }
        if self.rerank_sessions > 16 {
            return Err(bad("rerank_sessions", "0 (авто) или от 1 до 16"));
        }
        Ok(())
    }

    /// Каталог моделей с учётом умолчания.
    pub fn models_dir(&self, data_dir: &Path) -> PathBuf {
        self.models_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("models"))
    }

    /// `unload_after` как длительность; `None` — выключено.
    pub fn unload_after(&self) -> Result<Option<std::time::Duration>, ConfigError> {
        let raw = self.unload_after.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let invalid = || ConfigError::Invalid {
            key: "ml.unload_after".into(),
            source_name: "итог".into(),
            message: format!("ожидается \"\" или <число>s|m|h, получено `{raw}`"),
        };
        // последний символ — единица; многобайтовый символ («5м») — ошибка, не паника
        let Some((at, unit)) = raw.char_indices().last() else {
            return Err(invalid());
        };
        let n: u64 = raw[..at].trim().parse().map_err(|_| invalid())?;
        let mul = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return Err(invalid()),
        };
        let secs = n.checked_mul(mul).ok_or_else(invalid)?;
        if secs == 0 {
            return Err(invalid());
        }
        Ok(Some(std::time::Duration::from_secs(secs)))
    }
}

/// Секция `[index]` (§8.1): воркер индексации.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSection {
    /// Предел токенов одного вызова инференса.
    pub batch_tokens: usize,
    /// Уступка между пачками, мс.
    pub yield_ms: u64,
    /// Дебаунс свёртки L0 папок, мс.
    pub rollup_debounce_ms: u64,
    /// Чанкинг §8.2, токены счётчика модели: лист набирается до `chunk_target_min..=
    /// chunk_target_max`, недобравший может вырасти до `chunk_hard_max` (предел любого листа).
    pub chunk_target_min: usize,
    pub chunk_target_max: usize,
    pub chunk_hard_max: usize,
    /// Секция короче склеивается с соседней.
    pub chunk_merge_below: usize,
    /// Предложений перекрытия соседних листов секции.
    pub chunk_overlap_sentences: usize,
    /// Вес силы границы λ при выборе разреза листа (задача 17 M3): 0 — жадная упаковка M2
    /// (с исправлениями висячего заголовка и вводной фразы), больше 0 — динамическое
    /// программирование по силе границ. По умолчанию 0 — решение владельца 2026-10-26 (M4,
    /// §8.2): собственный эффект λ = 0,5 поверх структурных исправлений M3 от нуля не отличим
    /// (Δ nDCG@10 +0,003 [−0,003…+0,010]); в M3 умолчание было 0,5.
    pub chunk_boundary_lambda: f64,
    /// Лексическая связность прозы (основы слов) в силе границы при λ > 0.
    pub chunk_cohesion: bool,
    /// Эксперимент: L0 заметки (до 50 токенов) в контекстном префиксе листов.
    pub chunk_prefix_l0: bool,
    /// H4–H6 в пути заголовков листа и тем самым в контекстном префиксе (`false` — путь M2
    /// по H1–H3; переключатель сетки eval).
    pub chunk_heading_path_h4: bool,
}

/// Предел `chunk_boundary_lambda`.
pub const CHUNK_LAMBDA_MAX: f64 = 10.0;

/// Санитарный предел `chunk_hard_max` в конфигурации. Действующий предел задаёт модель:
/// лист с префиксом контекста и спецтокенами должен уместиться в её окно при индексации
/// (`max_tokens_passage` минус [`CHUNK_PREFIX_BUDGET`], см. [`IndexSection::chunk_params_for`]).
pub const CHUNK_HARD_MAX_CAP: usize = 8192;
/// Бюджет префикса контекста (`[space/path] имя > заголовки | теги`) и спецтокенов в окне
/// модели, токенов.
pub const CHUNK_PREFIX_BUDGET: usize = 64;
/// Окно модели, когда она неизвестна (токенизатора нет, счёт оценкой): bge-m3 — 1024.
pub const DEFAULT_PASSAGE_WINDOW: usize = 1024;
/// Предел `chunk_overlap_sentences`.
pub const CHUNK_OVERLAP_LIMIT: usize = 5;

impl IndexSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: String| ConfigError::Invalid {
            key: format!("index.{key}"),
            source_name: "итог".into(),
            message,
        };
        if self.batch_tokens == 0 {
            return Err(bad("batch_tokens", "должно быть больше 0".into()));
        }
        if !(1..=CHUNK_HARD_MAX_CAP).contains(&self.chunk_hard_max) {
            return Err(bad(
                "chunk_hard_max",
                format!("от 1 до {CHUNK_HARD_MAX_CAP}"),
            ));
        }
        if !(1..=self.chunk_hard_max).contains(&self.chunk_target_max) {
            return Err(bad("chunk_target_max", "от 1 до chunk_hard_max".into()));
        }
        if !(1..=self.chunk_target_max).contains(&self.chunk_target_min) {
            return Err(bad("chunk_target_min", "от 1 до chunk_target_max".into()));
        }
        if self.chunk_merge_below > self.chunk_target_max {
            return Err(bad(
                "chunk_merge_below",
                "не больше chunk_target_max".into(),
            ));
        }
        if self.chunk_overlap_sentences > CHUNK_OVERLAP_LIMIT {
            return Err(bad(
                "chunk_overlap_sentences",
                format!("от 0 до {CHUNK_OVERLAP_LIMIT}"),
            ));
        }
        if !(self.chunk_boundary_lambda.is_finite()
            && (0.0..=CHUNK_LAMBDA_MAX).contains(&self.chunk_boundary_lambda))
        {
            return Err(bad(
                "chunk_boundary_lambda",
                format!("от 0 до {CHUNK_LAMBDA_MAX}"),
            ));
        }
        Ok(())
    }

    /// Пороги чанкера для модели с окном `window` токенов (`max_tokens_passage`; `None` —
    /// модель неизвестна, [`DEFAULT_PASSAGE_WINDOW`]): `chunk_hard_max` не больше окна минус
    /// [`CHUNK_PREFIX_BUDGET`] (и минус L0 в префиксе при `chunk_prefix_l0`), остальные
    /// пороги — не больше него. Второе значение — `true`, если пороги конфигурации пришлось
    /// уменьшить.
    pub fn chunk_params_for(&self, window: Option<usize>) -> (zam_markdown::ChunkParams, bool) {
        let want = self.chunk_params();
        let l0 = if self.chunk_prefix_l0 {
            zam_store::PREFIX_L0_MAX_TOKENS
        } else {
            0
        };
        let limit = window
            .unwrap_or(DEFAULT_PASSAGE_WINDOW)
            .saturating_sub(CHUNK_PREFIX_BUDGET + l0)
            .max(1);
        let mut p = want;
        p.hard_max = p.hard_max.min(limit);
        p.target_max = p.target_max.min(p.hard_max);
        p.target_min = p.target_min.min(p.target_max);
        p.merge_below = p.merge_below.min(p.target_max);
        (p, p != want)
    }

    /// Пороги чанкера из конфигурации как есть (без учёта окна модели).
    pub fn chunk_params(&self) -> zam_markdown::ChunkParams {
        zam_markdown::ChunkParams {
            target_min: self.chunk_target_min,
            target_max: self.chunk_target_max,
            hard_max: self.chunk_hard_max,
            merge_below: self.chunk_merge_below,
            overlap_sentences: self.chunk_overlap_sentences,
            boundary_lambda: self.chunk_boundary_lambda,
            cohesion: self.chunk_cohesion,
            heading_path_h4: self.chunk_heading_path_h4,
        }
    }

    /// `chunk_cohesion = true` при `chunk_boundary_lambda = 0`: связность входит только в силу
    /// границы DP, а при λ = 0 работает жадная упаковка — флаг ничего не меняет (при старте —
    /// предупреждение в журнал).
    pub fn cohesion_ignored(&self) -> bool {
        self.chunk_cohesion && self.chunk_boundary_lambda == 0.0
    }
}

/// Секция `[search]` (§9.2): слияние каналов weighted RRF и поправки.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSection {
    /// Константа RRF (> 0).
    pub rrf_k: f64,
    /// Вес векторного канала (Qdrant: dense + sparse + сводки).
    pub w_vector: f64,
    /// Вес канала FTS5 `bm25`, когда нашлись чанки со всеми словами запроса.
    pub w_fts: f64,
    /// Вес `bm25`, когда сработал только ослабленный запрос (любое слово): совпадения по
    /// одному слову — слабый сигнал (по умолчанию 0,25 — подобран на eval, §18.2).
    pub w_fts_relaxed: f64,
    /// Вес канала `alias` (M4.5): FTS по запросу, расширенному именами и псевдонимами терминов
    /// словаря; 0..=1 (по умолчанию 0,5 — ниже исходного канала), 0 выключает канал.
    pub w_fts_alias: f64,
    /// Вес канала FTS5 `trigram` (идентификаторы и код).
    pub w_trigram: f64,
    /// Полупериод затухания по свежести, дней; 0 — без затухания.
    pub recency_half_life_days: f64,
    /// Не больше чанков с одного узла (≥ 1).
    pub max_per_node: usize,
    /// Не больше фрагментов с одного документа вложения (≥ 1, по умолчанию 2; M5): квота
    /// заметки-владельца (`max_per_node`) — своя, вложение её не занимает.
    pub max_per_attachment: usize,
    /// Окно prefetch фрагментов вложений гибридного поиска (dense и sparse, M5; 1..=500, по
    /// умолчанию 30 — `zam_index::ATTACH_PREFETCH`, вдвое при `area`/`certainty`). Подбор — eval
    /// документов задачи 10 M5 (`eval/results/2026-11-02-m5-docs-summary.md`).
    pub attach_prefetch: usize,
    /// Множители слитого балла (§9.2 п. 4) для insight вида `gotcha` и `failure` и для узлов и
    /// записей текущей области (`X-Zam-Area`). По умолчанию 1,05 / 1,05 / 1,0. Буст вида — сетка
    /// eval задачи 18 M3: буст вида должен быть ниже буста `theorem` (1,10), иначе гипотеза `gotcha` обходит
    /// противоречащую ей теорему (при 1,15 и 1,10 `reliable_first` 0,857, при 1,05 — 1,000);
    /// 1,05 против 1,0 — Δ nDCG@10 −0,001 [−0,004…+0,003] и сохраняет подъём `failure`-записи над
    /// заметками (`s-29`). Буст области 1,0 (без буста) — сетка задачи 11 M6 по набору областей
    /// (`eval/area-queries.toml`, решение контролёра): при 1,10 запросы с ответом без области
    /// теряют −0,110 nDCG@10 (5 из 12 хуже), с ответом в другой области — −0,026, а выигрыш своей
    /// области (+0,035, 4/0) по знаковому тесту незначим (p = 0,0625); весь набор 1,0 − 1,10 =
    /// +0,033. Прежние 1,10 (M3) — без замера.
    pub boost_gotcha: f64,
    pub boost_failure: f64,
    pub boost_area: f64,
    /// Множители по эпистемическому статусу `certainty` заметок и записей (§7.5, §9.2 п. 4):
    /// `axiom` 1,20, `theorem` 1,10, `hypothesis` 1,00; без статуса — 1. Подтверждено сеткой eval
    /// задачи 18 M3 (1,10/1,05 и 1,0 — ниже `reliable_first` и nDCG@10).
    pub boost_axiom: f64,
    pub boost_theorem: f64,
    pub boost_hypothesis: f64,
    /// `expand=graph` (§8.5, §9.2 п. 7): балл соседа по графу — балл найденного элемента,
    /// умноженный на этот вес (0..=1, по умолчанию 0,5).
    pub graph_weight: f64,
    /// Вес канала папок (§8.3, §9.2): найденная векторным поиском папка поднимает чанки
    /// узлов своего поддерева на `w_folder / (rrf_k + ранг папки)`. По умолчанию 0 — канал
    /// выключен по сетке eval задачи 18 M3: 0,3 / 0,5 / 1,0 устойчиво хуже (Δ nDCG@10 −0,052 и
    /// ниже) — подъём узлов обходит записи журналов, у которых папки нет.
    pub w_folder: f64,
    /// Не больше карточек папок в выдаче при `filters.kinds ∋ folder` (по умолчанию 2).
    pub max_folders: usize,
    /// Порядок по связи `contradicts` (M4.5, §5.2): после реранка и бустов запись не стоит выше
    /// действующей более надёжной записи (`theorem`/`axiom`), которой противоречит, если обе в
    /// выдаче (axiom → axiom и theorem → theorem не переставляются). `false` — порядок
    /// реранкера без поправки (по умолчанию `true`).
    pub contradicts_order: bool,
    /// Когда действует буст вида insight `boost_gotcha`/`boost_failure` (M6 §10): `always` —
    /// всегда (v0.0.9); `unless_contradicted` — не у записи, которой противоречит связью
    /// `contradicts` действующая более надёжная запись (`theorem`/`axiom`) среди всех кандидатов
    /// после слияния, до отсечения `top_k`; `reliable_only` — только у `theorem`/`axiom`.
    /// По умолчанию `always` — сетка задачи 10 M6 (журнал).
    pub kind_boost_mode: KindBoostMode,
    /// Вес фрагментов документов-вложений (M6 §11, решение 14): множитель их балла рядом с
    /// бустами — слитого и реранкера (0..=10, по умолчанию 1,0 — выдача побайтово как в v0.0.9).
    /// Выбор 1,0 — сетка eval документов задачи 11 M6 (журнал).
    pub w_attach: f64,
    /// Бюджет выдачи `POST /search` и `memory_search` без явного `budget_tokens`, токенов
    /// счётчика сервера (1..=200000, 800; M6 §4.2): Markdown `brief`/`notes` целиком, вместе
    /// со строкой `~N tok` MCP, укладывается в него; первая карточка отдаётся всегда.
    pub budget_tokens: usize,
    /// Отсечка слабых карточек `brief`/`notes` (0..=1, 0 — выключена, поведение v0.0.9; M6
    /// §4.2): карточка с баллом ниже `brief_min_rel × лучший` не выдаётся, явный `top_k` её
    /// не снимает. По умолчанию 0 — сетка ТС-1 dev задачи 5 M6: 0,4 ничего не отсекает, 0,5
    /// экономит 0,16 %, но выбрасывает находки только канала `alias` (его вес 0,5 — балл вдвое
    /// ниже: приёмка M4.5 `glossary_ru_en`), поэтому остаётся значение v0.0.9.
    pub brief_min_rel: f64,
}

impl SearchSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("search.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(self.rrf_k.is_finite() && self.rrf_k > 0.0) {
            return Err(bad("rrf_k", "должно быть больше 0"));
        }
        for (k, v) in [
            ("w_vector", self.w_vector),
            ("w_fts", self.w_fts),
            ("w_fts_relaxed", self.w_fts_relaxed),
            ("w_trigram", self.w_trigram),
            ("w_folder", self.w_folder),
            ("recency_half_life_days", self.recency_half_life_days),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                return Err(bad(k, "должно быть конечным и не меньше 0"));
            }
        }
        if !(self.w_fts_alias.is_finite() && (0.0..=1.0).contains(&self.w_fts_alias)) {
            return Err(bad("w_fts_alias", "должно быть в 0..=1"));
        }
        if self.max_per_node == 0 {
            return Err(bad("max_per_node", "должно быть больше 0"));
        }
        if self.max_per_attachment == 0 {
            return Err(bad("max_per_attachment", "должно быть больше 0"));
        }
        if !(1..=500).contains(&self.attach_prefetch) {
            return Err(bad("attach_prefetch", "должно быть в 1..=500"));
        }
        for (k, v) in [
            ("boost_gotcha", self.boost_gotcha),
            ("boost_failure", self.boost_failure),
            ("boost_area", self.boost_area),
            ("boost_axiom", self.boost_axiom),
            ("boost_theorem", self.boost_theorem),
            ("boost_hypothesis", self.boost_hypothesis),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(bad(k, "должно быть конечным и больше 0 (1 — без буста)"));
            }
        }
        if !(self.graph_weight.is_finite() && (0.0..=1.0).contains(&self.graph_weight)) {
            return Err(bad("graph_weight", "должно быть в 0..=1"));
        }
        if !(self.w_attach.is_finite() && (0.0..=10.0).contains(&self.w_attach)) {
            return Err(bad("w_attach", "должно быть в 0..=10"));
        }
        if !(1..=200_000).contains(&self.budget_tokens) {
            return Err(bad("budget_tokens", "от 1 до 200000"));
        }
        if !(self.brief_min_rel.is_finite() && (0.0..=1.0).contains(&self.brief_min_rel)) {
            return Err(bad("brief_min_rel", "должно быть в 0..=1"));
        }
        Ok(())
    }
}

/// Секция `[insights]` (§7.5): дедупликация при записи insight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightsSection {
    /// Порог dense-косинуса, с которого действующий insight той же области — кандидат в
    /// дубли (0..=1, по умолчанию 0,90).
    pub dedup_threshold: f32,
    /// Порог dense-косинуса, с которого действующая аксиома-`decision` той же области —
    /// «близкая» к новому решению агента (`warnings: decision_near_axiom`; M4.5, §5.1);
    /// 0..=1, по умолчанию 0,80.
    pub decision_conflict_threshold: f32,
}

impl InsightsSection {
    fn validate(&self) -> Result<(), ConfigError> {
        for (k, v) in [
            ("dedup_threshold", self.dedup_threshold),
            (
                "decision_conflict_threshold",
                self.decision_conflict_threshold,
            ),
        ] {
            if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
                return Err(ConfigError::Invalid {
                    key: format!("insights.{k}"),
                    source_name: "итог".into(),
                    message: "должно быть в 0..=1".into(),
                });
            }
        }
        Ok(())
    }
}

/// Секция `[read]` (§9.3 шаг 2): чтение пачкой `POST /read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSection {
    /// Не больше элементов в одном запросе (1..=100, по умолчанию 20); больше — 422.
    pub max_items: usize,
    /// Бюджет токенов на весь ответ по умолчанию (1..=200000, по умолчанию 1500).
    pub budget_tokens: usize,
    /// Явный фокус (M6 §6): не больше листов в выдаче (1..=10, по умолчанию 3).
    pub focus_leaves: usize,
    /// Цель не длиннее (полный текст, токенов) — целиком без фокуса (0..=20000, по умолчанию 600).
    pub focus_min_tokens: usize,
    /// Срок эмбеддинга запроса и чтения векторов листов, мс; вышел — фокус по лексике
    /// (1..=10000, по умолчанию 80). Срок вызова — это × число различных `focus`, не больше
    /// `focus_timeout_max_ms`.
    pub focus_timeout_ms: u64,
    /// Потолок срока вызова фокуса, мс (1..=10000, не меньше `focus_timeout_ms`, по умолчанию 300).
    pub focus_timeout_max_ms: u64,
}

impl ReadSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("read.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=100).contains(&self.max_items) {
            return Err(bad("max_items", "от 1 до 100"));
        }
        if !(1..=200_000).contains(&self.budget_tokens) {
            return Err(bad("budget_tokens", "от 1 до 200000"));
        }
        if !(1..=10).contains(&self.focus_leaves) {
            return Err(bad("focus_leaves", "от 1 до 10"));
        }
        if self.focus_min_tokens > 20_000 {
            return Err(bad("focus_min_tokens", "от 0 до 20000"));
        }
        if !(1..=10_000).contains(&self.focus_timeout_ms) {
            return Err(bad("focus_timeout_ms", "от 1 до 10000"));
        }
        if !(1..=10_000).contains(&self.focus_timeout_max_ms) {
            return Err(bad("focus_timeout_max_ms", "от 1 до 10000"));
        }
        if self.focus_timeout_max_ms < self.focus_timeout_ms {
            return Err(bad(
                "focus_timeout_max_ms",
                "не меньше read.focus_timeout_ms",
            ));
        }
        Ok(())
    }
}

/// Секция `[cache]` (§9.4): кэш выжимок агентов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheSection {
    /// Предел выжимки в токенах (1..=10000, по умолчанию 600); больше — 413.
    pub summary_tokens: usize,
    /// Порог косинуса семантического попадания (0..=1, по умолчанию 0,92).
    pub semantic_threshold: f32,
    /// Порог косинуса нового релевантного источника (0..=1, по умолчанию 0,80).
    pub new_sources_threshold: f32,
    /// Срок жизни выжимки без обращений, дней (1..=3650, по умолчанию 30).
    pub ttl_days: u32,
    /// Предел выжимок на базу, LRU по последнему обращению (1..=1000000, по умолчанию 2000).
    pub max_entries: usize,
    /// Поиск выдаёт выжимки других сессий той же области (по умолчанию да).
    pub cross_session: bool,
}

impl CacheSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("cache.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=10_000).contains(&self.summary_tokens) {
            return Err(bad("summary_tokens", "от 1 до 10000"));
        }
        for (k, v) in [
            ("semantic_threshold", self.semantic_threshold),
            ("new_sources_threshold", self.new_sources_threshold),
        ] {
            if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
                return Err(bad(k, "должно быть в 0..=1"));
            }
        }
        if !(1..=3650).contains(&self.ttl_days) {
            return Err(bad("ttl_days", "от 1 до 3650"));
        }
        if !(1..=1_000_000).contains(&self.max_entries) {
            return Err(bad("max_entries", "от 1 до 1000000"));
        }
        Ok(())
    }
}

/// Секция `[recall]` (§10.3): проактивное вспоминание `POST /recall`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallSection {
    /// Бюджет дайджеста старта сессии и подсказок по умолчанию, токенов (1..=20000, 1500).
    /// Без Rules (резерв сверх бюджета, M6 §8). 1500 — по сетке ТС-2 (M6 §8, задача 8c): 800 не
    /// проходит (выпадают идеи тяжёлых сессий); 1000 проходит, но дешевле не больше чем на 2 %
    /// (Claude R 0,996, Codex 1,003) — по §3.5 берётся больший бюджет; запаса у 1000 нет.
    pub digest_tokens: usize,
    /// Бюджет пакета восстановления после сжатия контекста и `resume`, токенов (1..=20000,
    /// 2000; M4.5 §9.2): `/recall` `event: recovery` или `session_start` после `compact|resume`.
    /// Сетка ТС-2 {1200, 1500, 2000} вырождена (пакеты без Rules меньше 1200) — 2000 (задача 8c).
    pub recovery_tokens: usize,
    /// Бюджет пакета под задачу, токенов (1..=20000, 800; M4.5 §9.4): `/recall` `event: brief`
    /// (хук `subagent-start`, MCP `memory_recall` при составлении задания субагенту).
    /// 800 — по сетке ТС-2 (задача 8c): 500 экономит 1,43 % токенов сессии (Claude, R 0,986;
    /// ≤ 2 % — больший бюджет, §3.5), а субагент теряет треть правил и решения.
    pub brief_tokens: usize,
    /// Наименьший балл поиска подсказки к запросу пользователя (0..=1, 0,6).
    pub prompt_min_score: f64,
    /// Тот же порог, когда поиск `prompt` деградировал до FTS (`degraded`, нет модели или Qdrant;
    /// M6 §9, M4.5 M-9), 0..=1 — подсказки `prompt` и подсказки-идеи (пакет субагенту — всегда
    /// `prompt_min_score`). Не задан — равен `prompt_min_score`, как до M6 (ревью задачи 9, M-1):
    /// сетка {0,2…0,6} условий §9 не выполнила ни при одном значении (тишина 0,27 < 0,7).
    #[serde(default)]
    pub prompt_min_score_fts: Option<f64>,
    /// Порог покрытия слов ошибки у ослабленного FTS `post_tool` (M6 §9):
    /// `{divisor, cap, word_match}` — `clamp(⌈n / divisor⌉, 2, cap)`, слова `plain` | `stems`;
    /// `{2, 6, plain}` — по сетке задачи 9 (до M6 — `{3, 4, plain}`; [`CoverageRule::M6`]).
    pub post_tool_coverage: CoverageRule,
    /// Бюджет сервера на сбор подсказок, мс (1..=60000, 200): по превышении отдаётся собранное
    /// и `warnings: ["recall timeout"]`.
    pub timeout_ms: u64,
    /// Фрагмент под подсказкой поиска (правило, insight), токенов (0..=200, 60; M6 §4.3);
    /// 0 — без фрагмента.
    pub snippet_tokens: usize,
    /// Записей в разделе дайджеста и пакета восстановления (задачи, ответы, ловушки, revisit,
    /// changelog, решения), 1..=20, 5 (M6 §8; прежде константа `DIGEST_ENTRIES_MAX`). Сетка ТС-2
    /// (задача 8c): при 3 выпадают задачи и ответы, 8 не дешевле 5.
    pub digest_entries: usize,
    /// Мини-карта кода дайджеста `### Карта кода` (центральные символы), токенов, 0..=1000,
    /// 0 — только строка-указатель карты (M6 §8, решение 16; включение — по A/B).
    pub digest_code_map_tokens: usize,
    /// Раздел «Последнее известное состояние» (снимок сессии, M6 §8а) в дайджесте (только
    /// оборванная сессия) и пакете восстановления, токенов (0..=2000, 400), внутри `digest_tokens`/`recovery_tokens`;
    /// 0 — не показывать (снимок хранится).
    pub snapshot_tokens: usize,
    /// Задача снимка «давно не обновлялась», если её последняя ревизия старше снимка на столько
    /// минут (0..=1440, 30; M6 §8а).
    pub snapshot_stale_min: u32,
    /// Окно дайджеста для снимков других сессий, суток UTC от начала текущих (1..=30, 2).
    pub snapshot_digest_days: u32,
}

impl RecallSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("recall.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=20_000).contains(&self.digest_tokens) {
            return Err(bad("digest_tokens", "от 1 до 20000"));
        }
        if !(1..=20_000).contains(&self.recovery_tokens) {
            return Err(bad("recovery_tokens", "от 1 до 20000"));
        }
        if !(1..=20_000).contains(&self.brief_tokens) {
            return Err(bad("brief_tokens", "от 1 до 20000"));
        }
        if !(self.prompt_min_score.is_finite() && (0.0..=1.0).contains(&self.prompt_min_score)) {
            return Err(bad("prompt_min_score", "должно быть в 0..=1"));
        }
        if let Some(fts) = self.prompt_min_score_fts
            && !(fts.is_finite() && (0.0..=1.0).contains(&fts))
        {
            return Err(bad("prompt_min_score_fts", "должно быть в 0..=1"));
        }
        let cov = &self.post_tool_coverage;
        if !(1..=10).contains(&cov.divisor) {
            return Err(bad("post_tool_coverage.divisor", "от 1 до 10"));
        }
        if !(2..=32).contains(&cov.cap) {
            return Err(bad("post_tool_coverage.cap", "от 2 до 32"));
        }
        if !(1..=60_000).contains(&self.timeout_ms) {
            return Err(bad("timeout_ms", "от 1 до 60000"));
        }
        if self.snippet_tokens > 200 {
            return Err(bad("snippet_tokens", "от 0 до 200"));
        }
        if !(1..=20).contains(&self.digest_entries) {
            return Err(bad("digest_entries", "от 1 до 20"));
        }
        if self.digest_code_map_tokens > 1000 {
            return Err(bad("digest_code_map_tokens", "от 0 до 1000"));
        }
        if self.snapshot_tokens > 2000 {
            return Err(bad("snapshot_tokens", "от 0 до 2000"));
        }
        if self.snapshot_stale_min > 1440 {
            return Err(bad("snapshot_stale_min", "от 0 до 1440"));
        }
        if !(1..=30).contains(&self.snapshot_digest_days) {
            return Err(bad("snapshot_digest_days", "от 1 до 30"));
        }
        Ok(())
    }
}

/// Секция `[docs]` (M5 §1–2): лимиты разбора документов и рабочие процессы разбора
/// (`zam-server doc-worker`: все виды документов, задача 5 M5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsSection {
    /// Библиотека pdfium; по умолчанию рядом с исполняемым файлом (`pdfium.dll` /
    /// `libpdfium.so`), относительный путь — от каталога исполняемого файла.
    pub pdfium_dylib: Option<PathBuf>,
    /// Рабочих процессов разбора (1..=16, 2): столько же документов разбирается одновременно.
    pub workers: usize,
    /// Заданий на процесс до перезапуска (1..=100000, 200).
    pub worker_jobs: u32,
    /// Лимит памяти процесса, МиБ (128..=65536, 2048; не меньше 3 × `max_bytes` + 64 МиБ):
    /// Job Object / `RLIMIT_AS`.
    pub worker_memory_mb: u64,
    /// Срок разбора файла, мс (100..=3600000, 60000); по истечении процесс убивается.
    pub timeout_ms: u64,
    /// Рабочих процессов превью (1..=16, 1): свой пул, чтобы долгий разбор индексации не
    /// задерживал первый экран листа (ТЗ §17); лимиты памяти и срок — те же.
    pub preview_workers: usize,
    /// Предел кэша превью `previews/`, МиБ (1..=1048576, 1024): сверх него старые файлы
    /// вытесняются.
    pub preview_cache_mb: u64,
    /// Размер файла, байт (100 МиБ); больше — отказ.
    pub max_bytes: u64,
    /// Распакованный объём zip, байт (500 МиБ).
    pub max_unpacked: u64,
    /// Степень сжатия записи zip (100).
    pub max_ratio: u32,
    /// Записей zip (10000).
    pub max_entries: u32,
    /// Глубина XML (256).
    pub max_xml_depth: u32,
    /// Непустых ячеек листа (2 млн).
    pub max_cells: u64,
    /// Объём выдачи, символов (20 млн): текст, пути заголовков и служебный вес единиц.
    pub max_chars: u64,
    /// Страниц PDF (5000).
    pub max_pages: u32,
    /// Исполняемый файл рабочего процесса (подкоманда `doc-worker`); `None` — исполняемый файл
    /// сервера. Не ключ конфигурации: его задают интеграционные тесты, где текущий исполняемый
    /// файл — тестовый, а не `zam-server`.
    #[serde(skip)]
    pub worker_program: Option<PathBuf>,
}

impl Default for DocsSection {
    fn default() -> Self {
        let l = zam_docs::Limits::default();
        DocsSection {
            pdfium_dylib: None,
            workers: 2,
            worker_jobs: 200,
            worker_memory_mb: 2048,
            timeout_ms: 60_000,
            preview_workers: 1,
            preview_cache_mb: 1024,
            max_bytes: l.max_bytes,
            max_unpacked: l.max_unpacked,
            max_ratio: l.max_ratio,
            max_entries: l.max_entries,
            max_xml_depth: l.max_xml_depth,
            max_cells: l.max_cells,
            max_chars: l.max_chars,
            max_pages: l.max_pages,
            worker_program: None,
        }
    }
}

impl DocsSection {
    /// Лимиты разбора `zam-docs`.
    pub fn limits(&self) -> zam_docs::Limits {
        zam_docs::Limits {
            max_bytes: self.max_bytes,
            max_unpacked: self.max_unpacked,
            max_ratio: self.max_ratio,
            max_entries: self.max_entries,
            // внутренний предел разбора (не ключ `[docs]`): защита каталога zip, не настройка
            max_central_dir: zam_docs::Limits::default().max_central_dir,
            max_xml_depth: self.max_xml_depth,
            max_cells: self.max_cells,
            max_chars: self.max_chars,
            max_pages: self.max_pages,
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("docs.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=16).contains(&self.workers) {
            return Err(bad("workers", "от 1 до 16"));
        }
        if !(1..=100_000).contains(&self.worker_jobs) {
            return Err(bad("worker_jobs", "от 1 до 100000"));
        }
        if !(128..=65_536).contains(&self.worker_memory_mb) {
            return Err(bad("worker_memory_mb", "от 128 до 65536"));
        }
        if !(100..=3_600_000).contains(&self.timeout_ms) {
            return Err(bad("timeout_ms", "от 100 до 3600000"));
        }
        if !(1..=16).contains(&self.preview_workers) {
            return Err(bad("preview_workers", "от 1 до 16"));
        }
        if !(1..=1_048_576).contains(&self.preview_cache_mb) {
            return Err(bad("preview_cache_mb", "от 1 до 1048576"));
        }
        for (k, v) in [
            ("max_bytes", self.max_bytes),
            ("max_unpacked", self.max_unpacked),
            ("max_ratio", u64::from(self.max_ratio)),
            ("max_entries", u64::from(self.max_entries)),
            ("max_xml_depth", u64::from(self.max_xml_depth)),
            ("max_cells", self.max_cells),
            ("max_chars", self.max_chars),
            ("max_pages", u64::from(self.max_pages)),
        ] {
            if v == 0 {
                return Err(bad(k, "должно быть больше 0"));
            }
        }
        // файл целиком в кадре base64 рабочего процесса: кадр — u32
        if self.max_bytes > 1 << 30 {
            return Err(bad("max_bytes", "не больше 1 ГиБ"));
        }
        // в рабочем процессе одновременно: кадр base64 (4/3), декодированный файл и pdfium
        let need = 3 * self.max_bytes + 64 * 1024 * 1024;
        if self.worker_memory_mb * 1024 * 1024 < need {
            return Err(bad(
                "worker_memory_mb",
                &format!(
                    "меньше 3 × max_bytes + 64 МиБ = {} МиБ: файл предельного размера \
                     не разобрать; увеличьте worker_memory_mb или уменьшите max_bytes",
                    need.div_ceil(1024 * 1024)
                ),
            ));
        }
        Ok(())
    }
}

/// Секция `[vault]` (§7.6): файл мастер-ключа. Сам ключ — только в файле или в
/// `ZAM_MASTER_KEY` (переменная важнее файла), в конфигурацию он не попадает.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSection {
    /// Файл мастер-ключа (base64 32 байт); по умолчанию `<data_dir>/master.key`.
    pub master_key_file: Option<PathBuf>,
}

/// Пределы размеров (§16.2): заметка и вложение, байт.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    pub note_max_bytes: usize,
    pub blob_max_bytes: usize,
}

/// Секция `[reports]` (M5.5 §5–6): пределы файлов отчёта. Размер одного файла — общий предел
/// блоба `limits.blob_max_bytes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportsSection {
    /// Не больше файлов в одном отчёте (1..=1024, по умолчанию 64); больше — 422.
    pub max_files: u32,
    /// Сумма размеров файлов отчёта, байт (по умолчанию 200 МиБ); больше — 413.
    pub max_total_bytes: u64,
    /// Предел отчёта через MCP (JSON, файлы в base64): тело плюс декодированные файлы, байт
    /// (по умолчанию 16 МиБ, не больше `max_total_bytes`); больше — 413 с подсказкой `zam report`.
    pub mcp_max_bytes: u64,
}

impl Default for ReportsSection {
    fn default() -> Self {
        Self {
            max_files: 64,
            max_total_bytes: 209_715_200,
            mcp_max_bytes: 16_777_216,
        }
    }
}

impl ReportsSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("reports.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=1024).contains(&self.max_files) {
            return Err(bad("max_files", "от 1 до 1024"));
        }
        if self.max_total_bytes == 0 {
            return Err(bad("max_total_bytes", "больше 0"));
        }
        if self.mcp_max_bytes == 0 || self.mcp_max_bytes > self.max_total_bytes {
            return Err(bad(
                "mcp_max_bytes",
                "больше 0 и не больше reports.max_total_bytes",
            ));
        }
        Ok(())
    }
}

/// Секция `[code]` (M5.6 §12): карта кода решения — выключатель маршрутов `/code/*`, пределы
/// недоверенного входа клиента `zam`, бюджеты карты и срок пересчёта рангов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeSection {
    /// Маршруты карты кода (`false` — 404 у всех `/code/*`; строка дайджеста и `### Код` гаснут).
    pub enabled: bool,
    /// Не больше файлов в манифесте `POST /code/sync` (1..=500000, 50 000); больше — 422.
    pub max_files: u32,
    /// Не больше символов в файле загрузки (1..=100000, 5000); больше — 422.
    pub max_symbols_per_file: u32,
    /// Не больше агрегированных ссылок в файле загрузки (1..=1000000, 20 000); больше — 422.
    pub max_refs_per_file: u32,
    /// Предел тела `POST /code/sync`, `PUT /code/files`, `POST /code/impact`, байт
    /// (1 КиБ..=256 МиБ, 8 МиБ); больше — 413.
    pub max_request_bytes: u64,
    /// Бюджет карты по умолчанию, токенов (1..=20000, 1000).
    pub map_tokens: usize,
    /// Бюджет раздела `### Код` пакета субагенту, токенов (0..=1000, 150; сетка ТС-2 {100, 150}
    /// вырождена — раздел фикстуры меньше 100, задача 8c).
    pub brief_tokens: usize,
    /// Сколько запрос карты ждёт пересчёта рангов, мс (0..=60000, 1000; 0 — не ждать); дольше —
    /// старые ранги и `warnings: ["rank pending"]`, пересчёт продолжается в фоне.
    pub rank_timeout_ms: u64,
}

impl Default for CodeSection {
    fn default() -> Self {
        Self {
            enabled: true,
            max_files: 50_000,
            max_symbols_per_file: 5000,
            max_refs_per_file: 20_000,
            max_request_bytes: 8_388_608,
            map_tokens: 1000,
            brief_tokens: 150,
            rank_timeout_ms: 1000,
        }
    }
}

impl CodeSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("code.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1..=500_000).contains(&self.max_files) {
            return Err(bad("max_files", "от 1 до 500000"));
        }
        if !(1..=100_000).contains(&self.max_symbols_per_file) {
            return Err(bad("max_symbols_per_file", "от 1 до 100000"));
        }
        if !(1..=1_000_000).contains(&self.max_refs_per_file) {
            return Err(bad("max_refs_per_file", "от 1 до 1000000"));
        }
        if !(1024..=268_435_456).contains(&self.max_request_bytes) {
            return Err(bad("max_request_bytes", "от 1024 до 268435456 (256 МиБ)"));
        }
        if !(1..=20_000).contains(&self.map_tokens) {
            return Err(bad("map_tokens", "от 1 до 20000"));
        }
        if self.brief_tokens > 1000 {
            return Err(bad("brief_tokens", "от 0 до 1000"));
        }
        if self.rank_timeout_ms > 60_000 {
            return Err(bad("rank_timeout_ms", "от 0 до 60000"));
        }
        Ok(())
    }
}

/// Секция `[ideas]` (M5.7 §8): журнал «Идеи» — проактивность (раздел дайджеста `### Идеи`,
/// подсказки `prompt`) и срок отсрочки по умолчанию.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeasSection {
    /// Глобальный выключатель проактивности (`false` — ни раздела, ни подсказок у всех);
    /// действует вместе с пользовательским `ideas.proactive` (`server && user`).
    pub proactive: bool,
    /// Идей в разделе `### Идеи` дайджеста (0..=10, 3; 0 — раздела нет).
    pub digest_max: u32,
    /// Идей в подсказке `prompt` (0..=3, 1).
    pub prompt_max: u32,
    /// `top_k` отдельного поиска идей к `prompt` (1..=20, 5).
    pub prompt_top_k: u32,
    /// `horizon` по умолчанию у `defer`, дней (1..=3650, 30).
    pub defer_days: u32,
}

impl Default for IdeasSection {
    fn default() -> Self {
        Self {
            proactive: true,
            digest_max: 3,
            prompt_max: 1,
            prompt_top_k: 5,
            defer_days: zam_store::IDEA_DEFER_DAYS_DEFAULT,
        }
    }
}

impl IdeasSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("ideas.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if self.digest_max > 10 {
            return Err(bad("digest_max", "от 0 до 10"));
        }
        if self.prompt_max > 3 {
            return Err(bad("prompt_max", "от 0 до 3"));
        }
        if !(1..=20).contains(&self.prompt_top_k) {
            return Err(bad("prompt_top_k", "от 1 до 20"));
        }
        if !(1..=3650).contains(&self.defer_days) {
            return Err(bad("defer_days", "от 1 до 3650"));
        }
        Ok(())
    }
}

/// Расписание автокопий (M6 §12б.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupSchedule {
    Off,
    Daily,
}

/// Секция `[backup]` (M6 §12б.2–§12б.4): копии `zam-server backup` и автокопии.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSection {
    /// Каталог копий; по умолчанию `<data_dir>/backups`.
    pub dir: Option<PathBuf>,
    /// `off` (по умолчанию: копия на том же диске может его заполнить) или `daily`.
    pub schedule: BackupSchedule,
    /// Время автокопии, `HH:MM` UTC (по умолчанию `03:30`).
    pub at: String,
    /// Сколько автокопий хранить (1..=365, 7); сверх — удаляются самые старые.
    pub keep: u32,
    /// `GET /index/status` предупреждает `no_recent_backup`, если последней копии больше
    /// стольких суток или её нет (1..=365, 7).
    pub warn_days: u32,
    /// Признак копии `.zam-backup` старше стольких часов — брошенный (1..=168, 6).
    pub flag_max_hours: u32,
}

impl Default for BackupSection {
    fn default() -> Self {
        Self {
            dir: None,
            schedule: BackupSchedule::Off,
            at: "03:30".into(),
            keep: 7,
            warn_days: 7,
            flag_max_hours: 6,
        }
    }
}

impl BackupSection {
    /// Каталог копий: `backup.dir` или `<data_dir>/backups`.
    pub fn dir(&self, data_dir: &Path) -> PathBuf {
        self.dir.clone().unwrap_or_else(|| data_dir.join("backups"))
    }

    /// `at` как (часы, минуты); проверено [`BackupSection::validate`].
    pub fn at_hm(&self) -> (i8, i8) {
        parse_hm(&self.at).unwrap_or((3, 30))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("backup.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if parse_hm(&self.at).is_none() {
            return Err(bad("at", "время HH:MM (UTC), например 03:30"));
        }
        if !(1..=365).contains(&self.keep) {
            return Err(bad("keep", "от 1 до 365"));
        }
        if !(1..=365).contains(&self.warn_days) {
            return Err(bad("warn_days", "от 1 до 365"));
        }
        if !(1..=168).contains(&self.flag_max_hours) {
            return Err(bad("flag_max_hours", "от 1 до 168"));
        }
        Ok(())
    }
}

/// Секция `[snapshot]` (M6 §8а): снимок сессии `PUT /sessions/{id}/snapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSection {
    /// Предел записи снимка — всего тела JSON после маскировки, байт (1024..=65536, 16384):
    /// больше — 413.
    pub max_bytes: usize,
    /// Срок хранения снимка от `taken_at`, суток (1..=90, 14): старше удаляет воркер
    /// обслуживания раз в сутки; вместе с сессией снимок уходит и раньше (`gc_sessions`).
    pub keep_days: u32,
}

impl Default for SnapshotSection {
    fn default() -> Self {
        Self {
            max_bytes: 16_384,
            keep_days: 14,
        }
    }
}

impl SnapshotSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("snapshot.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        if !(1024..=65_536).contains(&self.max_bytes) {
            return Err(bad("max_bytes", "от 1024 до 65536"));
        }
        if !(1..=90).contains(&self.keep_days) {
            return Err(bad("keep_days", "от 1 до 90"));
        }
        Ok(())
    }
}

/// Секция `[export]` (M6 §12б.5): экспорт пространства `GET /spaces/{space}/export`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSection {
    /// Предел длительности одного экспорта, с (1..=604800, 14400 — 4 ч): дольше — поток
    /// обрывается (`complete: false` в аудите), транзакция чтения закрывается.
    pub max_secs: u64,
}

impl Default for ExportSection {
    fn default() -> Self {
        Self { max_secs: 4 * 3600 }
    }
}

impl ExportSection {
    fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=604_800).contains(&self.max_secs) {
            return Err(ConfigError::Invalid {
                key: "export.max_secs".into(),
                source_name: "итог".into(),
                message: "от 1 до 604800 (7 суток)".into(),
            });
        }
        Ok(())
    }
}

/// Секция `[import]` (M6 §12б.5): импорт пространства `POST /spaces/{space}/import`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSection {
    /// Предел тела запроса (zip) и распакованного объёма архива, байт (1 МиБ..=64 ГиБ, 2 ГиБ):
    /// больше — 413 (тело) или 422 (распакованный объём).
    pub max_bytes: u64,
    /// Файлов в архиве не больше (1..=1 000 000, 100 000): больше — 422.
    pub max_files: usize,
    /// Тексты архива (всё, кроме вложений `_attachments/`: заметки, записи, история, связи)
    /// держит в памяти план импорта — их объём не больше, байт (1 МиБ..=64 ГиБ, 256 МиБ): больше —
    /// 422.
    pub max_text_bytes: u64,
    /// Запас свободного места, байт (0..=1 ТиБ, 2 ГиБ): перед приёмом тела на диске каталога
    /// данных должно быть свободно `min_free_bytes + 2 × ожидаемый объём` сверх места,
    /// зарезервированного идущими приёмами; иначе 507.
    pub min_free_bytes: u64,
}

impl Default for ImportSection {
    fn default() -> Self {
        Self {
            max_bytes: 2 << 30,
            max_files: 100_000,
            max_text_bytes: zam_store::import::MAX_TEXT_BYTES_DEFAULT,
            min_free_bytes: 2 << 30,
        }
    }
}

impl ImportSection {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |key: &str, message: &str| ConfigError::Invalid {
            key: format!("import.{key}"),
            source_name: "итог".into(),
            message: message.into(),
        };
        const MIB: u64 = 1 << 20;
        const MAX: u64 = 64 << 30;
        if !(MIB..=MAX).contains(&self.max_bytes) {
            return Err(bad(
                "max_bytes",
                "от 1048576 (1 МиБ) до 68719476736 (64 ГиБ)",
            ));
        }
        if !(1..=1_000_000).contains(&self.max_files) {
            return Err(bad("max_files", "от 1 до 1000000"));
        }
        if self.min_free_bytes > 1 << 40 {
            return Err(bad("min_free_bytes", "от 0 до 1099511627776 (1 ТиБ)"));
        }
        if !(MIB..=MAX).contains(&self.max_text_bytes) {
            return Err(bad(
                "max_text_bytes",
                "от 1048576 (1 МиБ) до 68719476736 (64 ГиБ)",
            ));
        }
        Ok(())
    }
}

/// `HH:MM` → (часы 0..=23, минуты 0..=59).
fn parse_hm(s: &str) -> Option<(i8, i8)> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..=23).contains(&h) && (0..=59).contains(&m)).then_some((h, m))
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server: ServerSection {
                bind: SocketAddr::from(([0, 0, 0, 0], 7700)),
                public_url: None,
                base_path: String::new(),
                trusted_proxies: Vec::new(),
                downloads_dir: None,
            },
            admin: AdminSection {
                login: "admin".into(),
                password_hash: None,
            },
            data_dir: PathBuf::from("./data"),
            log: LogSection {
                level: "info".into(),
                format: LogFormat::Json,
                max_files: 14,
            },
            qdrant: QdrantSection {
                mode: QdrantMode::Managed,
                url: None,
                http_port: 6333,
                grpc_port: 6334,
                binary: None,
                max_search_threads: 2,
                optimizer_cpu_budget: 2,
                api_key: None,
                flush_interval_sec: zam_index::supervisor::DEFAULT_FLUSH_INTERVAL_SEC,
                wal_capacity_mb: zam_index::supervisor::DEFAULT_WAL_CAPACITY_MB,
                shutdown_flush_wait_ms: zam_index::supervisor::DEFAULT_SHUTDOWN_FLUSH_WAIT
                    .as_millis() as u64,
                exact_search: false,
            },
            ml: MlSection {
                device: MlDevice::Auto,
                models_dir: None,
                embed_model: None,
                ort_dylib: None,
                index_threads: 0,
                query_threads: 2,
                unload_after: String::new(),
                rerank_model_cpu: "jina-reranker-v2".into(),
                rerank_model_gpu: "bge-reranker-v2-m3".into(),
                rerank_top: 20,
                rerank_tokens: 256,
                rerank_sessions: 0,
            },
            index: IndexSection {
                batch_tokens: 8192,
                yield_ms: 5,
                rollup_debounce_ms: 2000,
                chunk_target_min: 300,
                chunk_target_max: 500,
                chunk_hard_max: 800,
                chunk_merge_below: 100,
                chunk_overlap_sentences: 1,
                chunk_boundary_lambda: 0.0,
                chunk_cohesion: false,
                chunk_prefix_l0: false,
                chunk_heading_path_h4: true,
            },
            search: SearchSection {
                rrf_k: 60.0,
                w_vector: 1.0,
                w_fts: 1.0,
                w_fts_relaxed: 0.25,
                w_fts_alias: 0.5,
                w_trigram: 0.7,
                recency_half_life_days: 180.0,
                max_per_node: 3,
                max_per_attachment: 2,
                attach_prefetch: zam_index::ATTACH_PREFETCH,
                boost_gotcha: 1.05,
                boost_failure: 1.05,
                boost_area: 1.0,
                boost_axiom: 1.20,
                boost_theorem: 1.10,
                boost_hypothesis: 1.00,
                graph_weight: 0.5,
                w_folder: 0.0,
                max_folders: 2,
                contradicts_order: true,
                kind_boost_mode: KindBoostMode::Always,
                w_attach: 1.0,
                budget_tokens: 800,
                brief_min_rel: 0.0,
            },
            insights: InsightsSection {
                dedup_threshold: 0.90,
                decision_conflict_threshold: 0.80,
            },
            read: ReadSection {
                max_items: 20,
                budget_tokens: 1500,
                focus_leaves: 3,
                focus_min_tokens: 600,
                focus_timeout_ms: 80,
                focus_timeout_max_ms: 300,
            },
            cache: CacheSection {
                summary_tokens: 600,
                semantic_threshold: 0.92,
                new_sources_threshold: 0.80,
                ttl_days: 30,
                max_entries: 2000,
                cross_session: true,
            },
            recall: RecallSection {
                digest_tokens: 1500,
                recovery_tokens: 2000,
                brief_tokens: 800,
                prompt_min_score: 0.6,
                prompt_min_score_fts: None,
                post_tool_coverage: CoverageRule::M6,
                timeout_ms: 200,
                snippet_tokens: 60,
                digest_entries: 5,
                digest_code_map_tokens: 0,
                snapshot_tokens: 400,
                snapshot_stale_min: 30,
                snapshot_digest_days: 2,
            },
            limits: LimitsSection {
                note_max_bytes: 5_242_880,
                blob_max_bytes: 209_715_200,
            },
            vault: VaultSection::default(),
            docs: DocsSection::default(),
            reports: ReportsSection::default(),
            code: CodeSection::default(),
            ideas: IdeasSection::default(),
            backup: BackupSection::default(),
            export: ExportSection::default(),
            import: ImportSection::default(),
            snapshot: SnapshotSection::default(),
        }
    }
}

fn ser_nets<S: Serializer>(nets: &[IpNet], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(nets.iter().map(|n| n.to_string()))
}

/// CIDR или одиночный IP (как /32 или /128).
fn de_nets<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<IpNet>, D::Error> {
    let raw = Vec::<String>::deserialize(d)?;
    raw.iter()
        .map(|s| {
            s.parse::<IpNet>()
                .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| serde::de::Error::custom(format!("неверный CIDR или IP: `{s}`")))
        })
        .collect()
}

/// Переопределения из командной строки (верхний слой).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub bind: Option<SocketAddr>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminSecret {
    Hash(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать файл конфигурации {path}: {message}")]
    Read { path: String, message: String },
    #[error("ошибка синтаксиса в файле конфигурации {path}: {message}")]
    Syntax { path: String, message: String },
    #[error("неверное значение ключа `{key}` ({source_name}): {message}")]
    Invalid {
        key: String,
        source_name: String,
        message: String,
    },
    #[error("задайте [admin].password_hash (zam-server hash-password) или ZAM_ADMIN_PASSWORD")]
    AdminPasswordMissing,
    #[error("[admin].password_hash не является корректным PHC-хэшем: {0}")]
    AdminHashInvalid(String),
    #[error("не удалось захэшировать пароль администратора: {0}")]
    Hash(String),
}

/// Собирает конфигурацию: умолчания, файл, окружение, CLI.
pub fn load(
    file: Option<&Path>,
    env: &[(String, String)],
    cli: &CliOverrides,
) -> Result<ServerConfig, ConfigError> {
    let mut merged = Value::try_from(ServerConfig::default()).expect("умолчания сериализуются");
    // (путь ключа, источник, значение) в порядке применения.
    let mut leaves: Vec<(Vec<String>, String, Value, Option<String>)> = Vec::new();

    if let Some(path) = file {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: shown.clone(),
            message: e.to_string(),
        })?;
        let table: Table = toml::from_str(&text).map_err(|e| {
            // Без цитаты исходника: в строке может быть секрет.
            let place = e.span().map_or(String::new(), |sp| {
                let before = &text.as_bytes()[..sp.start.min(text.len())];
                let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
                let col = before.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
                format!(" (строка {line}, столбец {col})")
            });
            ConfigError::Syntax {
                path: shown.clone(),
                message: format!("{}{place}", e.message()),
            }
        })?;
        flatten(&mut Vec::new(), &Value::Table(table), &mut |p, v| {
            leaves.push((p, format!("file {shown}"), v, None));
        });
    }

    for (name, raw) in env {
        let upper = name.to_ascii_uppercase();
        let Some(rest) = upper.strip_prefix("ZAM_") else {
            continue;
        };
        if RESERVED_ENV.contains(&upper.as_str()) || rest.is_empty() {
            continue;
        }
        let path: Vec<String> = rest
            .to_ascii_lowercase()
            .split("__")
            .map(String::from)
            .collect();
        leaves.push((
            path,
            format!("env {name}"),
            parse_scalar(raw),
            Some(raw.clone()),
        ));
    }

    if let Some(bind) = cli.bind {
        leaves.push((
            vec!["server".into(), "bind".into()],
            "cli".into(),
            Value::String(bind.to_string()),
            None,
        ));
    }
    if let Some(dir) = &cli.data_dir {
        leaves.push((
            vec!["data_dir".into()],
            "cli".into(),
            Value::String(dir.to_string_lossy().into_owned()),
            None,
        ));
    }

    let defaults = merged.clone();
    for (path, source, mut value, raw) in leaves {
        // Пустая таблица известной секции ничего не меняет.
        if path.is_empty()
            || (is_empty_table(&value) && get_path(&defaults, &path).is_some_and(Value::is_table))
        {
            continue;
        }
        // Проверка значения в изоляции, чтобы указать ключ и источник.
        let probe_with = |v: &Value| {
            let mut probe = defaults.clone();
            set_path(&mut probe, &path, v.clone());
            probe.try_into::<ServerConfig>().map(|_| ())
        };
        if let Err(e) = probe_with(&value) {
            // Значение env может быть строкой, похожей на число/булево/дату.
            let as_string = raw.map(Value::String).filter(|v| probe_with(v).is_ok());
            match as_string {
                Some(v) => value = v,
                None => {
                    return Err(ConfigError::Invalid {
                        key: path.join("."),
                        source_name: source,
                        message: e.message().to_string(),
                    });
                }
            }
        }
        set_path(&mut merged, &path, value);
    }

    let mut cfg: ServerConfig =
        merged
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Invalid {
                key: "?".into(),
                source_name: "итог".into(),
                message: e.message().to_string(),
            })?;
    cfg.server.base_path = normalize_base_path(&cfg.server.base_path);
    cfg.ml.validate()?;
    cfg.index.validate()?;
    cfg.search.validate()?;
    cfg.insights.validate()?;
    cfg.read.validate()?;
    cfg.cache.validate()?;
    cfg.recall.validate()?;
    cfg.docs.validate()?;
    cfg.reports.validate()?;
    cfg.code.validate()?;
    cfg.ideas.validate()?;
    cfg.backup.validate()?;
    cfg.export.validate()?;
    cfg.import.validate()?;
    cfg.snapshot.validate()?;
    Ok(cfg)
}

/// Значение из env: TOML-скаляр или массив, иначе строка.
fn parse_scalar(raw: &str) -> Value {
    toml::from_str::<Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn normalize_base_path(raw: &str) -> String {
    let t = raw.trim().trim_matches('/');
    if t.is_empty() {
        String::new()
    } else {
        format!("/{t}")
    }
}

/// Обходит листья (всё, кроме таблиц).
fn flatten(prefix: &mut Vec<String>, v: &Value, f: &mut impl FnMut(Vec<String>, Value)) {
    match v {
        Value::Table(t) if !t.is_empty() => {
            for (k, child) in t {
                prefix.push(k.clone());
                flatten(prefix, child, f);
                prefix.pop();
            }
        }
        leaf => f(prefix.clone(), leaf.clone()),
    }
}

fn is_empty_table(v: &Value) -> bool {
    v.as_table().is_some_and(Table::is_empty)
}

fn get_path<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |v, k| v.as_table()?.get(k))
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    if !root.is_table() {
        *root = Value::Table(Table::new());
    }
    let table = root.as_table_mut().expect("таблица");
    if tail.is_empty() {
        table.insert(head.clone(), value);
    } else {
        let child = table
            .entry(head.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        set_path(child, tail, value);
    }
}

impl ServerConfig {
    /// Хэш пароля администратора: из конфига, иначе из `ZAM_ADMIN_PASSWORD` (argon2id в памяти).
    pub fn admin_secret(&self, env_password: Option<&str>) -> Result<AdminSecret, ConfigError> {
        if let Some(h) = self
            .admin
            .password_hash
            .as_deref()
            .filter(|h| !h.is_empty())
        {
            PasswordHash::new(h).map_err(|e| ConfigError::AdminHashInvalid(e.to_string()))?;
            return Ok(AdminSecret::Hash(h.to_string()));
        }
        match env_password.filter(|p| !p.is_empty()) {
            Some(p) => hash_password(p).map(AdminSecret::Hash),
            None => Err(ConfigError::AdminPasswordMissing),
        }
    }
}

/// argon2id с параметрами по умолчанию и случайной солью.
pub fn hash_password(password: &str) -> Result<String, ConfigError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ConfigError::Hash(e.to_string()))
}

/// Хэш построен тем же алгоритмом и с теми же параметрами, что [`hash_password`]: только тогда
/// фиктивный хэш неизвестного логина выравнивает время входа с администратором (§11.4).
pub fn has_default_params(hash: &str) -> bool {
    let Ok(h) = argon2::PasswordHash::new(hash) else {
        return false;
    };
    let Ok(p) = argon2::Params::try_from(&h) else {
        return false;
    };
    let d = argon2::Params::default();
    h.algorithm == argon2::Algorithm::Argon2id.ident()
        && (p.m_cost(), p.t_cost(), p.p_cost()) == (d.m_cost(), d.t_cost(), d.p_cost())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::write_in_target;

    fn env(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn defaults() {
        let c = load(None, &[], &Default::default()).unwrap();
        assert_eq!(c.server.bind.to_string(), "0.0.0.0:7700");
        assert_eq!(c.server.base_path, "");
        assert_eq!(c.admin.login, "admin");
        assert_eq!(c.data_dir, std::path::PathBuf::from("./data"));
        assert_eq!(c.log.level, "info");
        assert_eq!(c.log.format, LogFormat::Json);
        assert_eq!(c.log.max_files, 14);
        assert_eq!(c.qdrant.mode, QdrantMode::Managed);
        assert_eq!((c.qdrant.http_port, c.qdrant.grpc_port), (6333, 6334));
        assert_eq!(
            (c.qdrant.max_search_threads, c.qdrant.optimizer_cpu_budget),
            (2, 2)
        );
        assert_eq!((&c.qdrant.binary, &c.qdrant.api_key), (&None, &None));
        assert_eq!(c.limits.note_max_bytes, 5_242_880);
        assert_eq!(c.limits.blob_max_bytes, 209_715_200);
        assert_eq!(
            (
                c.search.boost_gotcha,
                c.search.boost_failure,
                c.search.boost_area
            ),
            // сетка задачи 18 M3 (fix round 1): буст вида ниже буста theorem — 1,05; буст
            // области — 1,0 (сетка задачи 11 M6)
            (1.05, 1.05, 1.0)
        );
        assert_eq!(
            (
                c.search.boost_axiom,
                c.search.boost_theorem,
                c.search.boost_hypothesis
            ),
            (1.20, 1.10, 1.00)
        );
        assert_eq!(c.search.graph_weight, 0.5);
        // канал папок выключен по сетке задачи 18 M3, карточек папок — не больше двух
        assert_eq!((c.search.w_folder, c.search.max_folders), (0.0, 2));
        assert_eq!(c.insights.dedup_threshold, 0.90);
        // M4.5: близкие аксиомы решений и порядок по `contradicts`
        assert_eq!(c.insights.decision_conflict_threshold, 0.80);
        assert!(c.search.contradicts_order);
        assert_eq!(c.search.kind_boost_mode, KindBoostMode::Always);
    }

    /// M6 §10: `search.kind_boost_mode` из `ZAM_*`, неизвестное значение — ошибка.
    #[test]
    fn kind_boost_mode_key() {
        for (v, want) in [
            ("always", KindBoostMode::Always),
            ("unless_contradicted", KindBoostMode::UnlessContradicted),
            ("reliable_only", KindBoostMode::ReliableOnly),
        ] {
            let c = load(
                None,
                &[env("ZAM_SEARCH__KIND_BOOST_MODE", v)],
                &Default::default(),
            )
            .unwrap();
            assert_eq!(c.search.kind_boost_mode, want, "{v}");
        }
        assert!(
            load(
                None,
                &[env("ZAM_SEARCH__KIND_BOOST_MODE", "sometimes")],
                &Default::default()
            )
            .is_err()
        );
    }

    /// M6 §11: `search.w_attach` — по умолчанию 1,0, из `ZAM_*`, вне 0..=10 — ошибка.
    #[test]
    fn w_attach_key() {
        let c = load(None, &[], &Default::default()).unwrap();
        assert_eq!(c.search.w_attach, 1.0);
        for (v, ok) in [
            ("0", true),
            ("1.3", true),
            ("10", true),
            ("10.5", false),
            ("-0.1", false),
        ] {
            let r = load(None, &[env("ZAM_SEARCH__W_ATTACH", v)], &Default::default());
            assert_eq!(r.is_ok(), ok, "{v}");
            if let Ok(c) = r {
                assert_eq!(c.search.w_attach, v.parse::<f64>().unwrap());
            }
        }
    }

    #[test]
    fn boosts_and_dedup_threshold_validated() {
        let ok = vec![
            env("ZAM_SEARCH__BOOST_AREA", "1.3"),
            env("ZAM_INSIGHTS__DEDUP_THRESHOLD", "0.85"),
            env("ZAM_INSIGHTS__DECISION_CONFLICT_THRESHOLD", "0.7"),
            env("ZAM_SEARCH__CONTRADICTS_ORDER", "false"),
        ];
        let c = load(None, &ok, &Default::default()).unwrap();
        assert_eq!(c.insights.decision_conflict_threshold, 0.7);
        assert!(!c.search.contradicts_order);
        assert_eq!(
            (c.search.boost_area, c.insights.dedup_threshold),
            (1.3, 0.85)
        );
        for (k, v) in [
            ("ZAM_SEARCH__BOOST_GOTCHA", "0"),
            ("ZAM_SEARCH__BOOST_FAILURE", "-1.0"),
            ("ZAM_SEARCH__BOOST_AREA", "nan"),
            ("ZAM_SEARCH__BOOST_AXIOM", "0"),
            ("ZAM_SEARCH__BOOST_HYPOTHESIS", "-1"),
            ("ZAM_SEARCH__GRAPH_WEIGHT", "1.5"),
            ("ZAM_INSIGHTS__DEDUP_THRESHOLD", "1.5"),
            ("ZAM_INSIGHTS__DECISION_CONFLICT_THRESHOLD", "-0.1"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default());
            assert!(e.is_err(), "{k}={v}");
        }
    }

    #[test]
    fn limits_from_file_and_env() {
        let f = write_in_target(
            "limits.toml",
            "[limits]
note_max_bytes = 1000
",
        );
        let e = vec![env("ZAM_LIMITS__BLOB_MAX_BYTES", "2000")];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        assert_eq!(
            (c.limits.note_max_bytes, c.limits.blob_max_bytes),
            (1000, 2000)
        );
        let bad = vec![env("ZAM_LIMITS__NOTE_MAX_BYTES", "-1")];
        let e = load(None, &bad, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("limits.note_max_bytes"), "{e}");
    }

    #[test]
    fn file_then_env_then_cli() {
        let f = write_in_target(
            "cfg.toml",
            "[server]\nbind = \"127.0.0.1:8000\"\n[log]\nlevel=\"debug\"",
        );
        let env = vec![env("ZAM_SERVER__BIND", "127.0.0.1:9000")];
        let c = load(Some(&f), &env, &Default::default()).unwrap();
        assert_eq!(c.server.bind.port(), 9000);
        assert_eq!(c.log.level, "debug");
        let cli = CliOverrides {
            bind: Some("127.0.0.1:9100".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(load(Some(&f), &env, &cli).unwrap().server.bind.port(), 9100);
    }

    #[test]
    fn env_top_level_and_case_insensitive() {
        let env = vec![
            env("ZAM_DATA_DIR", "/srv/zam"),
            env("zam_log__max_files", "3"),
            env("PATH", "/usr/bin"),
        ];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/srv/zam"));
        assert_eq!(c.log.max_files, 3);
    }

    #[test]
    fn cli_data_dir() {
        let cli = CliOverrides {
            data_dir: Some("/x/y".into()),
            ..Default::default()
        };
        let c = load(None, &[], &cli).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/x/y"));
    }

    #[test]
    fn reserved_env_are_skipped() {
        let env = vec![
            env("ZAM_ADMIN_PASSWORD", "x"),
            env("ZAM_URL", "x"),
            env("ZAM_TOKEN", "x"),
            env("ZAM_MASTER_KEY", "x"),
            env("ZAM_SESSION", "s"),
            env("ZAM_AGENT", "1"),
            env("ZAM_MACHINE", "box"),
        ];
        assert!(load(None, &env, &Default::default()).is_ok());
    }

    #[test]
    fn build_env_are_skipped() {
        // Сборочные переменные CI/разработчика не должны ломать запуск сервера.
        let env = vec![
            env("ZAM_GIT_SHA", "abc"),
            env("ZAM_WEB_REBUILD", "1"),
            env("ZAM_WEB_INDEX", "/x/index.html"),
            env("ZAM_VCRUNTIME_DLL", "C:/x/vcruntime140.dll"),
            env("ZAM_VC_CRT_DIR", "C:/x/Microsoft.VC145.CRT"),
        ];
        assert_eq!(
            load(None, &env, &Default::default()).unwrap(),
            ServerConfig::default()
        );
    }

    #[test]
    fn unknown_env_key_is_error() {
        let env = vec![env("ZAM_SERVER__BINDD", "1")];
        let e = load(None, &env, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("ZAM_SERVER__BINDD"), "{e}");
    }

    #[test]
    fn unknown_file_key_is_error() {
        let f = write_in_target("unknown.toml", "[server]\nbnd = \"1.2.3.4:1\"\n");
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("server.bnd") && e.contains("file"), "{e}");
    }

    #[test]
    fn missing_file_is_error() {
        let p = std::path::Path::new("no/such/zam.toml");
        assert!(load(Some(p), &[], &Default::default()).is_err());
    }

    #[test]
    fn base_path_normalized() {
        for (raw, want) in [
            ("memory/", "/memory"),
            ("/memory/", "/memory"),
            ("/", ""),
            ("", ""),
        ] {
            let env = vec![env("ZAM_SERVER__BASE_PATH", raw)];
            let c = load(None, &env, &Default::default()).unwrap();
            assert_eq!(c.server.base_path, want, "raw={raw:?}");
        }
    }

    #[test]
    fn trusted_proxies_cidr_and_bare_ip() {
        let f = write_in_target(
            "proxies.toml",
            "[server]\ntrusted_proxies = [\"10.0.0.0/8\", \"127.0.0.1\", \"::1\"]\n",
        );
        let c = load(Some(&f), &[], &Default::default()).unwrap();
        let got: Vec<String> = c
            .server
            .trusted_proxies
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert_eq!(got, ["10.0.0.0/8", "127.0.0.1/32", "::1/128"]);
    }

    #[test]
    fn trusted_proxies_from_env_array() {
        let env = vec![env("ZAM_SERVER__TRUSTED_PROXIES", "[\"192.168.0.0/16\"]")];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.server.trusted_proxies[0].to_string(), "192.168.0.0/16");
    }

    #[test]
    fn enums_lowercase() {
        let env = vec![
            env("ZAM_LOG__FORMAT", "text"),
            env("ZAM_QDRANT__MODE", "external"),
        ];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.log.format, LogFormat::Text);
        assert_eq!(c.qdrant.mode, QdrantMode::External);
    }

    /// Умолчания M4 (решения владельца 2026-10-26, §4.3, §8.2): λ = 0 — жадная упаковка M2
    /// со структурными исправлениями M3; Qdrant сбрасывает сегменты раз в 3 с и ждёт
    /// подтверждения WAL при остановке до 14 с — и в супервизоре, и в конфигурации сервера.
    #[test]
    fn default_lambda_zero_and_qdrant_defaults() {
        let d = load(None, &[], &Default::default()).unwrap();
        assert_eq!(d.index.chunk_boundary_lambda, 0.0);
        assert_eq!(d.index.chunk_params().boundary_lambda, 0.0);
        assert_eq!(d.qdrant.flush_interval_sec, 3);
        assert_eq!(d.qdrant.shutdown_flush_wait_ms, 14_000);
        assert_eq!(zam_index::supervisor::DEFAULT_FLUSH_INTERVAL_SEC, 3);
        assert_eq!(
            zam_index::supervisor::DEFAULT_SHUTDOWN_FLUSH_WAIT,
            std::time::Duration::from_secs(14)
        );
    }

    /// `qdrant.exact_search` (задача 15 M4): по умолчанию выключен (HNSW), `ZAM_*` его
    /// включает (прогоны гейта eval), значение доходит до супервизора.
    #[test]
    fn qdrant_exact_search_off_by_default_and_layers() {
        use zam_index::supervisor::SupervisorConfig;
        let exe = std::path::Path::new("inst");
        let d = load(None, &[], &Default::default()).unwrap();
        assert!(!d.qdrant.exact_search);
        let Some(SupervisorConfig::Managed(m)) = d.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert!(!m.exact_search);
        let on = vec![env("ZAM_QDRANT__EXACT_SEARCH", "true")];
        let c = load(None, &on, &Default::default()).unwrap();
        assert!(c.qdrant.exact_search);
        let Some(SupervisorConfig::Managed(m)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert!(m.exact_search);
        // external: параметр запроса — передаётся и внешнему Qdrant
        let ext = vec![
            env("ZAM_QDRANT__EXACT_SEARCH", "true"),
            env("ZAM_QDRANT__MODE", "external"),
            env("ZAM_QDRANT__URL", "http://127.0.0.1:6334"),
        ];
        let c = load(None, &ext, &Default::default()).unwrap();
        let Some(SupervisorConfig::External(x)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался external");
        };
        assert!(x.exact_search);
    }

    /// `[qdrant]` WAL и сброс (задача 19 M3): слои, перенос в супервизор, пределы.
    #[test]
    fn qdrant_wal_options_layer_and_validate() {
        use zam_index::supervisor::SupervisorConfig;
        let exe = std::path::Path::new("inst");
        let d = load(None, &[], &Default::default()).unwrap();
        let Some(SupervisorConfig::Managed(m)) = d.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert_eq!(
            (
                m.flush_interval_sec,
                m.wal_capacity_mb,
                m.shutdown_flush_wait
            ),
            (
                zam_index::supervisor::DEFAULT_FLUSH_INTERVAL_SEC,
                zam_index::supervisor::DEFAULT_WAL_CAPACITY_MB,
                zam_index::supervisor::DEFAULT_SHUTDOWN_FLUSH_WAIT
            )
        );
        let f = write_in_target(
            "qdrant-wal.toml",
            "[qdrant]\nflush_interval_sec = 2\nwal_capacity_mb = 8\n",
        );
        let e = vec![env("ZAM_QDRANT__SHUTDOWN_FLUSH_WAIT_MS", "1500")];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        let Some(SupervisorConfig::Managed(m)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert_eq!((m.flush_interval_sec, m.wal_capacity_mb), (2, 8));
        assert_eq!(
            m.shutdown_flush_wait,
            std::time::Duration::from_millis(1500)
        );
        for (k, v, key) in [
            (
                "ZAM_QDRANT__FLUSH_INTERVAL_SEC",
                "0",
                "qdrant.flush_interval_sec",
            ),
            (
                "ZAM_QDRANT__FLUSH_INTERVAL_SEC",
                "61",
                "qdrant.flush_interval_sec",
            ),
            ("ZAM_QDRANT__WAL_CAPACITY_MB", "0", "qdrant.wal_capacity_mb"),
            (
                "ZAM_QDRANT__SHUTDOWN_FLUSH_WAIT_MS",
                "60001",
                "qdrant.shutdown_flush_wait_ms",
            ),
        ] {
            let c = load(None, &[env(k, v)], &Default::default()).unwrap();
            let err = c.qdrant.supervisor_config(exe).unwrap_err().to_string();
            assert!(err.contains(key), "{k}={v}: {err}");
        }
    }

    #[test]
    fn qdrant_section_layers_and_supervisor_config() {
        use zam_index::supervisor::{SupervisorConfig, default_binary};
        let f = write_in_target(
            "qdrant.toml",
            "[qdrant]\nhttp_port = 7333\nmax_search_threads = 4\nbinary = \"/opt/q/qdrant\"\n",
        );
        let e = vec![
            env("ZAM_QDRANT__GRPC_PORT", "7334"),
            env("ZAM_QDRANT__OPTIMIZER_CPU_BUDGET", "1"),
        ];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        let exe = std::path::Path::new("inst");
        let Some(SupervisorConfig::Managed(m)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert_eq!((m.http_port, m.grpc_port), (7333, 7334));
        assert_eq!((m.max_search_threads, m.optimizer_cpu_budget), (4, 1));
        assert_eq!(m.binary, std::path::PathBuf::from("/opt/q/qdrant"));

        // По умолчанию — бинарник из комплекта.
        let d = load(None, &[], &Default::default()).unwrap();
        assert_eq!(d.qdrant.binary_path(exe), default_binary(exe));

        // Ключ — только для external; там же обязателен url.
        let e = vec![env("ZAM_QDRANT__API_KEY", "s3cret-key")];
        let c = load(None, &e, &Default::default()).unwrap();
        let err = c.qdrant.supervisor_config(exe).unwrap_err().to_string();
        assert!(err.contains("qdrant.api_key"), "{err}");
        assert!(!format!("{c:?}").contains("s3cret-key"), "ключ в Debug");
        let mut e = vec![
            env("ZAM_QDRANT__MODE", "external"),
            env("ZAM_QDRANT__API_KEY", "s3cret-key"),
        ];
        let c = load(None, &e, &Default::default()).unwrap();
        let err = c.qdrant.supervisor_config(exe).unwrap_err().to_string();
        assert!(err.contains("qdrant.url"), "{err}");
        e.push(env("ZAM_QDRANT__URL", "http://10.0.0.5:6334"));
        let c = load(None, &e, &Default::default()).unwrap();
        let Some(SupervisorConfig::External(x)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался external");
        };
        assert_eq!(x.grpc_url, "http://10.0.0.5:6334");
        assert_eq!(x.api_key.as_deref(), Some("s3cret-key"));

        let e = vec![env("ZAM_QDRANT__MODE", "edge")];
        let c = load(None, &e, &Default::default()).unwrap();
        assert!(c.qdrant.supervisor_config(exe).unwrap().is_none());
    }

    #[test]
    fn ml_and_index_sections() {
        let d = load(None, &[], &Default::default()).unwrap();
        assert_eq!(d.ml.device, MlDevice::Auto);
        assert_eq!((d.ml.index_threads, d.ml.query_threads), (0, 2));
        assert_eq!((&d.ml.embed_model, &d.ml.ort_dylib), (&None, &None));
        assert_eq!(d.ml.unload_after().unwrap(), None);
        assert_eq!(
            d.ml.models_dir(std::path::Path::new("dd")),
            std::path::Path::new("dd").join("models")
        );
        assert_eq!(
            (
                d.index.batch_tokens,
                d.index.yield_ms,
                d.index.rollup_debounce_ms
            ),
            (8192, 5, 2000)
        );
        let f = write_in_target(
            "ml.toml",
            "[ml]
device = \"cpu\"
embed_model = \"minilm-l6-int8\"
unload_after = \"30m\"
[index]
yield_ms = 1
",
        );
        let e = vec![env("ZAM_ML__QUERY_THREADS", "4")];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        assert_eq!(c.ml.device, MlDevice::Cpu);
        assert_eq!(c.ml.embed_model.as_deref(), Some("minilm-l6-int8"));
        assert_eq!(c.ml.query_threads, 4);
        assert_eq!(
            c.ml.unload_after().unwrap(),
            Some(std::time::Duration::from_secs(1800))
        );
        assert_eq!(c.index.yield_ms, 1);
        for bad in ["10", "5x", "0s", "m", "5м", "м", "18446744073709551615h"] {
            let e = vec![env("ZAM_ML__UNLOAD_AFTER", bad)];
            let err = load(None, &e, &Default::default()).unwrap_err().to_string();
            assert!(err.contains("ml.unload_after"), "{bad}: {err}");
        }
        let e = vec![env("ZAM_ML__DEVICE", "directml")];
        assert!(load(None, &e, &Default::default()).is_err());
        // пороги чанкинга: умолчания сервера и чанкера совпадают (одно значение, подобранное
        // на eval, §18.2), в том числе λ = 0 (M4); переопределение и проверка согласованности
        assert_eq!(d.index.chunk_params(), zam_markdown::ChunkParams::default());
        let e = vec![
            env("ZAM_INDEX__CHUNK_TARGET_MAX", "700"),
            env("ZAM_INDEX__CHUNK_OVERLAP_SENTENCES", "2"),
        ];
        let c = load(None, &e, &Default::default()).unwrap();
        assert_eq!(
            (
                c.index.chunk_params().target_max,
                c.index.chunk_params().overlap_sentences
            ),
            (700, 2)
        );
        for (k, v) in [
            ("ZAM_INDEX__CHUNK_TARGET_MAX", "900"),
            ("ZAM_INDEX__CHUNK_TARGET_MIN", "0"),
            ("ZAM_INDEX__CHUNK_TARGET_MIN", "600"),
            ("ZAM_INDEX__CHUNK_HARD_MAX", "8193"),
            ("ZAM_INDEX__CHUNK_MERGE_BELOW", "501"),
            ("ZAM_INDEX__CHUNK_OVERLAP_SENTENCES", "6"),
            ("ZAM_INDEX__BATCH_TOKENS", "0"),
            ("ZAM_INDEX__CHUNK_BOUNDARY_LAMBDA", "-1"),
            ("ZAM_INDEX__CHUNK_BOUNDARY_LAMBDA", "11"),
            ("ZAM_INDEX__CHUNK_BOUNDARY_LAMBDA", "nan"),
        ] {
            let field = k.trim_start_matches("ZAM_INDEX__").to_ascii_lowercase();
            let err = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            assert!(err.contains(&format!("index.{field}")), "{k}={v}: {err}");
        }
        // предел листа — от окна модели (минус бюджет префикса), а не константа
        let c = load(
            None,
            &[env("ZAM_INDEX__CHUNK_HARD_MAX", "2000")],
            &Default::default(),
        )
        .unwrap();
        assert_eq!(c.index.chunk_params().hard_max, 2000);
        assert_eq!(c.index.chunk_params_for(Some(4096)).0.hard_max, 2000);
        let (p, clamped) = c.index.chunk_params_for(None);
        assert!(clamped);
        assert_eq!(p.hard_max, DEFAULT_PASSAGE_WINDOW - CHUNK_PREFIX_BUDGET);
        let (p, clamped) = d.index.chunk_params_for(Some(1024));
        assert!(!clamped, "умолчания укладываются в окно bge-m3");
        assert_eq!(p, d.index.chunk_params());
        // MiniLM (256): все пороги не больше 192, порядок порогов сохранён
        let (p, clamped) = d.index.chunk_params_for(Some(256));
        assert!(clamped);
        assert_eq!(
            (p.hard_max, p.target_max, p.target_min, p.merge_below),
            (192, 192, 192, 100)
        );
        assert_eq!(d.index.chunk_params_for(Some(10)).0.hard_max, 1);
        // сила границ (задача 17 M3): флаги — в пороги чанкера; L0 в префиксе уменьшает предел
        // листа на свой бюджет
        let p = d.index.chunk_params();
        assert_eq!((p.cohesion, p.heading_path_h4), (false, true));
        assert!(!d.index.chunk_prefix_l0);
        // связность без λ ничего не делает — признак для предупреждения при старте
        assert!(!d.index.cohesion_ignored());
        let mut i = d.index.clone();
        i.chunk_cohesion = true;
        assert!(
            i.cohesion_ignored(),
            "при умолчании λ = 0 связность не действует"
        );
        i.chunk_boundary_lambda = 0.5;
        assert!(!i.cohesion_ignored());
        let c = load(
            None,
            &[
                env("ZAM_INDEX__CHUNK_BOUNDARY_LAMBDA", "2"),
                env("ZAM_INDEX__CHUNK_COHESION", "true"),
                env("ZAM_INDEX__CHUNK_HEADING_PATH_H4", "false"),
                env("ZAM_INDEX__CHUNK_PREFIX_L0", "true"),
                env("ZAM_INDEX__CHUNK_HARD_MAX", "2000"),
            ],
            &Default::default(),
        )
        .unwrap();
        let (p, _) = c.index.chunk_params_for(Some(1024));
        assert_eq!(
            (p.boundary_lambda, p.cohesion, p.heading_path_h4, p.hard_max),
            (
                2.0,
                true,
                false,
                1024 - CHUNK_PREFIX_BUDGET - zam_store::PREFIX_L0_MAX_TOKENS
            )
        );
        // реранкер
        assert_eq!(
            (
                d.ml.rerank_model_cpu.as_str(),
                d.ml.rerank_model_gpu.as_str()
            ),
            ("jina-reranker-v2", "bge-reranker-v2-m3")
        );
        assert_eq!((d.ml.rerank_top, d.ml.rerank_tokens), (20, 256));
        let e = vec![
            env("ZAM_ML__RERANK_TOP", "10"),
            env("ZAM_ML__RERANK_MODEL_CPU", "bge-reranker-v2-m3"),
        ];
        let c = load(None, &e, &Default::default()).unwrap();
        assert_eq!(
            (c.ml.rerank_top, c.ml.rerank_model_cpu.as_str()),
            (10, "bge-reranker-v2-m3")
        );
        for (k, v) in [
            ("ZAM_ML__RERANK_TOP", "0"),
            ("ZAM_ML__RERANK_TOP", "101"),
            ("ZAM_ML__RERANK_TOKENS", "4"),
            ("ZAM_ML__RERANK_MODEL_GPU", "../x"),
            ("ZAM_ML__RERANK_MODEL_CPU", ""),
        ] {
            let err = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            assert!(err.contains("ml.rerank"), "{k}={v}: {err}");
        }
        let e = vec![env("ZAM_INDEX__BATCH_TOKENS", "0")];
        assert!(load(None, &e, &Default::default()).is_err());
    }

    #[test]
    fn search_section() {
        let d = load(None, &[], &Default::default()).unwrap().search;
        assert_eq!(
            (d.rrf_k, d.w_vector, d.w_fts, d.w_trigram),
            (60.0, 1.0, 1.0, 0.7)
        );
        assert_eq!((d.recency_half_life_days, d.max_per_node), (180.0, 3));
        assert_eq!(d.max_per_attachment, 2);
        assert_eq!(d.attach_prefetch, 30);
        for v in ["0", "501"] {
            let bad = vec![env("ZAM_SEARCH__ATTACH_PREFETCH", v)];
            assert!(load(None, &bad, &Default::default()).is_err(), "{v}");
        }
        // ослабленный bm25 — вес по eval (§18.2)
        assert_eq!(d.w_fts_relaxed, 0.25);
        assert_eq!(d.w_fts_alias, 0.5);
        let bad = vec![env("ZAM_SEARCH__W_FTS_ALIAS", "1.5")];
        assert!(load(None, &bad, &Default::default()).is_err());
        let e = vec![
            env("ZAM_SEARCH__RRF_K", "30"),
            env("ZAM_SEARCH__W_VECTOR", "1.5"),
            env("ZAM_SEARCH__RECENCY_HALF_LIFE_DAYS", "0"),
        ];
        let c = load(None, &e, &Default::default()).unwrap().search;
        assert_eq!(
            (c.rrf_k, c.w_vector, c.recency_half_life_days),
            (30.0, 1.5, 0.0)
        );
        for (k, v) in [
            ("ZAM_SEARCH__RRF_K", "0"),
            ("ZAM_SEARCH__W_FTS", "-1"),
            ("ZAM_SEARCH__W_TRIGRAM", "nan"),
            ("ZAM_SEARCH__MAX_PER_NODE", "0"),
            ("ZAM_SEARCH__W_FOLDER", "-0.5"),
        ] {
            let err = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            assert!(err.contains("search."), "{k}={v}: {err}");
        }
    }

    #[test]
    fn bad_value_names_key_and_source() {
        let env = vec![env("ZAM_SERVER__BIND", "nope")];
        let e = load(None, &env, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("server.bind") && e.contains("ZAM_SERVER__BIND"),
            "{e}"
        );
    }

    #[test]
    fn admin_password_missing_is_error() {
        let c = load(None, &[], &Default::default()).unwrap();
        let e = c.admin_secret(None).unwrap_err().to_string();
        assert!(e.contains("hash-password") && e.contains("ZAM_ADMIN_PASSWORD"));
    }

    #[test]
    fn admin_password_from_env_is_hashed() {
        let c = load(None, &[], &Default::default()).unwrap();
        let AdminSecret::Hash(h) = c.admin_secret(Some("s3cret")).unwrap();
        assert!(h.starts_with("$argon2id$") && !h.contains("s3cret"));
    }

    #[test]
    fn default_params_detected() {
        assert!(has_default_params(
            &hash_password("пароль-для-теста").unwrap()
        ));
        let strong = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(65536, 3, 1, None).unwrap(),
        );
        let salt = SaltString::generate(&mut OsRng);
        let h = strong.hash_password(b"x", &salt).unwrap().to_string();
        assert!(!has_default_params(&h));
        assert!(!has_default_params("не хэш"));
    }

    #[test]
    fn admin_hash_from_config_wins() {
        let AdminSecret::Hash(h) = load(None, &[], &Default::default())
            .unwrap()
            .admin_secret(Some("pw"))
            .unwrap();
        let env = vec![env("ZAM_ADMIN__PASSWORD_HASH", &h)];
        let c = load(None, &env, &Default::default()).unwrap();
        let AdminSecret::Hash(got) = c.admin_secret(Some("other")).unwrap();
        assert_eq!(got, h);
    }

    #[test]
    fn invalid_admin_hash_is_error() {
        let env = vec![env("ZAM_ADMIN__PASSWORD_HASH", "plain")];
        let c = load(None, &env, &Default::default()).unwrap();
        assert!(c.admin_secret(None).is_err());
    }

    #[test]
    fn vault_master_key_file_from_file_and_env() {
        assert_eq!(ServerConfig::default().vault.master_key_file, None);
        let f = crate::test_util::write_in_target(
            "vault.toml",
            "[vault]
master_key_file = \"/srv/k/master.key\"
",
        );
        let c = load(Some(&f), &[], &Default::default()).unwrap();
        assert_eq!(
            c.vault.master_key_file,
            Some(std::path::PathBuf::from("/srv/k/master.key"))
        );
        let e = vec![env("ZAM_VAULT__MASTER_KEY_FILE", "/x/m.key")];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        assert_eq!(
            c.vault.master_key_file,
            Some(std::path::PathBuf::from("/x/m.key"))
        );
        // сам ключ — не ключ конфигурации
        let e = vec![env("ZAM_MASTER_KEY", "x")];
        assert!(load(None, &e, &Default::default()).is_ok());
        let f = crate::test_util::write_in_target(
            "vault-bad.toml",
            "[vault]
master_key = \"x\"
",
        );
        assert!(load(Some(&f), &[], &Default::default()).is_err());
    }

    #[test]
    fn recall_section_bounds() {
        for (k, v) in [
            ("ZAM_RECALL__DIGEST_TOKENS", "0"),
            ("ZAM_RECALL__RECOVERY_TOKENS", "0"),
            ("ZAM_RECALL__RECOVERY_TOKENS", "20001"),
            ("ZAM_RECALL__BRIEF_TOKENS", "0"),
            ("ZAM_RECALL__BRIEF_TOKENS", "20001"),
            ("ZAM_RECALL__PROMPT_MIN_SCORE", "1.5"),
            ("ZAM_RECALL__TIMEOUT_MS", "0"),
            ("ZAM_RECALL__DIGEST_ENTRIES", "0"),
            ("ZAM_RECALL__DIGEST_ENTRIES", "21"),
            ("ZAM_RECALL__DIGEST_CODE_MAP_TOKENS", "1001"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            assert!(e.contains("recall."), "{k}: {e}");
        }
        let c = load(
            None,
            &[env("ZAM_RECALL__PROMPT_MIN_SCORE", "0.5")],
            &Default::default(),
        )
        .unwrap();
        assert_eq!(c.recall.prompt_min_score, 0.5);
        assert_eq!(c.recall.digest_tokens, 1500);
        assert_eq!(c.recall.recovery_tokens, 2000);
        assert_eq!(c.recall.brief_tokens, 800);
        // M6 §8: записей в разделах дайджеста и мини-карта кода (0 — только строка-указатель)
        assert_eq!(c.recall.digest_entries, 5);
        assert_eq!(c.recall.digest_code_map_tokens, 0);
        let c = load(
            None,
            &[
                env("ZAM_RECALL__DIGEST_ENTRIES", "20"),
                env("ZAM_RECALL__DIGEST_CODE_MAP_TOKENS", "1000"),
            ],
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            (c.recall.digest_entries, c.recall.digest_code_map_tokens),
            (20, 1000)
        );
    }

    #[test]
    fn reports_section_defaults() {
        let r = load(None, &[], &Default::default()).unwrap().reports;
        assert_eq!(
            (r.max_files, r.max_total_bytes, r.mcp_max_bytes),
            (64, 209_715_200, 16_777_216)
        );
    }

    #[test]
    fn reports_section_validation() {
        for (k, v) in [
            ("ZAM_REPORTS__MAX_FILES", "0"),
            ("ZAM_REPORTS__MAX_FILES", "1025"),
            ("ZAM_REPORTS__MAX_TOTAL_BYTES", "0"),
            ("ZAM_REPORTS__MCP_MAX_BYTES", "0"),
            ("ZAM_REPORTS__MCP_MAX_BYTES", "209715201"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            assert!(e.contains("reports."), "{k}={v}: {e}");
        }
        // mcp_max_bytes сверяется с итоговым max_total_bytes
        let e = load(
            None,
            &[env("ZAM_REPORTS__MAX_TOTAL_BYTES", "1048576")],
            &Default::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("reports.mcp_max_bytes"), "{e}");
        let ok = vec![
            env("ZAM_REPORTS__MAX_FILES", "1024"),
            env("ZAM_REPORTS__MAX_TOTAL_BYTES", "1048576"),
            env("ZAM_REPORTS__MCP_MAX_BYTES", "1048576"),
        ];
        let c = load(None, &ok, &Default::default()).unwrap();
        assert_eq!(c.reports.max_files, 1024);
        // неизвестный ключ секции — ошибка
        let e = load(
            None,
            &[env("ZAM_REPORTS__MAX_IMAGES", "3")],
            &Default::default(),
        );
        assert!(e.is_err());
    }

    #[test]
    fn ideas_section_defaults_and_bounds() {
        let c = load(None, &[], &Default::default()).unwrap().ideas;
        assert_eq!(c, IdeasSection::default());
        assert!(c.proactive);
        assert_eq!(
            (c.digest_max, c.prompt_max, c.prompt_top_k, c.defer_days),
            (3, 1, 5, 30)
        );
        assert_eq!(c.defer_days, zam_store::IDEA_DEFER_DAYS_DEFAULT);
        for (k, v) in [
            ("ZAM_IDEAS__DIGEST_MAX", "11"),
            ("ZAM_IDEAS__PROMPT_MAX", "4"),
            ("ZAM_IDEAS__PROMPT_TOP_K", "0"),
            ("ZAM_IDEAS__PROMPT_TOP_K", "21"),
            ("ZAM_IDEAS__DEFER_DAYS", "0"),
            ("ZAM_IDEAS__DEFER_DAYS", "3651"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            let key = k.trim_start_matches("ZAM_IDEAS__").to_lowercase();
            assert!(e.contains(&format!("ideas.{key}")), "{k}={v}: {e}");
        }
        let ok = vec![
            env("ZAM_IDEAS__PROACTIVE", "false"),
            env("ZAM_IDEAS__DIGEST_MAX", "0"),
            env("ZAM_IDEAS__PROMPT_MAX", "3"),
            env("ZAM_IDEAS__PROMPT_TOP_K", "20"),
            env("ZAM_IDEAS__DEFER_DAYS", "3650"),
        ];
        let c = load(None, &ok, &Default::default()).unwrap().ideas;
        assert!(!c.proactive);
        assert_eq!(
            (c.digest_max, c.prompt_max, c.prompt_top_k, c.defer_days),
            (0, 3, 20, 3650)
        );
        let e = load(None, &[env("ZAM_IDEAS__AUTO", "true")], &Default::default());
        assert!(e.is_err(), "неизвестный ключ секции — ошибка");
        // ответ владельца на вопрос 8 (А): порога косинуса нет — ключ не выпускался ни в одном
        // релизе и отклоняется как неизвестный
        let e = load(
            None,
            &[env("ZAM_IDEAS__KNOWLEDGE_MIN_COSINE", "0.7")],
            &Default::default(),
        );
        assert!(e.is_err(), "knowledge_min_cosine удалён");
    }

    #[test]
    fn code_section_defaults() {
        let c = load(None, &[], &Default::default()).unwrap().code;
        assert_eq!(
            c,
            CodeSection {
                enabled: true,
                max_files: 50_000,
                max_symbols_per_file: 5000,
                max_refs_per_file: 20_000,
                max_request_bytes: 8_388_608,
                map_tokens: 1000,
                brief_tokens: 150,
                rank_timeout_ms: 1000,
            }
        );
    }

    #[test]
    fn code_section_validation() {
        for (k, v) in [
            ("ZAM_CODE__MAX_FILES", "0"),
            ("ZAM_CODE__MAX_FILES", "500001"),
            ("ZAM_CODE__MAX_SYMBOLS_PER_FILE", "0"),
            ("ZAM_CODE__MAX_SYMBOLS_PER_FILE", "100001"),
            ("ZAM_CODE__MAX_REFS_PER_FILE", "0"),
            ("ZAM_CODE__MAX_REFS_PER_FILE", "1000001"),
            ("ZAM_CODE__MAX_REQUEST_BYTES", "1023"),
            ("ZAM_CODE__MAX_REQUEST_BYTES", "268435457"),
            ("ZAM_CODE__MAP_TOKENS", "0"),
            ("ZAM_CODE__MAP_TOKENS", "20001"),
            ("ZAM_CODE__BRIEF_TOKENS", "1001"),
            ("ZAM_CODE__RANK_TIMEOUT_MS", "60001"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            let key = k.trim_start_matches("ZAM_CODE__").to_lowercase();
            assert!(e.contains(&format!("code.{key}")), "{k}={v}: {e}");
        }
        let ok = vec![
            env("ZAM_CODE__ENABLED", "false"),
            env("ZAM_CODE__MAX_FILES", "500000"),
            env("ZAM_CODE__MAX_REQUEST_BYTES", "1024"),
            env("ZAM_CODE__MAP_TOKENS", "20000"),
            env("ZAM_CODE__BRIEF_TOKENS", "0"),
            env("ZAM_CODE__RANK_TIMEOUT_MS", "0"),
        ];
        let c = load(None, &ok, &Default::default()).unwrap().code;
        assert!(!c.enabled);
        assert_eq!((c.max_files, c.brief_tokens), (500_000, 0));
        assert_eq!(c.rank_timeout_ms, 0, "0 — пересчёт только в фоне");
        let e = load(None, &[env("ZAM_CODE__AUTO", "true")], &Default::default());
        assert!(e.is_err(), "неизвестный ключ секции — ошибка");
    }

    /// M6 §9: `recall.prompt_min_score_fts` и `recall.post_tool_coverage` — умолчания, `ZAM_*`
    /// вложенного ключа, диапазоны.
    #[test]
    fn recall_hint_threshold_keys() {
        use zam_search::recall::WordMatch;
        let d = load(None, &[], &Default::default()).unwrap().recall;
        assert_eq!(
            d.prompt_min_score_fts, None,
            "не задан — как prompt_min_score"
        );
        assert_eq!(d.post_tool_coverage, CoverageRule::M6);
        let e = vec![
            env("ZAM_RECALL__PROMPT_MIN_SCORE_FTS", "0.3"),
            env("ZAM_RECALL__POST_TOOL_COVERAGE__DIVISOR", "2"),
            env("ZAM_RECALL__POST_TOOL_COVERAGE__CAP", "6"),
            env("ZAM_RECALL__POST_TOOL_COVERAGE__WORD_MATCH", "stems"),
        ];
        let r = load(None, &e, &Default::default()).unwrap().recall;
        assert_eq!(r.prompt_min_score_fts, Some(0.3));
        assert_eq!(
            r.post_tool_coverage,
            CoverageRule {
                divisor: 2,
                cap: 6,
                word_match: WordMatch::Stems
            }
        );
        for (k, v) in [
            ("ZAM_RECALL__PROMPT_MIN_SCORE_FTS", "1.5"),
            ("ZAM_RECALL__POST_TOOL_COVERAGE__DIVISOR", "0"),
            ("ZAM_RECALL__POST_TOOL_COVERAGE__CAP", "1"),
            ("ZAM_RECALL__POST_TOOL_COVERAGE__CAP", "33"),
            ("ZAM_RECALL__POST_TOOL_COVERAGE__WORD_MATCH", "snowball"),
        ] {
            assert!(
                load(None, &[env(k, v)], &Default::default()).is_err(),
                "{k}={v}"
            );
        }
    }

    #[test]
    fn backup_section_defaults_and_ranges() {
        let b = load(None, &[], &Default::default()).unwrap().backup;
        assert_eq!(b, BackupSection::default());
        assert_eq!(b.schedule, BackupSchedule::Off);
        assert_eq!(
            (b.at_hm(), b.keep, b.warn_days, b.flag_max_hours),
            ((3, 30), 7, 7, 6)
        );
        let ok = load(
            None,
            &[
                env("ZAM_BACKUP__SCHEDULE", "daily"),
                env("ZAM_BACKUP__AT", "23:05"),
            ],
            &Default::default(),
        )
        .unwrap()
        .backup;
        assert_eq!((ok.schedule, ok.at_hm()), (BackupSchedule::Daily, (23, 5)));
        for (k, v) in [
            ("ZAM_BACKUP__AT", "24:00"),
            ("ZAM_BACKUP__AT", "3:30"),
            ("ZAM_BACKUP__KEEP", "0"),
            ("ZAM_BACKUP__KEEP", "366"),
            ("ZAM_BACKUP__WARN_DAYS", "0"),
            ("ZAM_BACKUP__FLAG_MAX_HOURS", "169"),
            ("ZAM_BACKUP__SCHEDULE", "weekly"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default());
            assert!(e.unwrap_err().to_string().contains("backup."), "{k}={v}");
        }
    }

    #[test]
    fn export_section_defaults_and_ranges() {
        let e = load(None, &[], &Default::default()).unwrap().export;
        assert_eq!(e.max_secs, 14_400);
        for v in ["0", "604801"] {
            let r = load(None, &[env("ZAM_EXPORT__MAX_SECS", v)], &Default::default());
            assert!(
                r.unwrap_err().to_string().contains("export.max_secs"),
                "{v}"
            );
        }
    }

    #[test]
    fn import_section_defaults_and_ranges() {
        let i = load(None, &[], &Default::default()).unwrap().import;
        assert_eq!(i.max_bytes, 2 << 30);
        assert_eq!(i.max_files, 100_000);
        assert_eq!(i.max_text_bytes, 256 << 20);
        assert_eq!(i.min_free_bytes, 2 << 30);
        for (k, v) in [
            ("ZAM_IMPORT__MAX_BYTES", "1048575"),
            ("ZAM_IMPORT__MAX_BYTES", "68719476737"),
            ("ZAM_IMPORT__MAX_FILES", "0"),
            ("ZAM_IMPORT__MAX_FILES", "1000001"),
            ("ZAM_IMPORT__MAX_TEXT_BYTES", "1048575"),
            ("ZAM_IMPORT__MIN_FREE_BYTES", "1099511627777"),
        ] {
            let r = load(None, &[env(k, v)], &Default::default());
            assert!(
                r.unwrap_err().to_string().contains(&format!(
                    "import.{}",
                    k.trim_start_matches("ZAM_IMPORT__").to_lowercase()
                )),
                "{k}={v}"
            );
        }
    }

    /// M6 §8а: ключи снимка — умолчания и пределы.
    #[test]
    fn snapshot_keys_defaults_and_ranges() {
        let c = load(None, &[], &Default::default()).unwrap();
        assert_eq!(
            (
                c.recall.snapshot_tokens,
                c.recall.snapshot_stale_min,
                c.recall.snapshot_digest_days
            ),
            (400, 30, 2)
        );
        assert_eq!((c.snapshot.max_bytes, c.snapshot.keep_days), (16_384, 14));
        for (k, v) in [
            ("ZAM_RECALL__SNAPSHOT_TOKENS", "2001"),
            ("ZAM_RECALL__SNAPSHOT_STALE_MIN", "1441"),
            ("ZAM_RECALL__SNAPSHOT_DIGEST_DAYS", "0"),
            ("ZAM_RECALL__SNAPSHOT_DIGEST_DAYS", "31"),
            ("ZAM_SNAPSHOT__MAX_BYTES", "1023"),
            ("ZAM_SNAPSHOT__MAX_BYTES", "65537"),
            ("ZAM_SNAPSHOT__KEEP_DAYS", "0"),
            ("ZAM_SNAPSHOT__KEEP_DAYS", "91"),
        ] {
            let e = load(None, &[env(k, v)], &Default::default())
                .unwrap_err()
                .to_string();
            let key = k
                .trim_start_matches("ZAM_")
                .replace("__", ".")
                .to_lowercase();
            assert!(e.contains(&key), "{k}={v}: {e}");
        }
        let c = load(
            None,
            &[
                env("ZAM_RECALL__SNAPSHOT_TOKENS", "0"),
                env("ZAM_SNAPSHOT__MAX_BYTES", "65536"),
            ],
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            (c.recall.snapshot_tokens, c.snapshot.max_bytes),
            (0, 65_536)
        );
    }

    #[test]
    fn example_config_parses_to_defaults() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/config.example.toml");
        let c = load(Some(&p), &[], &Default::default()).unwrap();
        assert_eq!(c, load(None, &[], &Default::default()).unwrap());
    }

    #[test]
    fn env_string_like_values_for_string_fields() {
        for (raw, want) in [("123", "123"), ("true", "true"), ("nan", "nan")] {
            let env = vec![env("ZAM_ADMIN__LOGIN", raw)];
            let c = load(None, &env, &Default::default()).unwrap();
            assert_eq!(c.admin.login, want);
        }
        let e2 = vec![
            env("ZAM_DATA_DIR", "2024-01-01"),
            env("ZAM_SERVER__BASE_PATH", "123"),
        ];
        let c = load(None, &e2, &Default::default()).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("2024-01-01"));
        assert_eq!(c.server.base_path, "/123");
        let e3 = vec![env("ZAM_LOG__MAX_FILES", "7")];
        assert_eq!(
            load(None, &e3, &Default::default()).unwrap().log.max_files,
            7
        );
        let e4 = vec![env("ZAM_LOG__MAX_FILES", "abc")];
        let e = load(None, &e4, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("log.max_files") && e.contains("ZAM_LOG__MAX_FILES"),
            "{e}"
        );
    }

    #[test]
    fn empty_unknown_table_is_error() {
        let f = write_in_target("bogus.toml", "[bogus]\n");
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("bogus"), "{e}");
        let f = write_in_target("empty_known.toml", "[server]\n[log]\n");
        assert!(load(Some(&f), &[], &Default::default()).is_ok());
    }

    #[test]
    fn syntax_error_does_not_quote_source() {
        let f = write_in_target(
            "broken.toml",
            "[admin]\npassword_hash = \"$argon2id$SECRET\n",
        );
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(!e.contains("SECRET"), "{e}");
        assert!(e.contains("строка 2"), "{e}");
    }

    #[test]
    fn docs_section_defaults_and_memory_check() {
        let d = load(None, &[], &Default::default()).unwrap().docs;
        assert_eq!(
            (d.workers, d.worker_jobs, d.worker_memory_mb),
            (2, 200, 2048)
        );
        assert_eq!(d.timeout_ms, 60_000);
        assert_eq!((d.preview_workers, d.preview_cache_mb), (1, 1024));
        assert_eq!(d.limits(), zam_docs::Limits::default());
        // 3 × 100 МиБ + 64 МиБ = 364 МиБ > 256
        let e = load(
            None,
            &[env("ZAM_DOCS__WORKER_MEMORY_MB", "256")],
            &Default::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("docs.worker_memory_mb") && e.contains("364"),
            "{e}"
        );
        let ok = vec![
            env("ZAM_DOCS__WORKER_MEMORY_MB", "256"),
            env("ZAM_DOCS__MAX_BYTES", "10485760"),
        ];
        assert!(load(None, &ok, &Default::default()).is_ok());
        let e = load(None, &[env("ZAM_DOCS__WORKERS", "0")], &Default::default());
        assert!(e.unwrap_err().to_string().contains("docs.workers"));
        let e = load(
            None,
            &[env("ZAM_DOCS__PREVIEW_WORKERS", "17")],
            &Default::default(),
        );
        assert!(e.unwrap_err().to_string().contains("docs.preview_workers"));
        let e = load(
            None,
            &[env("ZAM_DOCS__PREVIEW_CACHE_MB", "0")],
            &Default::default(),
        );
        assert!(e.unwrap_err().to_string().contains("docs.preview_cache_mb"));
    }
}
