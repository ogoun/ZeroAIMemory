//! `cargo xtask docs-corpus` — тестовый набор документов `eval/docs/` (спецификация M5 §6).
//!
//! Свои синтетические документы, детерминированно из сида (SplitMix64, поток на документ):
//! одинаковый сид — одинаковые байты на любой платформе (тест `docs_corpus_deterministic`).
//! Бинарники в git не кладутся: `eval/docs/` под `.gitignore`, в git — генератор и запросы
//! (`eval/docs-queries.toml`).
//!
//! Состав (`files/`):
//! - DOCX — регламент (ru) и памятка (en): заголовки H1–H3, абзацы, таблица, сноска;
//! - XLSX — книга на 3 листа × 10 тыс. строк: общие строки, даты со стилем (свой `numFmt`
//!   `dd.mm.yyyy` и встроенный 14) — серийные числа в `<v>`;
//! - PPTX — 20 слайдов с заголовками, пунктами и заметками докладчика;
//! - PDF — одноколоночный (с таблицей), двухколоночный (поток содержимого чередует колонки,
//!   страница-таблица), кириллица и CJK (шрифты Type3 с `ToUnicode`, [`pdf`]), 500 страниц с
//!   колонтитулами, «скан» без текстового слоя;
//! - `hostile/` — zip-бомба, XXE и «миллиард смешков», глубокая вложенность XML, битый и
//!   обрезанный PDF, PDF с JavaScript, глубокая вложенность объектов PDF, поток глифов сверх
//!   предела страницы, картинка 30000 × 30000 (OOM миниатюры);
//! - `large/` (`--large`) — DOCX на ≈ 21 млн символов и XLSX на 300 тыс. строк (сроки разбора).
//!
//! `manifest.json` — файлы (sha256, роль, ожидание), факты (`id` → файл, `locator`, дословное
//! `evidence`) и эталон порядка строк страниц PDF (A/B порядка чтения, [`OrderPage`]).

mod hostile;
mod ooxml;
pub mod pdf;
mod text;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ooxml::{Block, Cell, DateFmt, Sheet, Slide, excel_serial};
use pdf::{Line, PAGE_H, PAGE_W, Page, text_width};
use text::{Lang, Rng, paragraph, sentence, wrap};

/// Сид по умолчанию.
pub const DEFAULT_SEED: u64 = 0x00d0_c5e7_2026_1102;
/// Версия набора: смена состава или правил генерации (2 — допустимые исходы `accept`).
pub const CORPUS_VERSION: u32 = 2;

/// Аргументы `cargo xtask docs-corpus`.
#[derive(Debug, clap::Args)]
pub struct DocsCorpusArgs {
    /// Сид генератора
    #[arg(long, default_value_t = DEFAULT_SEED)]
    pub seed: u64,
    /// Каталог набора (по умолчанию eval/docs)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Также большие файлы large/ (DOCX ≈ 21 млн символов, XLSX 300 тыс. строк; ≈ 1 мин)
    #[arg(long)]
    pub large: bool,
    /// Уменьшенный набор (тесты): те же документы, меньше строк и страниц
    #[arg(long, hide = true)]
    pub small: bool,
}

/// Масштаб набора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub small: bool,
    pub large: bool,
}

impl Profile {
    fn rows(self) -> usize {
        if self.small { 300 } else { 10_000 }
    }
    fn book_pages(self) -> u32 {
        if self.small { 20 } else { 500 }
    }
}

/// Файл набора.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Путь от каталога набора (`files/…`, `hostile/…`, `large/…`).
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    /// `content` | `scan` | `hostile` | `large`.
    pub role: String,
    /// Что ожидается от извлечения (для отчёта).
    pub expect: String,
    /// Допустимые исходы на сервере (сверка eval): `состояние`, `состояние:<начало причины>`,
    /// `состояние+<предупреждение>`, `done+ocr` — с `ocr_needed`; любой из списка — успех.
    #[serde(default)]
    pub accept: Vec<String>,
}

/// Факт: дословный текст в известном месте документа.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fact {
    pub id: String,
    pub file: String,
    /// Ожидаемый `locator` (`page:N`, `slide:N`, `sheet:<лист>!<ячейка>`, `para:N`).
    pub locator: String,
    /// Дословная подстрока текста документа.
    pub evidence: String,
}

/// Эталон порядка строк страницы PDF.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderPage {
    pub file: String,
    pub page: u32,
    /// `one-column` | `two-column` | `table` | `cjk`.
    pub layout: String,
    /// Строки в порядке чтения (у таблицы — заголовок и ячейки построчно); колонтитулы не
    /// входят.
    pub lines: Vec<String>,
    /// Таблица: столбцов в строке (ячейки `lines[1..]` по `cols`); иначе 0.
    #[serde(default)]
    pub cols: usize,
}

/// `manifest.json` набора.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: u32,
    pub seed: u64,
    pub small: bool,
    pub files: Vec<FileEntry>,
    pub facts: Vec<Fact>,
    pub order: Vec<OrderPage>,
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest> {
        let p = dir.join("manifest.json");
        let t = fs::read_to_string(&p)
            .with_context(|| format!("{} (cargo xtask docs-corpus)", p.display()))?;
        serde_json::from_str(&t).with_context(|| p.display().to_string())
    }

    pub fn fact(&self, id: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.id == id)
    }
}

/// Набор в памяти до записи.
#[derive(Default)]
struct Out {
    files: Vec<(FileEntry, Vec<u8>)>,
    facts: Vec<Fact>,
    order: Vec<OrderPage>,
}

impl Out {
    fn add(&mut self, path: &str, role: &str, expect: &str, data: Vec<u8>) {
        let sha = hex(&Sha256::digest(&data));
        self.files.push((
            FileEntry {
                path: path.to_string(),
                sha256: sha,
                bytes: data.len() as u64,
                role: role.to_string(),
                expect: expect.to_string(),
                accept: match role {
                    "scan" => vec!["done+ocr".to_string()],
                    "large" => vec!["done+truncated: max_chars".to_string()],
                    _ => vec!["done".to_string()],
                },
            },
            data,
        ));
    }

    /// Допустимые исходы файла `path` (враждебные файлы).
    fn set_accept(&mut self, path: &str, alts: &[&str]) {
        let e = self
            .files
            .iter_mut()
            .find(|f| f.0.path == path)
            .unwrap_or_else(|| panic!("нет файла {path}"));
        e.0.accept = alts.iter().map(|s| s.to_string()).collect();
    }

    fn fact(&mut self, id: &str, file: &str, locator: String, evidence: &str) {
        self.facts.push(Fact {
            id: id.to_string(),
            file: file.to_string(),
            locator,
            evidence: evidence.to_string(),
        });
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Точка входа `cargo xtask docs-corpus`.
pub fn main(a: DocsCorpusArgs, repo: &Path) -> Result<()> {
    let dir = a.out.unwrap_or_else(|| repo.join("eval").join("docs"));
    let profile = Profile {
        small: a.small,
        large: a.large,
    };
    let t0 = std::time::Instant::now();
    let m = write(&dir, a.seed, profile)?;
    let total: u64 = m.files.iter().map(|f| f.bytes).sum();
    println!(
        "набор документов: {} файлов ({:.1} МБ), фактов {}, страниц эталона порядка {} → {} ({:.1} с)",
        m.files.len(),
        total as f64 / 1e6,
        m.facts.len(),
        m.order.len(),
        dir.display(),
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Генерирует набор и пишет его в `dir` (прежние `files/`, `hostile/`, `large/` удаляются).
pub fn write(dir: &Path, seed: u64, profile: Profile) -> Result<Manifest> {
    let out = generate(seed, profile);
    for sub in ["files", "hostile", "large"] {
        let d = dir.join(sub);
        if d.exists() {
            fs::remove_dir_all(&d).with_context(|| d.display().to_string())?;
        }
    }
    let mut files = Vec::new();
    for (e, data) in out.files {
        let p = dir.join(&e.path);
        fs::create_dir_all(p.parent().context("каталог файла")?)?;
        fs::write(&p, &data).with_context(|| p.display().to_string())?;
        files.push(e);
    }
    let m = Manifest {
        version: CORPUS_VERSION,
        seed,
        small: profile.small,
        files,
        facts: out.facts,
        order: out.order,
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&m)? + "\n",
    )?;
    Ok(m)
}

/// Манифест набора без записи на диск (тесты).
pub fn manifest_in_memory(seed: u64, profile: Profile) -> Manifest {
    let o = generate(seed, profile);
    Manifest {
        version: CORPUS_VERSION,
        seed,
        small: profile.small,
        files: o.files.into_iter().map(|f| f.0).collect(),
        facts: o.facts,
        order: o.order,
    }
}

/// Файлы набора в памяти: (путь, байты) (тесты).
#[cfg(test)]
pub fn generate_files(seed: u64, profile: Profile) -> Vec<(String, Vec<u8>)> {
    generate(seed, profile)
        .files
        .into_iter()
        .map(|(e, d)| (e.path, d))
        .collect()
}

/// Весь набор в памяти.
fn generate(seed: u64, p: Profile) -> Out {
    let mut o = Out::default();
    docx_ru(&mut o, seed);
    docx_en(&mut o, seed);
    xlsx_inventory(&mut o, seed, p);
    pptx_roadmap(&mut o, seed);
    pdf_onecol(&mut o, seed);
    pdf_twocol(&mut o, seed);
    pdf_cyrillic(&mut o, seed);
    pdf_cjk(&mut o, seed);
    pdf_book(&mut o, seed, p);
    pdf_scan(&mut o);
    hostile::all(&mut o, seed, p);
    if p.large {
        large(&mut o, seed);
    }
    o
}

// ------------------------------------------------------------------ DOCX

/// Абзац «наполнитель — факт — наполнитель».
fn around(r: &mut Rng, lang: Lang, fact: &str) -> String {
    let sep = if matches!(lang, Lang::Zh | Lang::Ja) {
        ""
    } else {
        " "
    };
    format!("{}{sep}{fact}{sep}{}", sentence(r, lang), sentence(r, lang))
}

const D1: &str = "Подшипники главного насоса заменяют каждые 4200 моточасов, а при вибрации выше 7,1 мм/с — немедленно.";
const D2: &str = "промывка обратным током каждые 14 суток";
const D3: &str = "Методика замера вибрации согласована с заводом-изготовителем в марте 2024 года.";
const D4: &str = "Резервный дизель-генератор проверяют под нагрузкой по первым понедельникам месяца не менее 40 минут.";
const D5: &str =
    "Аварийный сброс давления выполняется задвижкой З-17 у восточной стены машинного зала.";

fn docx_ru(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "regulations-ru.docx");
    let l = Lang::Ru;
    let mut b: Vec<Block> = Vec::new();
    let mut fact_at: Vec<(&str, usize, &str)> = Vec::new();
    let filler = |r: &mut Rng, b: &mut Vec<Block>, n: usize| {
        for _ in 0..n {
            b.push(Block::Para(paragraph(r, l, 3, 6)));
        }
    };
    b.push(Block::Heading(
        1,
        "Регламент эксплуатации насосной станции № 3".into(),
    ));
    b.push(Block::Heading(2, "1. Общие положения".into()));
    filler(&mut r, &mut b, 5);
    b.push(Block::Heading(2, "2. Техническое обслуживание".into()));
    b.push(Block::Heading(3, "2.1. Насосные агрегаты".into()));
    filler(&mut r, &mut b, 3);
    fact_at.push(("d1", b.len(), D1));
    b.push(Block::Para(around(&mut r, l, D1)));
    filler(&mut r, &mut b, 3);
    b.push(Block::Heading(3, "2.2. Фильтры и сетки".into()));
    filler(&mut r, &mut b, 2);
    let mut rows = vec![vec![
        "Узел".to_string(),
        "Операция".to_string(),
        "Ответственный".to_string(),
    ]];
    for (i, unit) in [
        "Сетка водозабора",
        "Обратный клапан",
        "Фильтр грубой очистки",
        "Маслоохладитель",
        "Дренажный насос",
    ]
    .iter()
    .enumerate()
    {
        let op = if i == 2 {
            D2.to_string()
        } else {
            format!("осмотр раз в {} суток", r.range(2, 9) * 7)
        };
        rows.push(vec![
            unit.to_string(),
            op,
            r.pick(&["мастер смены", "слесарь КИПиА", "инженер-механик"])
                .to_string(),
        ]);
    }
    let table_ix = b.len();
    b.push(Block::Table(rows));
    filler(&mut r, &mut b, 2);
    b.push(Block::Heading(3, "2.3. Вибродиагностика".into()));
    filler(&mut r, &mut b, 2);
    let note_ix = b.len();
    b.push(Block::ParaNote(paragraph(&mut r, l, 3, 4), D3.into()));
    filler(&mut r, &mut b, 3);
    b.push(Block::Heading(2, "3. Резервное питание".into()));
    filler(&mut r, &mut b, 2);
    fact_at.push(("d4", b.len(), D4));
    b.push(Block::Para(around(&mut r, l, D4)));
    filler(&mut r, &mut b, 4);
    b.push(Block::Heading(2, "4. Аварийные режимы".into()));
    b.push(Block::Heading(3, "4.1. Сброс давления".into()));
    filler(&mut r, &mut b, 5);
    fact_at.push(("d5", b.len(), D5));
    b.push(Block::Para(around(&mut r, l, D5)));
    filler(&mut r, &mut b, 2);
    b.push(Block::Heading(2, "5. Документация".into()));
    filler(&mut r, &mut b, 8);
    let (data, first) = ooxml::docx(&b);
    let file = "files/regulations-ru.docx";
    for (id, ix, ev) in fact_at {
        o.fact(id, file, format!("para:{}", first[ix]), ev);
    }
    // ячейка D2 — третья строка таблицы (после шапки и двух строк), второй столбец
    o.fact(
        "d2",
        file,
        format!("para:{}", first[table_ix] + 3 * 3 + 1),
        D2,
    );
    o.fact("d3", file, format!("para:{}", first[note_ix]), D3);
    o.add(file, "content", "done", data);
}

const E1: &str = "New engineers receive badge access to the server room only after completing the fire suppression briefing.";
const E2: &str = "hardware VPN token issued by the IT desk, expires after 90 days";
const E3: &str =
    "The mentoring programme was piloted in the Lisbon office before the company-wide rollout.";
const E4: &str =
    "Expense reports above 500 euros require a second approval from the department head.";

fn docx_en(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "onboarding-en.docx");
    let l = Lang::En;
    let mut b: Vec<Block> = Vec::new();
    let filler = |r: &mut Rng, b: &mut Vec<Block>, n: usize| {
        for _ in 0..n {
            b.push(Block::Para(paragraph(r, l, 3, 6)));
        }
    };
    b.push(Block::Heading(1, "Engineering onboarding handbook".into()));
    b.push(Block::Heading(2, "Your first week".into()));
    filler(&mut r, &mut b, 4);
    let e1 = b.len();
    b.push(Block::Para(around(&mut r, l, E1)));
    filler(&mut r, &mut b, 3);
    b.push(Block::Heading(2, "Accounts and equipment".into()));
    filler(&mut r, &mut b, 2);
    let mut rows = vec![vec!["Item".to_string(), "Details".to_string()]];
    for (i, item) in ["Laptop", "VPN token", "Office key", "Parking card"]
        .iter()
        .enumerate()
    {
        let d = if i == 1 {
            E2.to_string()
        } else {
            format!(
                "issued by the {} on day {}",
                r.pick(&["front desk", "facility team", "IT desk"]),
                r.range(1, 5)
            )
        };
        rows.push(vec![item.to_string(), d]);
    }
    let table = b.len();
    b.push(Block::Table(rows));
    filler(&mut r, &mut b, 3);
    b.push(Block::Heading(2, "Mentoring".into()));
    filler(&mut r, &mut b, 2);
    let note = b.len();
    b.push(Block::ParaNote(paragraph(&mut r, l, 3, 4), E3.into()));
    filler(&mut r, &mut b, 3);
    b.push(Block::Heading(2, "Travel and expenses".into()));
    b.push(Block::Heading(3, "Approvals".into()));
    filler(&mut r, &mut b, 3);
    let e4 = b.len();
    b.push(Block::Para(around(&mut r, l, E4)));
    filler(&mut r, &mut b, 5);
    let (data, first) = ooxml::docx(&b);
    let file = "files/onboarding-en.docx";
    o.fact("e1", file, format!("para:{}", first[e1]), E1);
    // строка «VPN token» — третья (шапка, Laptop, VPN token), столбец Details
    o.fact("e2", file, format!("para:{}", first[table] + 2 * 2 + 1), E2);
    o.fact("e3", file, format!("para:{}", first[note]), E3);
    o.fact("e4", file, format!("para:{}", first[e4]), E4);
    o.add(file, "content", "done", data);
}

// ------------------------------------------------------------------ XLSX

const X1: &str = "партия возвращена поставщику из-за протечки упаковки антифриза";
const X2: &str = "стеллаж С-12 перегружен, требуется усиление несущей балки";
const X3: &str = "forklift certification overdue for the night shift operator";

const SUPPLIERS: &[&str] = &[
    "ООО Гранит-Сервис",
    "АО Северснаб",
    "ООО Техноресурс",
    "ИП Ковалёв",
    "ООО Балтснаб",
    "АО Уралкомплект",
    "ООО Вектор-М",
];
const ITEMS: &[&str] = &[
    "антифриз 20 л",
    "фильтр масляный",
    "ремень приводной",
    "перчатки нитриловые",
    "болт М12",
    "кабель ВВГ 3×2,5",
    "смазка литиевая",
    "лампа светодиодная",
];
const COMMENTS: &[&str] = &[
    "без замечаний",
    "доставлено частично",
    "ожидает приёмки",
    "счёт оплачен",
    "",
    "",
];

/// Дата из диапазона 2024-01-01..2025-12-28 (серийный номер).
fn rand_date(r: &mut Rng) -> f64 {
    excel_serial(2024 + r.range(0, 1) as i32, r.range(1, 12), r.range(1, 28))
}

fn xlsx_inventory(o: &mut Out, seed: u64, p: Profile) {
    let mut r = Rng::fork(seed, "inventory.xlsx");
    let n = p.rows();
    let s = |x: &str| Cell::Str(x.to_string());
    // строки фактов (номер строки листа, шапка — строка 1); в малом наборе — пропорционально
    let at = |full: usize| if p.small { 2 + full * n / 10_000 } else { full };
    let (r1, r4) = (at(1234), at(9001));
    let mut supplies = vec![vec![
        s("Дата"),
        s("Номер"),
        s("Поставщик"),
        s("Товар"),
        s("Количество"),
        s("Цена, руб."),
        s("Комментарий"),
    ]];
    for i in 0..n {
        let row = i + 2;
        let (date, comment, supplier) = if row == r1 {
            (
                rand_date(&mut r),
                X1.to_string(),
                r.pick(SUPPLIERS).to_string(),
            )
        } else if row == r4 {
            (
                excel_serial(2021, 3, 15),
                "поставка по рамочному договору".to_string(),
                "ООО Гранит-Сервис".to_string(),
            )
        } else {
            (
                rand_date(&mut r),
                r.pick(COMMENTS).to_string(),
                r.pick(SUPPLIERS).to_string(),
            )
        };
        supplies.push(vec![
            Cell::Date(date, DateFmt::Custom),
            Cell::Str(format!("П-{:05}", 10_000 + i)),
            Cell::Str(supplier),
            s(r.pick(ITEMS)),
            Cell::Num(f64::from(r.range(1, 400))),
            Cell::Num(f64::from(r.range(50, 90_000)) / 10.0),
            Cell::Str(comment),
        ]);
    }
    let (r2, r5) = (at(7777), at(4321));
    let mut stock = vec![vec![
        s("Дата инвентаризации"),
        s("Стеллаж"),
        s("Ячейка"),
        s("Артикул"),
        s("Остаток"),
        s("Ответственный"),
        s("Примечание"),
    ]];
    let mut r5_article = String::new();
    for i in 0..n {
        let row = i + 2;
        let date = if row == r5 {
            excel_serial(2020, 7, 9)
        } else {
            rand_date(&mut r)
        };
        let article = format!("А{:06}", r.range(1, 999_999));
        if row == r5 {
            r5_article = article.clone();
        }
        let note = if row == r2 {
            X2.to_string()
        } else {
            r.pick(&["норма", "норма", "пересчитано", "требует сверки", ""])
                .to_string()
        };
        stock.push(vec![
            Cell::Date(date, DateFmt::Builtin14),
            Cell::Str(format!("С-{}", r.range(1, 40))),
            Cell::Str(format!("{}-{}", r.range(1, 12), r.range(1, 8))),
            Cell::Str(article),
            Cell::Num(f64::from(r.range(0, 500))),
            s(r.pick(&["Петров", "Сидорова", "Ким", "Абрамов", "Лебедева"])),
            Cell::Str(note),
        ]);
    }
    let (r3, r6) = (at(5021), at(2500));
    let mut staff = vec![vec![
        s("Hire date"),
        s("Employee ID"),
        s("Name"),
        s("Department"),
        s("Shift"),
        s("Certification"),
        s("Remark"),
    ]];
    let first = [
        "Alex", "Maria", "John", "Olga", "Wei", "Priya", "Tom", "Sara",
    ];
    let last = [
        "Smith", "Novak", "Garcia", "Chen", "Ivanova", "Brown", "Kumar",
    ];
    for i in 0..n {
        let row = i + 2;
        let date = if row == r6 {
            excel_serial(2019, 2, 28)
        } else {
            excel_serial(2020 + r.range(0, 5) as i32, r.range(1, 12), r.range(1, 28))
        };
        let remark = if row == r3 {
            X3.to_string()
        } else {
            r.pick(&["", "", "on leave", "part time", "relocated"])
                .to_string()
        };
        staff.push(vec![
            Cell::Date(date, DateFmt::Custom),
            Cell::Str(format!("E-{:05}", 20_000 + i)),
            Cell::Str(format!("{} {}", r.pick(&first), r.pick(&last))),
            s(r.pick(&["Warehouse", "Logistics", "Maintenance", "Quality", "Office"])),
            s(r.pick(&["day", "evening", "night"])),
            s(r.pick(&["forklift", "first aid", "crane", "none"])),
            Cell::Str(remark),
        ]);
    }
    let data = ooxml::xlsx(&[
        Sheet {
            name: "Поставки".into(),
            rows: supplies,
        },
        Sheet {
            name: "Склад".into(),
            rows: stock,
        },
        Sheet {
            name: "Staff".into(),
            rows: staff,
        },
    ]);
    let file = "files/inventory.xlsx";
    o.fact("x1", file, format!("sheet:Поставки!G{r1}"), X1);
    o.fact("x2", file, format!("sheet:Склад!G{r2}"), X2);
    o.fact("x3", file, format!("sheet:Staff!G{r3}"), X3);
    // даты: evidence — номер строки соседней ячейки (дата в выдаче сейчас — серийное число)
    o.fact(
        "x4",
        file,
        format!("sheet:Поставки!A{r4}"),
        &format!("П-{:05}", 10_000 + r4 - 2),
    );
    o.fact("x5", file, format!("sheet:Склад!A{r5}"), &r5_article);
    o.fact(
        "x6",
        file,
        format!("sheet:Staff!A{r6}"),
        &format!("E-{:05}", 20_000 + r6 - 2),
    );
    o.add(file, "content", "done", data);
}

// ------------------------------------------------------------------ PPTX

const P1: &str = "Migration of billing to the new ledger is scheduled for the third quarter.";
const P2: &str =
    "Пилот голосового помощника в колл-центре сократил среднее время ожидания на 18 %.";
const P3: &str =
    "Data retention for call recordings is reduced from five years to eighteen months.";
const P4: &str = "Бюджет пилота согласован с финансовой дирекцией до конца октября.";

fn pptx_roadmap(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "roadmap.pptx");
    let titles = [
        "Product roadmap 2026",
        "Agenda",
        "Where we are",
        "Billing platform",
        "Customer portal",
        "Mobile app",
        "Support operations",
        "Quality metrics",
        "Голосовой помощник: план",
        "Hiring plan",
        "Колл-центр: итоги пилота",
        "Infrastructure",
        "Security",
        "Partners",
        "Risks",
        "Budget",
        "Compliance",
        "Timeline",
        "Open questions",
        "Next steps",
    ];
    let mut slides = Vec::new();
    for (i, t) in titles.iter().enumerate() {
        let n = i + 1;
        let lang = if t.chars().any(|c| ('а'..='я').contains(&c)) {
            Lang::Ru
        } else {
            Lang::En
        };
        let mut bullets: Vec<String> = (0..r.range(2, 4)).map(|_| sentence(&mut r, lang)).collect();
        match n {
            4 => bullets.insert(1, P1.into()),
            11 => bullets.insert(0, P2.into()),
            17 => bullets.push(P3.into()),
            _ => {}
        }
        let notes = match n {
            9 => Some(format!("{} {P4}", sentence(&mut r, Lang::Ru))),
            _ if r.below(3) == 0 => Some(sentence(&mut r, lang)),
            _ => None,
        };
        slides.push(Slide {
            title: t.to_string(),
            bullets,
            notes,
        });
    }
    let file = "files/roadmap.pptx";
    o.fact("p1", file, "slide:4".into(), P1);
    o.fact("p2", file, "slide:11".into(), P2);
    o.fact("p3", file, "slide:17".into(), P3);
    o.fact("p4", file, "slide:9".into(), P4);
    o.add(file, "content", "done", ooxml::pptx(&slides));
}

// ------------------------------------------------------------------ PDF

/// Верх текста страницы (ниже верхних 8 % высоты — там колонтитулы, их убирает `zam-docs`) и
/// нижнее поле.
const TOP: f32 = 750.0;
const BOTTOM: f32 = 60.0;

/// Колонка: левый край, ширина, кегль, интервал.
#[derive(Clone, Copy)]
struct Col {
    x: f32,
    w: f32,
    size: f32,
    lead: f32,
}

/// Абзацы страницы в колонку: строки (x, y, текст) от `top`; не влезло — остаток абзацев.
/// Абзац целиком: не помещается — переносится на следующую страницу.
fn flow(col: Col, top: f32, paras: &mut Vec<String>) -> (Vec<Line>, f32) {
    let mut y = top;
    let mut out = Vec::new();
    while let Some(p) = paras.first() {
        let lines = wrap(p, col.w, |s| text_width(s, col.size));
        let need = lines.len() as f32 * col.lead;
        if y - need < BOTTOM {
            break;
        }
        for t in lines {
            out.push(Line {
                x: col.x,
                y,
                size: col.size,
                text: t,
            });
            y -= col.lead;
        }
        y -= col.lead * 0.6;
        paras.remove(0);
    }
    (out, y)
}

/// Страницы одной колонки: на страницу `fact_page` абзац факта ставится вторым.
struct FlowDoc<'a> {
    file: &'a str,
    lang: Lang,
    col: Col,
    title: &'a str,
}

/// Страница одной колонки с заголовком; `fact` — абзац факта вторым на странице.
fn onecol_page(r: &mut Rng, d: &FlowDoc<'_>, no: u32, fact: Option<&str>) -> (Page, Vec<String>) {
    let mut paras: Vec<String> = (0..12).map(|_| paragraph(r, d.lang, 3, 5)).collect();
    if let Some(f) = fact {
        paras.insert(1, around(r, d.lang, f));
    }
    let head = Line {
        x: d.col.x,
        y: TOP,
        size: 14.0,
        text: format!("{} {no}", d.title),
    };
    let (lines, _) = flow(d.col, TOP - 26.0, &mut paras);
    let mut gt = vec![head.text.clone()];
    gt.extend(lines.iter().map(|l| l.text.clone()));
    let mut all = vec![head];
    all.extend(lines);
    (
        Page {
            lines: all,
            ..Page::default()
        },
        gt,
    )
}

/// Страница-таблица: шапка и строки ячеек (поток — построчно), эталон — ячейки построчно.
fn table_page(title: &str, rows: &[Vec<String>], xs: &[f32]) -> (Page, Vec<String>) {
    let mut lines = vec![Line {
        x: xs[0],
        y: TOP,
        size: 14.0,
        text: title.to_string(),
    }];
    let mut gt = vec![title.to_string()];
    let mut y = TOP - 30.0;
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            lines.push(Line {
                x: xs[c],
                y,
                size: 10.0,
                text: cell.clone(),
            });
            gt.push(cell.clone());
        }
        y -= 16.0;
    }
    (
        Page {
            lines,
            ..Page::default()
        },
        gt,
    )
}

fn order(o: &mut Out, file: &str, page: u32, layout: &str, lines: Vec<String>) {
    order_cols(o, file, page, layout, lines, 0);
}

fn order_cols(o: &mut Out, file: &str, page: u32, layout: &str, lines: Vec<String>, cols: usize) {
    o.order.push(OrderPage {
        file: file.to_string(),
        page,
        layout: layout.to_string(),
        lines,
        cols,
    });
}

const O1: &str =
    "The cooling tower fan blades were rebalanced after resonance was detected at 1450 rpm.";
const O2: &str =
    "Groundwater sampling wells must be purged of three casing volumes before collection.";
const O3: &str =
    "Contractors must sign the confined space permit before entering the valve chamber.";
const O4: &str = "seal replaced after leak";

fn pdf_onecol(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "onecol-en.pdf");
    let file = "files/onecol-en.pdf";
    let d = FlowDoc {
        file,
        lang: Lang::En,
        col: Col {
            x: 72.0,
            w: 451.0,
            size: 11.0,
            lead: 14.0,
        },
        title: "Site services report, section",
    };
    let mut pages = Vec::new();
    for no in 1..=10u32 {
        if no == 6 {
            let mut rows = vec![vec![
                "Unit".to_string(),
                "Pressure".to_string(),
                "Speed".to_string(),
                "Status".to_string(),
            ]];
            for k in 0..12 {
                let status = if k == 6 {
                    O4.to_string()
                } else {
                    r.pick(&["in service", "standby", "scheduled check", "monitoring"])
                        .to_string()
                };
                rows.push(vec![
                    format!("Pump P-{}", k + 1),
                    format!("{}.{} bar", r.range(1, 6), r.range(0, 9)),
                    format!("{} rpm", r.range(9, 30) * 100),
                    status,
                ]);
            }
            let (p, gt) = table_page("Table 6. Pump status", &rows, &[72.0, 200.0, 300.0, 400.0]);
            order_cols(o, file, no, "table", gt, 4);
            pages.push(p);
            continue;
        }
        let fact = match no {
            3 => Some(O1),
            7 => Some(O2),
            9 => Some(O3),
            _ => None,
        };
        let (p, gt) = onecol_page(&mut r, &d, no, fact);
        order(o, file, no, "one-column", gt);
        pages.push(p);
    }
    let _ = d.file;
    o.fact("o1", file, "page:3".into(), O1);
    o.fact("o2", file, "page:7".into(), O2);
    o.fact("o3", file, "page:9".into(), O3);
    o.fact("o4", file, "page:6".into(), O4);
    o.add(
        file,
        "content",
        "done",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

const T1: &str = "Lithium iron phosphate cells retained 92 percent of capacity after 3000 cycles.";
const T2: &str = "Thermal runaway propagation was stopped by mica sheets between the modules.";
const T3: &str = "19.2 kWh";
const T4: &str = "The pilot site in Tromso reported no heating faults during the polar night.";

/// Двухколоночная страница: поток чередует строки левой и правой колонок (ловушка для порядка
/// потока), эталон — сначала левая колонка, затем правая.
fn twocol_page(r: &mut Rng, title: Option<&str>, facts: [Option<&str>; 2]) -> (Page, Vec<String>) {
    let left = Col {
        x: 50.0,
        w: 235.0,
        size: 10.0,
        lead: 12.5,
    };
    let right = Col { x: 310.0, ..left };
    let mut lines = Vec::new();
    let mut gt = Vec::new();
    let mut top = TOP;
    if let Some(t) = title {
        lines.push(Line {
            x: 50.0,
            y: top,
            size: 16.0,
            text: t.to_string(),
        });
        gt.push(t.to_string());
        top -= 26.0;
        let mut abs = vec![paragraph(r, Lang::En, 3, 3)];
        let full = Col {
            x: 50.0,
            w: 495.0,
            size: 10.0,
            lead: 12.5,
        };
        let (a, y) = flow(full, top, &mut abs);
        gt.extend(a.iter().map(|l| l.text.clone()));
        lines.extend(a);
        top = y - 10.0;
    }
    let mut cols = Vec::new();
    for (c, fact) in [left, right].into_iter().zip(facts) {
        let mut paras: Vec<String> = (0..10).map(|_| paragraph(r, Lang::En, 2, 4)).collect();
        if let Some(f) = fact {
            paras.insert(1, around(r, Lang::En, f));
        }
        let (l, _) = flow(c, top, &mut paras);
        cols.push(l);
    }
    let (l, rr) = (&cols[0], &cols[1]);
    gt.extend(l.iter().map(|x| x.text.clone()));
    gt.extend(rr.iter().map(|x| x.text.clone()));
    for i in 0..l.len().max(rr.len()) {
        lines.extend(l.get(i).cloned());
        lines.extend(rr.get(i).cloned());
    }
    (
        Page {
            lines,
            ..Page::default()
        },
        gt,
    )
}

fn pdf_twocol(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "twocol-en.pdf");
    let file = "files/twocol-en.pdf";
    let mut pages = Vec::new();
    for no in 1..=6u32 {
        if no == 5 {
            let mut rows = vec![vec![
                "Module".to_string(),
                "Voltage".to_string(),
                "Energy".to_string(),
                "Cooling".to_string(),
                "Site".to_string(),
            ]];
            for (k, m) in ["A", "B", "C", "D", "E", "F", "G", "H", "J", "K"]
                .iter()
                .enumerate()
            {
                let energy = if k == 2 {
                    T3.to_string()
                } else {
                    format!("{}.{} kWh", r.range(5, 40), r.range(0, 9))
                };
                let cooling = if k == 2 {
                    "passive air cooling".to_string()
                } else {
                    r.pick(&["liquid loop", "forced air", "heat pipes"])
                        .to_string()
                };
                rows.push(vec![
                    format!("Module {m}"),
                    format!("{} V", r.pick(&[12, 24, 48, 400])),
                    energy,
                    cooling,
                    r.pick(&["Oslo", "Bergen", "Tromso", "Narvik"]).to_string(),
                ]);
            }
            let (p, gt) = table_page(
                "Table 2. Battery modules",
                &rows,
                &[50.0, 150.0, 230.0, 320.0, 450.0],
            );
            order_cols(o, file, no, "table", gt, 5);
            pages.push(p);
            continue;
        }
        let title = (no == 1).then_some("Grid storage field study");
        let facts = match no {
            2 => [None, Some(T1)],
            4 => [Some(T2), None],
            6 => [None, Some(T4)],
            _ => [None, None],
        };
        let (p, gt) = twocol_page(&mut r, title, facts);
        order(o, file, no, "two-column", gt);
        pages.push(p);
    }
    o.fact("t1", file, "page:2".into(), T1);
    o.fact("t2", file, "page:4".into(), T2);
    o.fact("t3", file, "page:5".into(), T3);
    o.fact("t4", file, "page:6".into(), T4);
    o.add(
        file,
        "content",
        "done",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

const C1: &str = "Сварные швы трубопровода проверяют ультразвуком на всю толщину стенки.";
const C2: &str = "Котельная переходит на летний режим после пятнадцатого мая при среднесуточной температуре выше восьми градусов.";
const C3: &str = "Журнал обходов хранится в диспетчерской не менее трёх лет.";

fn pdf_cyrillic(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "cyrillic-ru.pdf");
    let file = "files/cyrillic-ru.pdf";
    let d = FlowDoc {
        file,
        lang: Lang::Ru,
        col: Col {
            x: 72.0,
            w: 451.0,
            size: 11.0,
            lead: 14.0,
        },
        title: "Отчёт службы эксплуатации, раздел",
    };
    let mut pages = Vec::new();
    for no in 1..=6u32 {
        let fact = match no {
            2 => Some(C1),
            5 => Some(C2),
            6 => Some(C3),
            _ => None,
        };
        let (p, gt) = onecol_page(&mut r, &d, no, fact);
        order(o, file, no, "one-column", gt);
        pages.push(p);
    }
    o.fact("c1", file, "page:2".into(), C1);
    o.fact("c2", file, "page:5".into(), C2);
    o.fact("c3", file, "page:6".into(), C3);
    o.add(
        file,
        "content",
        "done",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

const K1: &str = "仓库温度必须保持在十五到二十五摄氏度之间。";
const K2: &str = "新员工入职第一周需要完成安全培训。";
const K3: &str = "東京支社の会議室は毎週金曜日に清掃されます。";

fn pdf_cjk(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "cjk-zh.pdf");
    let file = "files/cjk-zh.pdf";
    let mut pages = Vec::new();
    for no in 1..=4u32 {
        let (lang, title, fact) = match no {
            2 => (Lang::Zh, "运营报告 第二节", Some(K1)),
            3 => (Lang::Zh, "运营报告 第三节", Some(K2)),
            4 => (Lang::Ja, "運営報告 第四節", Some(K3)),
            _ => (Lang::Zh, "运营报告 第一节", None),
        };
        let d = FlowDoc {
            file,
            lang,
            col: Col {
                x: 72.0,
                w: 451.0,
                size: 11.0,
                lead: 15.0,
            },
            title,
        };
        let (p, gt) = onecol_page(&mut r, &d, no, fact);
        order(o, file, no, "cjk", gt);
        pages.push(p);
    }
    o.fact("k1", file, "page:2".into(), K1);
    o.fact("k2", file, "page:3".into(), K2);
    o.fact("k3", file, "page:4".into(), K3);
    o.add(
        file,
        "content",
        "done",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

const B1: &str = "The emergency diesel reserve at the northern depot holds 64000 litres.";
const B2: &str = "Rail sidings were extended by 400 metres to accommodate longer freight trains.";
const B3: &str = "The drone inspection programme covers the transmission lines twice a year.";
const B4: &str =
    "Spare transformer windings are stored in the climate controlled hall of building 9.";

/// Книга: колонтитулы (верхний — название, нижний — номер страницы) на каждой странице.
fn pdf_book(o: &mut Out, seed: u64, p: Profile) {
    let mut r = Rng::fork(seed, "book-500.pdf");
    let file = "files/book-500.pdf";
    let n = p.book_pages();
    let at = |full: u32| {
        if p.small {
            (full * n / 500).max(1)
        } else {
            full
        }
    };
    let facts = [
        (at(37), "b1", B1),
        (at(251), "b2", B2),
        (at(444), "b3", B3),
        (at(498), "b4", B4),
    ];
    let d = FlowDoc {
        file,
        lang: Lang::En,
        col: Col {
            x: 72.0,
            w: 451.0,
            size: 11.0,
            lead: 14.0,
        },
        title: "Chapter",
    };
    let mut pages = Vec::new();
    for no in 1..=n {
        let fact = facts.iter().find(|f| f.0 == no).map(|f| f.2);
        let (mut pg, gt) = onecol_page(&mut r, &d, no, fact);
        pg.lines.insert(
            0,
            Line {
                x: 72.0,
                y: PAGE_H - 30.0,
                size: 9.0,
                text: "Annual Operations Compendium 2025".into(),
            },
        );
        pg.lines.push(Line {
            x: PAGE_W / 2.0 - 30.0,
            y: 28.0,
            size: 9.0,
            text: format!("Page {no} of {n}"),
        });
        if no % 10 == 1 || fact.is_some() {
            order(o, file, no, "one-column", gt);
        }
        pages.push(pg);
    }
    for (pg, id, ev) in facts {
        o.fact(id, file, format!("page:{pg}"), ev);
    }
    o.add(
        file,
        "content",
        "done",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

fn pdf_scan(o: &mut Out) {
    let pages = (0..3)
        .map(|_| Page {
            image: true,
            ..Page::default()
        })
        .collect();
    o.add(
        "files/scan.pdf",
        "scan",
        "done, ocr_needed",
        pdf::build(&pdf::Doc {
            pages,
            open_action_js: None,
        }),
    );
}

// ------------------------------------------------------------------ large

/// Большие файлы: DOCX ≈ 21 млн символов (выше `max_chars` 20 млн) и XLSX 300 тыс. строк ×
/// 7 столбцов (2,1 млн ячеек — выше `max_cells` 2 млн; выдача кончается раньше, по
/// `max_chars`): сроки разбора при умолчаниях.
fn large(o: &mut Out, seed: u64) {
    let mut r = Rng::fork(seed, "large");
    let mut blocks = Vec::new();
    let mut chars = 0usize;
    let mut k = 0;
    while chars < 21_000_000 {
        if k % 40 == 0 {
            blocks.push(Block::Heading(2, format!("Раздел {}", k / 40 + 1)));
        }
        let p = paragraph(&mut r, Lang::Ru, 3, 6);
        chars += p.chars().count();
        blocks.push(Block::Para(p));
        k += 1;
    }
    let (docx, _) = ooxml::docx(&blocks);
    o.add(
        "large/big-21m.docx",
        "large",
        "done, truncated: max_chars",
        docx,
    );
    let mut rows = vec![
        [
            "Дата",
            "Номер",
            "Поставщик",
            "Товар",
            "Количество",
            "Цена",
            "Комментарий",
        ]
        .iter()
        .map(|s| Cell::Str(s.to_string()))
        .collect::<Vec<_>>(),
    ];
    for i in 0..300_000 {
        rows.push(vec![
            Cell::Date(rand_date(&mut r), DateFmt::Custom),
            Cell::Str(format!("П-{:06}", i)),
            Cell::Str(r.pick(SUPPLIERS).to_string()),
            Cell::Str(r.pick(ITEMS).to_string()),
            Cell::Num(f64::from(r.range(1, 400))),
            Cell::Num(f64::from(r.range(50, 90_000)) / 10.0),
            Cell::Str(r.pick(COMMENTS).to_string()),
        ]);
    }
    let xlsx = ooxml::xlsx(&[Sheet {
        name: "Поставки".into(),
        rows,
    }]);
    o.add(
        "large/big-300k.xlsx",
        "large",
        "done, truncated: max_chars",
        xlsx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Один сид — одинаковые байты каждого файла и одинаковый манифест; другой сид — другие
    /// файлы. Малый набор (`small`): те же генераторы, меньше строк и страниц.
    /// Сводный sha256 манифеста `manifest_in_memory(7, малый набор)`.
    const MANIFEST_SHA256_SEED7_SMALL: &str =
        "48fba1dfbf034e40ebed143cc61b5a14f024e5213236d9be0376d9e66e62070c";

    #[test]
    fn docs_corpus_deterministic() {
        let p = Profile {
            small: true,
            large: false,
        };
        // эталон: сводный sha256 манифеста малого набора (sha256 каждого файла входит в него) —
        // одинаков на Windows и Linux (CI), иначе набор зависит от платформы
        let m = manifest_in_memory(7, p);
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(
            hex(&Sha256::digest(json.as_bytes())),
            MANIFEST_SHA256_SEED7_SMALL,
            "набор изменился: если намеренно — обновите эталон и CORPUS_VERSION"
        );
        let a = generate(7, p);
        let b = generate(7, p);
        let c = generate(8, p);
        assert_eq!(a.files.len(), b.files.len());
        for ((ea, da), (eb, db)) in a.files.iter().zip(&b.files) {
            assert_eq!(ea, eb);
            assert!(da == db, "{}: байты различаются", ea.path);
        }
        assert_eq!(a.facts, b.facts);
        assert_eq!(a.order, b.order);
        let differ = a
            .files
            .iter()
            .zip(&c.files)
            .filter(|(x, y)| x.0.sha256 != y.0.sha256)
            .count();
        assert!(differ >= 10, "сид меняет содержимое: различаются {differ}");
        // состав §6: форматы, роли и не меньше 30 фактов
        let paths: Vec<&str> = a.files.iter().map(|f| f.0.path.as_str()).collect();
        for want in [
            "files/regulations-ru.docx",
            "files/inventory.xlsx",
            "files/roadmap.pptx",
            "files/onecol-en.pdf",
            "files/twocol-en.pdf",
            "files/cyrillic-ru.pdf",
            "files/cjk-zh.pdf",
            "files/book-500.pdf",
            "files/scan.pdf",
            "hostile/zip-bomb.docx",
            "hostile/xxe.docx",
            "hostile/broken.pdf",
            "hostile/js-action.pdf",
        ] {
            assert!(paths.contains(&want), "нет {want}: {paths:?}");
        }
        assert!(a.facts.len() >= 30, "фактов {}", a.facts.len());
        for f in &a.facts {
            assert!(paths.contains(&f.file.as_str()), "{}: {}", f.id, f.file);
            zam_docs::Locator::parse(&f.locator).unwrap_or_else(|e| panic!("{}: {e}", f.id));
        }
    }

    /// Факты DOCX, PPTX и XLSX — в тексте извлечения `zam-docs` и по своему `locator`
    /// (абзац — в единице, начавшейся не позже его; слайд и ячейка листа — совпадают).
    #[test]
    fn docs_corpus_facts_extract() {
        let p = Profile {
            small: true,
            large: false,
        };
        let o = generate(DEFAULT_SEED, p);
        let limits = zam_docs::Limits::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let norm = |s: &str| s.split_whitespace().collect::<String>();
        for (e, data) in &o.files {
            let kind = match e.path.rsplit('.').next() {
                Some("docx") => zam_docs::DocKind::Docx,
                Some("pptx") => zam_docs::DocKind::Pptx,
                Some("xlsx") => zam_docs::DocKind::Xlsx,
                _ => continue,
            };
            if e.role != "content" {
                continue;
            }
            let x = zam_docs::extract(kind, data, &limits, deadline).unwrap();
            for f in o.facts.iter().filter(|f| f.file == e.path) {
                let want = zam_docs::Locator::parse(&f.locator).unwrap();
                let hit = x
                    .units
                    .iter()
                    .find(|u| norm(&u.text).contains(&norm(&f.evidence)));
                let u =
                    hit.unwrap_or_else(|| panic!("{}: нет «{}» в {}", f.id, f.evidence, e.path));
                match (&u.locator, &want) {
                    (zam_docs::Locator::Para(a), zam_docs::Locator::Para(b)) => {
                        assert!(a <= b, "{}: единица para:{a} после факта para:{b}", f.id)
                    }
                    _ if f.id.starts_with('x') && ["x4", "x5", "x6"].contains(&f.id.as_str()) => {}
                    _ => assert!(
                        u.locator.overlaps(&want),
                        "{}: {:?} ≠ {}",
                        f.id,
                        u.locator,
                        f.locator
                    ),
                }
            }
        }
    }
}
