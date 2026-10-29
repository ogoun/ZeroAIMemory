//! `cargo xtask eval` — оценка качества поиска (ТЗ §18.2) на корпусе `eval/`.
//!
//! Прогон поднимает собранный `zam-server` на временном каталоге в `target/eval/` (обвязка
//! smoke: запуск, токен, API), загружает корпус в Knowledge, ждёт векторной индексации и
//! выполняет запросы `eval/queries.toml` (`mode = notes`, top 10). Метрики — recall@5,
//! MRR@10, nDCG@10 по режимам и видам запросов с 95 % доверительными интервалами
//! бутстрепа по запросам. Результат — `eval/results/<дата>-<модель>-<режим>.json` и `.md`.
//!
//! Режимы:
//! - `fts` — сервер без моделей (FTS-only, как свежая установка без `models fetch`);
//! - `hybrid` — модель `--model` на устройстве `--device` (Qdrant + FTS, weighted RRF);
//! - `hybrid+rerank` — то же с `rerank: on` (реранкер устройства эмбеддера).
//!
//! `--quality-gate` сравнивает int8 (и fp16, если есть CUDA) с fp32-эталоном BAAI
//! ([`gate`]); `--grid` подбирает веса `[search]` и пороги чанкинга ([`grid`]); `--check`
//! только проверяет корпус и запросы ([`corpus::validate`]), а у полного набора — ещё подсказки
//! и сценарии M4.
//!
//! Eval M4 (задача 15): `--hints` — точность подсказок хуков `eval/hints.toml`, калибровка
//! порогов и задержки `/recall` и хуков ([`hints`]); `--tokens` — расход токенов агента по
//! сценариям `eval/scenarios.toml` ([`tokens`]).
//!
//! Eval M4.5 (задача 15): `--glossary` — словарь RU↔EN `eval/glossary.toml`, канал `alias` и
//! его вес, основной набор со словарём и связями `contradicts` ([`glossary`]).

pub mod compare;
pub mod corpus;
pub mod gate;
pub mod glossary;
pub mod grid;
pub mod hints;
pub mod insights;
pub mod leaves;
pub mod metrics;
pub mod report;
pub mod run;
pub mod tokens;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use corpus::{Corpus, Query, load_insights, load_queries, validate};

/// Режим поиска прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, serde::Serialize)]
pub enum Mode {
    #[value(name = "fts")]
    #[serde(rename = "fts")]
    Fts,
    #[value(name = "hybrid")]
    #[serde(rename = "hybrid")]
    Hybrid,
    #[value(name = "hybrid+rerank")]
    #[serde(rename = "hybrid+rerank")]
    HybridRerank,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fts => "fts",
            Mode::Hybrid => "hybrid",
            Mode::HybridRerank => "hybrid+rerank",
        }
    }

    /// Для имени файла: `+` → `-`.
    pub fn file_tag(self) -> &'static str {
        match self {
            Mode::Fts => "fts",
            Mode::Hybrid => "hybrid",
            Mode::HybridRerank => "hybrid-rerank",
        }
    }
}

/// Устройство инференса прогона (`ml.device` сервера).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Cpu,
    Cuda,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
        }
    }
}

/// Аргументы `cargo xtask eval`.
#[derive(Debug, clap::Args)]
pub struct EvalArgs {
    /// Модель эмбеддингов из deploy/models.toml (target/models/<id>); по умолчанию
    /// bge-m3-int8 на cpu и bge-m3-fp16 на cuda
    #[arg(long)]
    pub model: Option<String>,
    /// Режимы (можно повторять; один сервер и один индекс на все режимы прогона)
    #[arg(long = "mode", value_enum, default_values_t = [Mode::Hybrid])]
    pub modes: Vec<Mode>,
    /// Устройство инференса
    #[arg(long, value_enum, default_value_t = Device::Cpu)]
    pub device: Device,
    /// Реранкер вместо выбранного сервером по устройству (ml.rerank_model_cpu|gpu)
    #[arg(long)]
    pub reranker: Option<String>,
    /// Гейт качества: int8 (и fp16 при доступной CUDA) против fp32-эталона BAAI (точный
    /// поиск Qdrant, `qdrant.exact_search = true`)
    #[arg(long, conflicts_with_all = ["grid", "check", "hints", "tokens"])]
    pub quality_gate: bool,
    /// Точность подсказок хуков (eval/hints.toml), калибровка порогов, задержки /recall и хуков
    #[arg(long, conflicts_with_all = ["grid", "check", "tokens"])]
    pub hints: bool,
    /// Расход токенов агента по сценариям eval/scenarios.toml (протокол §9.3, кэш, chunks)
    #[arg(long, conflicts_with_all = ["grid", "check"])]
    pub tokens: bool,
    /// Гейт без справочного прогона fp16 на CUDA (GPU занят): вердикт — по int8 и fp32 на CPU
    #[arg(long, requires = "quality_gate")]
    pub skip_cuda: bool,
    /// zam для хуков в --hints (по умолчанию собирается `cargo build --release -p zam-cli`)
    #[arg(long)]
    pub cli_bin: Option<PathBuf>,
    /// Подбор весов [search] и порогов чанкинга по сетке из файла (eval/grid.toml)
    #[arg(long, value_name = "FILE", conflicts_with = "check")]
    pub grid: Option<PathBuf>,
    /// Только проверить корпус и запросы (без сервера)
    #[arg(long)]
    pub check: bool,
    /// Файл запросов вместо eval/queries.toml (с --check — проверка фрагмента без
    /// требований к полному набору)
    #[arg(long, value_name = "FILE")]
    pub queries: Option<PathBuf>,
    /// Переопределение конфигурации сервера KEY=VALUE (`search.w_fts=0.8`,
    /// `index.chunk_target_max=800`), можно повторять
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub sets: Vec<String>,
    /// Метка в имени файла результата
    #[arg(long)]
    pub label: Option<String>,
    /// zam-server (по умолчанию собирается `cargo build --release -p zam-server`)
    #[arg(long)]
    pub server_bin: Option<PathBuf>,
    /// Каталог результатов (по умолчанию eval/results)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Каталог корпуса вместо eval/corpus (контрольные прогоны; проверка — как у фрагмента)
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,
    /// Файл insight вместо eval/insights.toml (несуществующий — без insight)
    #[arg(long, value_name = "FILE")]
    pub insights: Option<PathBuf>,
    /// Не убирать из PATH сервера каталоги CUDA Toolkit (по умолчанию убираются: библиотеки
    /// NVIDIA берутся только из <ORT>/cuda, как у пользователя)
    #[arg(long)]
    pub keep_cuda_path: bool,
    /// Не пересоздавать каталог прогона: сервер на уже загруженном и проиндексированном
    /// корпусе (target/eval/<прогон>), например с другими весами --set
    #[arg(long)]
    pub reuse: bool,
    /// Прежний результат (JSON eval/results/…): парная разность recall@5 и nDCG@10 по общим
    /// запросам (тот же id, вид и expect) — в отчёт прогона; можно повторять: прогон режима
    /// сравнивается с файлом того же режима (`info.mode`), один файл — со всеми режимами
    #[arg(long, value_name = "FILE")]
    pub compare: Vec<PathBuf>,
    /// Eval словаря M4.5 (eval/glossary.toml): запросы на псевдонимах и основной набор без
    /// словаря, со словарём и с весами канала `alias` 0 / 0,25 / 0,5 / 0,75
    #[arg(long, conflicts_with_all = ["grid", "check", "hints", "tokens", "quality_gate", "reuse"])]
    pub glossary: bool,
}

/// Каталоги и файлы набора.
#[derive(Clone)]
pub struct Paths {
    pub repo: PathBuf,
    pub corpus: PathBuf,
    pub queries: PathBuf,
    /// `eval/insights.toml` — insight набора M3.
    pub insights: PathBuf,
    /// `eval/hints.toml` — подсказки хуков (M4).
    pub hints: PathBuf,
    /// `eval/scenarios.toml` — сценарии расхода токенов (M4).
    pub scenarios: PathBuf,
    /// `eval/glossary.toml` — словарь и запросы на псевдонимах (M4.5).
    pub glossary: PathBuf,
    pub results: PathBuf,
    /// `target/eval` — каталоги данных прогонов.
    pub work: PathBuf,
}

impl Paths {
    pub fn new(repo: &Path, a: &EvalArgs) -> Paths {
        let eval = repo.join("eval");
        Paths {
            repo: repo.to_path_buf(),
            corpus: a.corpus.clone().unwrap_or_else(|| eval.join("corpus")),
            queries: a
                .queries
                .clone()
                .unwrap_or_else(|| eval.join("queries.toml")),
            insights: a
                .insights
                .clone()
                .unwrap_or_else(|| eval.join("insights.toml")),
            hints: eval.join("hints.toml"),
            scenarios: eval.join("scenarios.toml"),
            glossary: eval.join("glossary.toml"),
            results: a.out.clone().unwrap_or_else(|| eval.join("results")),
            work: repo.join("target").join("eval"),
        }
    }
}

/// Корпус и запросы после проверки (полный набор — со всеми требованиями).
pub fn load_set(p: &Paths, full: bool) -> Result<(Corpus, Vec<Query>, corpus::Summary)> {
    let mut corpus = Corpus::load(&p.corpus)?;
    corpus.insights = load_insights(&p.insights)?;
    let queries = load_queries(&p.queries)?;
    let s = validate(&corpus, &queries, full)?;
    Ok((corpus, queries, s))
}

fn print_summary(s: &corpus::Summary) {
    let kinds: Vec<String> = s.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
    println!(
        "корпус: {} заметок (ru {}, en {}; без запросов {}); запросов {}: {}; \
         перефразов без общих слов {} ({:.0} % запросов видов M2)",
        s.notes,
        s.notes_ru,
        s.notes_en,
        s.unreferenced,
        s.queries,
        kinds.join(", "),
        s.disjoint,
        s.disjoint_share() * 100.0
    );
    if s.insights > 0 || s.long_notes > 0 {
        println!(
            "набор M3: insight {} (цепочек замещения {}, отзывов {}, пар противоречий {}; \
             надёжное лексически ближе к запросу в {}, гипотеза — в {}), длинных заметок {} \
             (бесструктурных абзацев длиннее {} символов: {}), заметок с index: false {}, \
             запросов с also {}",
            s.insights,
            s.chains,
            s.retracted,
            s.contradictions,
            s.certainty_reliable_closer,
            s.certainty_hypothesis_closer,
            s.long_notes,
            corpus::LONG_PARAGRAPH_CHARS,
            s.long_paragraphs,
            s.fts_only_notes,
            s.with_also
        );
    }
}

/// Полный набор `eval/` (все требования проверки) — без своих корпуса, insight и запросов.
fn full_set(a: &EvalArgs) -> bool {
    a.queries.is_none() && a.corpus.is_none() && a.insights.is_none()
}

/// Точка входа `cargo xtask eval`.
pub fn main(a: EvalArgs, repo: &Path) -> Result<()> {
    let p = Paths::new(repo, &a);
    if a.check {
        // фрагмент (--queries) проверяется без требований к полному набору
        let (corpus, _, s) = load_set(&p, full_set(&a))?;
        print_summary(&s);
        if full_set(&a) {
            let (set, _) = hints::load_hints(&p.hints)?;
            let h = hints::validate_hints(&set, &corpus)?;
            let (sc, _) = tokens::load_scenarios(&p.scenarios)?;
            tokens::validate_scenarios(&sc, &corpus)?;
            println!(
                "набор M4: подсказок {} (релевантных {}, нерелевантных {}), сценариев расхода токенов {}",
                h.cases,
                h.relevant,
                h.irrelevant,
                sc.len()
            );
            let (g, _) = glossary::load_glossary(&p.glossary)?;
            glossary::validate_glossary(&g, &corpus, true)?;
            println!(
                "набор M4.5: терминов словаря {}, запросов на псевдонимах {}",
                g.term.len(),
                g.query.len()
            );
        }
        println!("EVAL-CHECK OK");
        return Ok(());
    }
    let (corpus, queries, s) = load_set(&p, full_set(&a))?;
    print_summary(&s);
    if a.quality_gate {
        return gate::run(&a, &p, &corpus, &queries);
    }
    if a.hints {
        return hints::run(&a, &p, &corpus, &queries);
    }
    if a.tokens {
        return tokens::run(&a, &p, &corpus);
    }
    if a.glossary {
        return glossary::run(&a, &p, &corpus, &queries);
    }
    if let Some(g) = &a.grid {
        return grid::run(&a, &p, &corpus, &queries, g);
    }
    if a.modes.is_empty() {
        bail!("укажите --mode");
    }
    run::run_modes(&a, &p, &corpus, &queries)
}
