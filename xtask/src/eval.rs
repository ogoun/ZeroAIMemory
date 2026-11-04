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
//! Eval M6 (задача 1a1): `--tokens --via mcp --split dev|holdout` — замер ТС-1 через `POST /mcp`
//! симулированным агентом §3.4 ([`tokens::mcp`]): возможности сервера по версии ([`caps`]),
//! нагеты и отпечатки с замком `eval/nuggets.lock` ([`nuggets`]), разбиение dev/holdout
//! ([`split`]), статистика §3.5 ([`metrics`]); `--grid` — точки `[[point]]` (только dev), `--arm`
//! с `--compare` — сравнение сохранённых плеч.
//!
//! Eval M4.5 (задача 15): `--glossary` — словарь RU↔EN `eval/glossary.toml`, канал `alias` и
//! его вес, основной набор со словарём и связями `contradicts` ([`glossary`]).

pub mod caps;
pub mod compare;
pub mod corpus;
pub mod docs;
pub mod gate;
pub mod glossary;
pub mod grid;
pub mod hints;
pub mod insights;
pub mod leaves;
pub mod mcp_client;
pub mod metrics;
pub mod nuggets;
pub mod report;
pub mod run;
pub mod sessions_corpus;
pub mod split;
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

/// Транспорт агента замера токенов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Mcp,
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
    /// Расход токенов агента по сценариям eval/scenarios.toml (протокол §9.3, кэш, chunks);
    /// с --via mcp — замер M6 через MCP (спецификация M6 §3)
    #[arg(long, conflicts_with = "check")]
    pub tokens: bool,
    /// Транспорт агента --tokens: mcp — ТС-1 M6 через POST /mcp (без флага — REST M4)
    #[arg(long, value_enum, requires = "tokens")]
    pub via: Option<Via>,
    /// Часть набора замера M6: dev (подбор, сетки) | holdout (приёмка; без --grid)
    #[arg(long, value_enum, requires = "via")]
    pub split: Option<split::Part>,
    /// Файл сценариев вместо eval/scenarios.toml (замок отпечатков — nuggets.lock рядом); с
    /// --check — проверка только набора ТС-1
    #[arg(long, value_name = "FILE")]
    pub scenarios: Option<PathBuf>,
    /// --via mcp без прогона: сохранённое плечо (JSON) сравнивается с каждым --compare
    #[arg(long, value_name = "FILE", requires = "via")]
    pub arm: Option<PathBuf>,
    /// Гейт без справочного прогона fp16 на CUDA (GPU занят): вердикт — по int8 и fp32 на CPU
    #[arg(long, requires = "quality_gate")]
    pub skip_cuda: bool,
    /// zam для хуков в --hints и сессий ТС-2 (по умолчанию собирается
    /// `cargo build --release -p zam-cli`); `--zam-bin` — то же
    #[arg(long, alias = "zam-bin")]
    pub cli_bin: Option<PathBuf>,
    /// Подбор весов [search] и порогов чанкинга по сетке из файла (eval/grid.toml); с --tokens
    /// --via mcp — точки `[[point]] name, env = { ZAM_… }` (только --split dev)
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
    /// Каталог результатов (по умолчанию eval/results); с --tokens --via mcp — каталог или
    /// префикс пути файлов `<префикс>[-<точка>].{json,md}`
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
    /// Префикс имени результата после даты (`--hints` и прогоны `--mode`):
    /// `<дата>-<префикс>-<модель|fts>-…`, например `m45` — итоги этапа под общим префиксом без
    /// ручного переименования (у `--glossary` имена свои)
    #[arg(long, value_name = "NAME", conflicts_with_all = ["glossary", "grid", "quality_gate", "tokens"])]
    pub prefix: Option<String>,
    /// Прежний результат (JSON eval/results/…): парная разность recall@5 и nDCG@10 по общим
    /// запросам (тот же id, вид и expect) — в отчёт прогона; можно повторять: прогон режима
    /// сравнивается с файлом того же режима (`info.mode`), один файл — со всеми режимами
    #[arg(long, value_name = "FILE")]
    pub compare: Vec<PathBuf>,
    /// Eval словаря M4.5 (eval/glossary.toml): запросы на псевдонимах и основной набор без
    /// словаря, со словарём и с весами канала `alias` 0 / 0,25 / 0,5 / 0,75
    #[arg(long, conflicts_with_all = ["grid", "check", "hints", "tokens", "quality_gate", "reuse"])]
    pub glossary: bool,
    /// Eval документов M5 (eval/docs/, eval/docs-queries.toml): A/B извлечения PDF, recall@5 по
    /// вложениям, точность `locator`, латентность поиска с вложениями и без, враждебные файлы,
    /// превью XLSX ([`docs`]); набор — `cargo xtask docs-corpus`
    #[arg(long, conflicts_with_all = ["grid", "hints", "tokens", "quality_gate", "glossary"])]
    pub docs: bool,
    /// Бэкенд PDF для --docs: pdfium (продукт) | pdf_oxide | pdf_oxide_plain (сборка xtask с
    /// --features pdf-oxide)
    #[arg(long, value_enum, default_value_t = docs::extract::PdfBackend::Pdfium, requires = "docs")]
    pub pdf_backend: docs::extract::PdfBackend,
    /// Каталог набора документов вместо eval/docs (--docs; у --tokens --via mcp — для
    /// отпечатка корпуса)
    #[arg(long, value_name = "DIR")]
    pub docs_dir: Option<PathBuf>,
    /// --docs: без заметок eval/corpus (и без замера латентности основного набора)
    #[arg(long, requires = "docs")]
    pub docs_only: bool,
    /// --docs: только A/B извлечения PDF, без сервера
    #[arg(long, requires = "docs")]
    pub ab_only: bool,
    /// --docs: также большие файлы large/ (docs-corpus --large): сроки разбора сервером
    #[arg(long, requires = "docs")]
    pub large: bool,
    /// --docs --compare: только запросы к файлам этого формата (pdf, docx, xlsx, pptx)
    #[arg(long, value_name = "FORMAT", requires = "docs")]
    pub docs_format: Option<String>,
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
    /// `eval/scenarios.toml` — сценарии расхода токенов (M4; ТС-1 M6 с нагетами).
    pub scenarios: PathBuf,
    /// `eval/docs` — набор документов (M5; входит в отпечаток корпуса ТС-1).
    pub docs: PathBuf,
    /// `eval/glossary.toml` — словарь и запросы на псевдонимах (M4.5).
    pub glossary: PathBuf,
    /// `eval/sessions-corpus` — корпус ТС-2 и идей (M6, задача 1c); ТС-1 его не грузит.
    pub sessions_corpus: PathBuf,
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
            scenarios: a
                .scenarios
                .clone()
                .unwrap_or_else(|| eval.join("scenarios.toml")),
            docs: a.docs_dir.clone().unwrap_or_else(|| eval.join("docs")),
            glossary: eval.join("glossary.toml"),
            sessions_corpus: eval.join(corpus::SESSIONS_CORPUS_DIR),
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
    if !s.areas.is_empty() || s.gotcha_contradicts > 0 {
        let areas: Vec<String> = s.areas.iter().map(|(a, n)| format!("{a} {n}")).collect();
        println!(
            "набор M6 (§10, §11): целей по областям: {}; пар hypothesis gotcha → theorem \
             (contradicts) {}, из них с ответом gotcha/failure {}",
            areas.join(", "),
            s.gotcha_contradicts,
            s.gotcha_answer_contradicts
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
    if a.docs {
        return docs::run(&a, &p);
    }
    if a.tokens && a.via == Some(Via::Mcp) {
        return tokens::mcp::main(&a, &p);
    }
    if a.tokens && a.grid.is_some() {
        bail!("--grid с --tokens — только у замера M6 (--via mcp)");
    }
    if a.check && a.scenarios.is_some() {
        // только набор ТС-1 (свой файл сценариев): нагеты, отпечатки, замок
        let set = tokens::mcp::load_set(&a, &p)?;
        tokens::mcp::print_set(&set, &p);
        println!("EVAL-CHECK OK");
        return Ok(());
    }
    if a.check {
        // фрагмент (--queries) проверяется без требований к полному набору
        let (corpus, _, s) = load_set(&p, full_set(&a))?;
        print_summary(&s);
        if full_set(&a) {
            let (set, _) = hints::load_hints(&p.hints)?;
            let h = hints::validate_hints(&set, &corpus)?;
            let set = tokens::mcp::load_set(&a, &p)?;
            let sc = &set.scenarios;
            println!(
                "набор M4: подсказок {} (релевантных {}, нерелевантных {}), сценариев расхода токенов {}",
                h.cases,
                h.relevant,
                h.irrelevant,
                sc.len()
            );
            tokens::mcp::print_set(&set, &p);
            let (g, _) = glossary::load_glossary(&p.glossary)?;
            glossary::validate_glossary(&g, &corpus, true)?;
            println!(
                "набор M4.5: терминов словаря {}, запросов на псевдонимах {}",
                g.term.len(),
                g.query.len()
            );
            let sc = sessions_corpus::load(&p.sessions_corpus)?;
            let ss = sessions_corpus::validate(&sc)?;
            let states: Vec<String> = ss
                .ideas_by_state
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect();
            println!(
                "корпус ТС-2 M6: Rules {}, задач {} (in_progress/blocked {}), вопросов {} \
                 (отвечено {}), запросов доступа {}, терминов {}, insight {}; идей {} ({}; с \
                 общим глаголом в заголовке {}, со словом «на стоп-основу» {})",
                ss.rules,
                ss.tasks,
                ss.active_tasks,
                ss.questions,
                ss.answered,
                ss.access,
                ss.terms,
                ss.insights,
                ss.ideas,
                states.join(", "),
                ss.stop_verb_ideas,
                ss.false_friend_ideas
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
