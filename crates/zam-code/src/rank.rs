//! Ранжирование карты кода (спецификация M5.6 §7): граф файлов, PageRank, ранги символов.
//!
//! Алгоритм — по образцу repo map проекта Aider (<https://github.com/Aider-AI/aider>,
//! `aider/repomap.py`, © Paul Gauthier, лицензия Apache-2.0): граф «файл ссылается на имя,
//! определённое в другом файле», веса √count × множитель имени / число определяющих файлов,
//! PageRank с персонализацией. Код не заимствован — переписан по описанию алгоритма; отличия:
//! граф не материализуется (факторизация через имена, ниже), доли символов — поток ранга по
//! имени, фокус — только вектор перезапуска (без ×10 упомянутым именам).
//!
//! **Факторизация.** Вес ссылки A на имя n, `w_An = √c × m(n) / d(n)`, от файла-определителя B не
//! зависит, поэтому рёбра A → B (их Σ ссылок × d) не строятся: поток имени
//! `F_n = Σ_A r[A] × w_An / out[A]` раздаётся всем определителям n, а определитель, сам
//! ссылающийся на n, теряет свою часть (ребра A → A нет); `out[A] = Σ_n w_An × (d(n) − [A ∈ n])`.
//! Итерация — O(ссылок + определений), итог совпадает с PageRank плотного графа (тест-эталон).
//!
//! Функции чистые: сервер считает их в `spawn_blocking`, хранит ранги файлов и доли символов;
//! персонализация — [`rank_on_graph`] по [`Graph`] из тех же входов.
//!
//! **Веса ссылок (M6 §12а)** — [`RankWeights::M6`]: вид ссылки по таблице `ref_kind_weight`
//! (тип и реализация ×2), вызов по получателю ×0,25. Входы старого клиента (`recv_count = 0`)
//! дают те же ранги, что M5.7 с той же таблицей видов; [`RankWeights::M57`] — ранги M5.7 целиком.
//!
//! **Связывание ссылки с определением (M7.5 §8.6)** — вместо «ко всем одноимённым»: ссылка идёт к
//! **группе** подходящих определений имени. В1 ([`RankWeights::bind_kind`]): `type`/`impl` — к
//! типам, `call` — к функциям и методам (конструктор Java/C# — метод), прочие виды и имя без
//! определения нужного вида — ко всем. В2 ([`RankWeights::bind_export`]): ссылка из другого
//! файла не идёт к определению с `exported = Some(false)`; своё закрытое определение файла —
//! среди подходящих (`d`), но ребра к себе нет. `d(n)` — число подходящих файлов, множитель
//! общего имени (больше 5 файлов) — по файлам группы. Поток группы, пришедший в файл, делится
//! поровну между его определениями, которые группа принимает. Без связывания группа — все
//! определения имени, номер группы — номер имени: правила M6 считаются теми же операциями в том
//! же порядке (тест `rank_m6_unchanged`).

use std::collections::{HashMap, HashSet};

use crate::{RefKind, SymbolKind};

/// Затухание PageRank (§7).
const DAMPING: f64 = 0.85;
/// Предел итераций PageRank (§7).
const MAX_ITER: usize = 50;
/// Точность PageRank: L1 между итерациями (§7).
const TOLERANCE: f64 = 1e-6;
/// Доля ранга файла на символы без входящих ссылок, если у файла есть входящие ссылки.
const UNREFERENCED_SHARE: f64 = 0.1;
/// Число файлов-определителей, сверх которого имя считается общим словом (×0,1).
const COMMON_DEFINERS: usize = 5;
/// Наименьшая длина имени, получающего ×10 за snake/camel-регистр.
const LONG_IDENT: usize = 8;
/// Наименьшая длина слова-идентификатора из `focus.text`.
const FOCUS_IDENT_MIN: usize = 3;
/// Символов из одного файла в списке центральных.
const CENTRAL_PER_FILE: usize = 3;
/// Остаток потока после вычитания своей части меньше этой доли потока имени — ноль (округление).
const FLOW_EPS: f64 = 1e-9;

/// Файл как вход ранжирования: определения и агрегированные ссылки.
#[derive(Debug, Clone, PartialEq)]
pub struct RankFile {
    /// Идентификатор строки `code_files` (для сервера; ранжирование работает по индексу).
    pub id: i64,
    pub path: String,
    /// `(name, kind, exported)` в порядке символов файла.
    pub defs: Vec<(String, SymbolKind, Option<bool>)>,
    /// `(name, kind, count, recv_count)` — как `RefAgg` протокола (`recv_count` нет — 0); виды
    /// одного имени складываются с весом вида ([`ref_kind_weight`], M6 §12а).
    pub refs: Vec<(String, RefKind, u32, u32)>,
}

/// Фокус карты (§7, §9): пути, имена и свободный текст задачи.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Focus {
    pub paths: Vec<String>,
    pub idents: Vec<String>,
    pub text: Option<String>,
}

/// Итог ранжирования; индексы — индексы входного среза [`RankFile`].
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    /// Ранг файла, сумма = 1 (при непустом входе).
    pub file_rank: Vec<f64>,
    /// Доля ранга файла на каждое определение (`defs`), сумма по файлу = 1 (если есть `defs`).
    pub symbol_share: Vec<Vec<f64>>,
}

/// Множитель имени (§7, как у Aider): ×10 — длинное (≥ 8) имя в snake/camel-регистре (такое имя
/// конкретно); ×0,1 — имя с `_` в начале; ×0,1 — имя определено больше чем в 5 файлах (общее
/// слово). Множители перемножаются. Вариант «×10 только при d > 1» замерен и отвергнут (журнал
/// этапа, ревью задачи 2, I2): он снимает множитель с уникальных типов (`StoreError`, `ApiError`).
pub fn ident_mult(name: &str, defined_in: usize) -> f64 {
    let mut m = 1.0;
    let has_alpha = name.chars().any(char::is_alphabetic);
    let snake = name.contains('_') && has_alpha;
    let kebab = name.contains('-') && has_alpha;
    let camel = name.chars().any(char::is_uppercase) && name.chars().any(char::is_lowercase);
    if (snake || kebab || camel) && name.chars().count() >= LONG_IDENT {
        m *= 10.0;
    }
    if name.starts_with('_') {
        m *= 0.1;
    }
    if defined_in > COMMON_DEFINERS {
        m *= 0.1;
    }
    m
}

/// Вес вида ссылки в счётчике `c` — фиксированная таблица M6 §12а (объявлена до замера и под
/// набор не подгоняется): тип (параметр, поле, возвращаемый тип, аннотация) и реализация — 2,0;
/// прямой вызов, импорт, прочее — 1,0. Вызов по получателю — [`RECV_CALL_WEIGHT`].
fn ref_kind_weight(kind: RefKind) -> f64 {
    match kind {
        RefKind::Type | RefKind::Impl => 2.0,
        RefKind::Call | RefKind::Import | RefKind::Other => 1.0,
    }
}

/// Вес вызова по получателю (`x.f()`, `self.f()`, `a.b.f()`; M6 §12а): такие вызовы стекаются к
/// немногим файлам, где имя определено методом (`is_empty`, `len`).
const RECV_CALL_WEIGHT: f64 = 0.25;

/// Правила весов и связывания ссылок. Продукт — правила сервера (`zam-server` `api/code.rs`,
/// с M7.5 — [`RankWeights::M75`]); [`rank`], [`Graph::new`] и [`edges_among`] — M6; прочие — для
/// абляции замера (`cargo xtask code-bench`: M6 §12а — справочно, M7.5 §8.6 — выбор связывания по
/// правилу, записанному до прогона).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankWeights {
    /// Таблица весов вида [`ref_kind_weight`]; `false` — все виды 1,0 (как M5.7).
    pub kind_table: bool,
    /// Вызов по получателю с весом [`RECV_CALL_WEIGHT`]; `false` — как прямой (как M5.7).
    pub recv: bool,
    /// В1 «по виду» (M7.5 §8.6 п. 1): `type`/`impl` — к типам, `call` — к функциям и методам;
    /// `false` — ко всем одноимённым (M6).
    pub bind_kind: bool,
    /// В2 «экспорт» (M7.5 §8.6 п. 2): ссылка из другого файла не идёт к определению с
    /// `exported = Some(false)`; `false` — идёт (M6).
    pub bind_export: bool,
}

impl RankWeights {
    /// Правила M6 (§12а): связывания нет.
    pub const M6: RankWeights = RankWeights {
        kind_table: true,
        recv: true,
        bind_kind: false,
        bind_export: false,
    };
    /// Правила M5.7: виды равны, получатель не различается.
    pub const M57: RankWeights = RankWeights {
        kind_table: false,
        recv: false,
        bind_kind: false,
        bind_export: false,
    };
    /// Правила M7.5 (§8.6 п. 3): M6 и В2 «экспорт» — точка, выбранная `cargo xtask code-bench` по
    /// правилу, записанному до прогона (журнал этапа, «M7.5 · Задача 4»: 5/10 против 4/10 у M6,
    /// В1 и В1 + В2 — тоже 5/10, но больше сумма мест или изменённых ключей).
    pub const M75: RankWeights = RankWeights {
        kind_table: true,
        recv: true,
        bind_kind: false,
        bind_export: true,
    };

    /// Отпечаток правил (`rank:` и четыре флага `0`/`1` по порядку полей): им сервер помечает,
    /// какими правилами посчитаны сохранённые ранги карты (`zam_store::code_rank_rules`).
    pub fn rules_id(self) -> String {
        let b = |x: bool| if x { '1' } else { '0' };
        format!(
            "rank:{}{}{}{}",
            b(self.kind_table),
            b(self.recv),
            b(self.bind_kind),
            b(self.bind_export)
        )
    }

    /// Вклад ссылки в счётчик `c`: `count` с весом вида; у `call` —
    /// `(count − recv) × 1,0 + recv × 0,25` (`recv` не больше `count`).
    fn contribution(self, kind: RefKind, count: u32, recv: u32) -> f64 {
        let k = if self.kind_table {
            ref_kind_weight(kind)
        } else {
            1.0
        };
        if self.recv && kind == RefKind::Call {
            let r = recv.min(count);
            k * f64::from(count - r) + RECV_CALL_WEIGHT * f64::from(r)
        } else {
            k * f64::from(count)
        }
    }
}

/// Класс группы связывания В1: определения-типы (struct, enum, trait, class, interface,
/// псевдоним типа).
const CLASS_TYPE: usize = 0;
/// Класс группы связывания В1: функции и методы (конструктор Java/C# — метод).
const CLASS_CALL: usize = 1;
/// Нет группы класса у имени.
const NO_GROUP: usize = usize::MAX;

/// Класс ссылки при В1: `type`/`impl` — типы, `call` — функции и методы; прочие — все виды
/// (`None`).
fn ref_class(kind: RefKind) -> Option<usize> {
    match kind {
        RefKind::Type | RefKind::Impl => Some(CLASS_TYPE),
        RefKind::Call => Some(CLASS_CALL),
        RefKind::Import | RefKind::Other => None,
    }
}

/// Класс определения при В1 ([`ref_class`]); прочие виды (константа, модуль, макрос) — `None`.
fn def_class(kind: SymbolKind) -> Option<usize> {
    match kind {
        SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Trait
        | SymbolKind::Type => Some(CLASS_TYPE),
        SymbolKind::Function | SymbolKind::Method => Some(CLASS_CALL),
        SymbolKind::Const | SymbolKind::Module | SymbolKind::Macro => None,
    }
}

/// Определение видно ссылке из другого файла при В2: всё, кроме `exported = Some(false)` (`None`
/// — язык без признака, Python).
fn visible(exported: Option<bool>) -> bool {
    exported != Some(false)
}

/// Группы, к которым относится определение имени `id` вида `kind`: все определения имени (номер
/// группы = номер имени) и, при В1, группа его класса ([`NO_GROUP`] — нет).
fn member_groups(id: usize, kind: SymbolKind, class_groups: &[[usize; 2]]) -> [usize; 2] {
    let class = class_groups
        .get(id)
        .zip(def_class(kind))
        .map_or(NO_GROUP, |(g, c)| g[c]);
    [id, class]
}

/// Файлы каждой группы по возрастанию (`files[start[g]..start[g + 1]]`): определения, которые
/// `keep(exported)` оставляет; повтор файла в группе — один раз.
fn group_files(
    files: &[RankFile],
    def_ids: &[Vec<usize>],
    class_groups: &[[usize; 2]],
    groups: usize,
    keep: impl Fn(Option<bool>) -> bool,
) -> (Vec<usize>, Vec<usize>) {
    let mut count = vec![0usize; groups];
    let mut last = vec![usize::MAX; groups];
    let each = |f: &mut dyn FnMut(usize, usize)| {
        for (i, (file, own)) in files.iter().zip(def_ids).enumerate() {
            for ((_, kind, e), &id) in file.defs.iter().zip(own) {
                if keep(*e) {
                    for g in member_groups(id, *kind, class_groups) {
                        if g != NO_GROUP {
                            f(i, g);
                        }
                    }
                }
            }
        }
    };
    each(&mut |i, g| {
        if last[g] != i {
            last[g] = i;
            count[g] += 1;
        }
    });
    let mut start = Vec::with_capacity(groups + 1);
    start.push(0);
    for c in &count {
        start.push(start.last().copied().unwrap_or(0) + c);
    }
    let mut fill = start.clone();
    let mut out = vec![0; fill.last().copied().unwrap_or(0)];
    last.iter_mut().for_each(|x| *x = usize::MAX);
    each(&mut |i, g| {
        if last[g] != i {
            last[g] = i;
            out[fill[g]] = i;
            fill[g] += 1;
        }
    });
    (start, out)
}

/// Ссылка графа: файл, группа связывания, `w_Ag / out[A]` (доля исходящего веса файла).
#[derive(Debug, Clone, Copy, PartialEq)]
struct GraphRef {
    src: usize,
    group: usize,
    wq: f64,
}

/// Граф файлов в факторизованном виде: интернированные имена, группы связывания с их файлами,
/// ссылки по группе, исходящий вес файла. Строится за O(ссылок + определений), по одному поиску
/// имени на ссылку; заимствует имена у входного среза.
#[derive(Debug, Clone, PartialEq)]
pub struct Graph<'a> {
    n: usize,
    /// Интернирование имён: `std` `HashMap` (SipHash со случайным ключом) — имена приходят
    /// от клиентов, детерминированный хэш позволил бы подобрать коллизии (HashDoS).
    ids: HashMap<&'a str, usize>,
    /// Файлы группы `g`: `def_files[def_start[g]..def_start[g + 1]]`, по возрастанию. Группы
    /// `0..имён` — все определения имени (номер группы = номер имени, как M6), дальше — классы В1.
    def_start: Vec<usize>,
    def_files: Vec<usize>,
    /// Группы классов имени `[типы, функции]` при В1 ([`NO_GROUP`] — нет); без В1 — пусто.
    class_groups: Vec<[usize; 2]>,
    /// При В2 — принимающие файлы группы (с открытым определением), как `def_start`/`def_files`;
    /// `None` — все файлы группы.
    open: Option<(Vec<usize>, Vec<usize>)>,
    /// id имени каждого определения файла (параллельно `RankFile::defs`).
    def_ids: Vec<Vec<usize>>,
    refs: Vec<GraphRef>,
    /// Ссылки файла на группы, куда он сам принимает поток (ребра A → A нет — часть вычитается).
    self_refs: Vec<GraphRef>,
    out: Vec<f64>,
    w: RankWeights,
}

impl<'a> Graph<'a> {
    /// Граф из входов ранжирования по правилам M6; ссылки на неопределённые имена и на имена,
    /// определённые только в самом файле, рёбер не дают.
    pub fn new(files: &'a [RankFile]) -> Graph<'a> {
        Graph::with_weights(files, RankWeights::M6)
    }

    /// Граф с правилами весов и связывания `w` (сервер — со своими правилами, абляция замера).
    pub fn with_weights(files: &'a [RankFile], w: RankWeights) -> Graph<'a> {
        let total_defs: usize = files.iter().map(|f| f.defs.len()).sum();
        let mut ids: HashMap<&'a str, usize> = HashMap::new();
        ids.reserve(total_defs);
        let mut def_ids = Vec::with_capacity(files.len());
        for f in files {
            let mut own = Vec::with_capacity(f.defs.len());
            for (name, _, _) in &f.defs {
                let next = ids.len();
                own.push(*ids.entry(name.as_str()).or_insert(next));
            }
            def_ids.push(own);
        }
        let names = ids.len();
        let mut name_of = vec![""; names];
        for (name, &id) in &ids {
            name_of[id] = name;
        }
        // группы классов В1: номер — по первому определению класса в порядке файлов
        let mut group_name: Vec<usize> = (0..names).collect();
        let mut class_groups = Vec::new();
        if w.bind_kind {
            class_groups = vec![[NO_GROUP; 2]; names];
            for (f, own) in files.iter().zip(&def_ids) {
                for ((_, kind, _), &id) in f.defs.iter().zip(own) {
                    if let Some(c) = def_class(*kind)
                        && class_groups[id][c] == NO_GROUP
                    {
                        class_groups[id][c] = group_name.len();
                        group_name.push(id);
                    }
                }
            }
        }
        let groups = group_name.len();
        let (def_start, def_files) = group_files(files, &def_ids, &class_groups, groups, |_| true);
        let open = w
            .bind_export
            .then(|| group_files(files, &def_ids, &class_groups, groups, visible));
        let mult: Vec<f64> = (0..groups)
            .map(|g| ident_mult(name_of[group_name[g]], def_start[g + 1] - def_start[g]))
            .collect();
        let mut refs = Vec::new();
        let mut self_refs = Vec::new();
        let mut out = vec![0.0; files.len()];
        let mut by_group: Vec<(usize, f64)> = Vec::new();
        for (a, f) in files.iter().enumerate() {
            by_group.clear();
            for (name, kind, count, recv) in &f.refs {
                if *count > 0
                    && let Some(&id) = ids.get(name.as_str())
                {
                    let g = group_of(id, *kind, w, &class_groups);
                    by_group.push((g, w.contribution(*kind, *count, *recv)));
                }
            }
            by_group.sort_unstable_by_key(|x| x.0);
            let first = refs.len();
            let mut k = 0;
            while k < by_group.len() {
                let g = by_group[k].0;
                let mut c = 0.0;
                while k < by_group.len() && by_group[k].0 == g {
                    c += by_group[k].1;
                    k += 1;
                }
                let defs = &def_files[def_start[g]..def_start[g + 1]];
                let self_def = defs.binary_search(&a).is_ok();
                let (rcv, self_rcv) = match &open {
                    Some((s, o)) => {
                        let rcv = &o[s[g]..s[g + 1]];
                        (rcv, rcv.binary_search(&a).is_ok())
                    }
                    None => (defs, self_def),
                };
                // своё закрытое определение — среди подходящих, но потока не принимает
                let d = rcv.len() + usize::from(self_def && !self_rcv);
                let targets = rcv.len() - usize::from(self_rcv);
                if targets == 0 {
                    continue;
                }
                let w = c.sqrt() * mult[g] / d as f64;
                out[a] += w * targets as f64;
                let e = GraphRef {
                    src: a,
                    group: g,
                    wq: w,
                };
                refs.push(e);
                if self_rcv {
                    self_refs.push(e);
                }
            }
            for e in &mut refs[first..] {
                e.wq /= out[a];
            }
        }
        for e in &mut self_refs {
            e.wq /= out[e.src];
        }
        Graph {
            n: files.len(),
            ids,
            def_start,
            def_files,
            class_groups,
            open,
            def_ids,
            refs,
            self_refs,
            out,
            w,
        }
    }

    /// Число файлов.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Граф без файлов.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Число файлов, где имя определено (любого вида).
    pub fn definers_count(&self, name: &str) -> usize {
        self.ids
            .get(name)
            .map_or(0, |&id| self.def_start[id + 1] - self.def_start[id])
    }

    /// Число групп связывания (без связывания — определённых имён).
    fn groups(&self) -> usize {
        self.def_start.len() - 1
    }

    /// Файлы, принимающие поток группы `g`.
    fn receivers(&self, g: usize) -> &[usize] {
        match &self.open {
            Some((s, o)) => &o[s[g]..s[g + 1]],
            None => &self.def_files[self.def_start[g]..self.def_start[g + 1]],
        }
    }

    /// Группы, чей поток принимает определение имени `id` вида `kind` ([`NO_GROUP`] — нет):
    /// при В2 закрытое (`Some(false)`) не принимает ничего.
    fn takers(&self, id: usize, kind: SymbolKind, exported: Option<bool>) -> [usize; 2] {
        if self.w.bind_export && !visible(exported) {
            return [NO_GROUP; 2];
        }
        member_groups(id, kind, &self.class_groups)
    }

    /// Поток каждой группы `F_g = Σ_A r[A] × w_Ag / out[A]` (без затухания).
    fn flows(&self, r: &[f64], flow: &mut [f64]) {
        flow.iter_mut().for_each(|x| *x = 0.0);
        for e in &self.refs {
            flow[e.group] += r[e.src] * e.wq;
        }
    }
}

/// Группы из `[usize; 2]` ([`Graph::takers`], [`member_groups`]) без [`NO_GROUP`].
fn each(t: &[usize; 2]) -> impl Iterator<Item = usize> + '_ {
    t.iter().copied().filter(|&x| x != NO_GROUP)
}

/// Группа ссылки вида `kind` на имя `id`: при В1 — группа класса, если у имени есть определения
/// этого класса; иначе — все определения имени (номер группы = номер имени).
fn group_of(id: usize, kind: RefKind, w: RankWeights, class_groups: &[[usize; 2]]) -> usize {
    if !w.bind_kind {
        return id;
    }
    ref_class(kind)
        .map(|c| class_groups[id][c])
        .filter(|&g| g != NO_GROUP)
        .unwrap_or(id)
}

/// Вектор перезапуска: нормированный `restart` или равномерный, если его нет, длина не `n`,
/// или в нём нет положительной конечной массы.
fn restart_vector(n: usize, restart: Option<&[f64]>) -> Vec<f64> {
    let clean = restart.filter(|r| r.len() == n).map(|r| {
        r.iter()
            .map(|v| if v.is_finite() && *v > 0.0 { *v } else { 0.0 })
            .collect::<Vec<_>>()
    });
    match clean {
        Some(v) if v.iter().sum::<f64>() > 0.0 => {
            let s: f64 = v.iter().sum();
            v.into_iter().map(|x| x / s).collect()
        }
        _ => vec![1.0 / n as f64; n],
    }
}

/// Степенной метод: затухание 0,85, ≤ 50 итераций, L1 < 1e-6; масса висячих узлов (`out = 0`) —
/// по вектору перезапуска. `spread(r, next)` добавляет в `next` поток `0,85 × …` по рёбрам.
fn power_iteration(
    n: usize,
    restart: Option<&[f64]>,
    out: &[f64],
    mut spread: impl FnMut(&[f64], &mut [f64]),
) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let p = restart_vector(n, restart);
    let mut r = vec![1.0 / n as f64; n];
    for _ in 0..MAX_ITER {
        let dangling: f64 = (0..n).filter(|&i| out[i] == 0.0).map(|i| r[i]).sum();
        let base = (1.0 - DAMPING) + DAMPING * dangling;
        let mut next: Vec<f64> = p.iter().map(|pi| base * pi).collect();
        spread(&r, &mut next);
        let total: f64 = next.iter().sum();
        if total > 0.0 {
            next.iter_mut().for_each(|v| *v /= total);
        }
        let l1: f64 = next.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        r = next;
        if l1 < TOLERANCE {
            break;
        }
    }
    r
}

/// PageRank по явным рёбрам `(src, dst, weight)` (обзор, тесты): параметры §7; мусорные рёбра
/// (индекс вне `n`, вес ≤ 0 или не конечный) отбрасываются. Сумма итога = 1.
pub fn pagerank(n: usize, edges: &[(usize, usize, f64)], restart: Option<&[f64]>) -> Vec<f64> {
    let edges: Vec<_> = edges
        .iter()
        .copied()
        .filter(|&(s, d, w)| s < n && d < n && w.is_finite() && w > 0.0)
        .collect();
    let mut out = vec![0.0; n];
    for &(s, _, w) in &edges {
        out[s] += w;
    }
    power_iteration(n, restart, &out, |r, next| {
        for &(s, d, w) in &edges {
            next[d] += DAMPING * r[s] * w / out[s];
        }
    })
}

/// PageRank по факторизованному графу (персонализация на сервере: `restart` из
/// [`focus_restart`]). Совпадает с [`pagerank`] по плотным рёбрам того же графа.
pub fn rank_on_graph(g: &Graph<'_>, restart: Option<&[f64]>) -> Vec<f64> {
    let mut flow = vec![0.0; g.groups()];
    power_iteration(g.n, restart, &g.out, |r, next| {
        g.flows(r, &mut flow);
        for e in &g.self_refs {
            next[e.src] -= DAMPING * r[e.src] * e.wq;
        }
        for (id, f) in flow.iter().enumerate() {
            if *f != 0.0 {
                let add = DAMPING * f;
                for &b in g.receivers(id) {
                    next[b] += add;
                }
            }
        }
    })
}

/// Файлы группы связывания для [`edges_among_with`]: все с определением группы и (В2) с
/// открытым — по возрастанию.
#[derive(Default)]
struct GroupFiles {
    files: Vec<usize>,
    open: Vec<usize>,
}

/// [`edges_among_with`] по правилам M6.
pub fn edges_among(files: &[RankFile], shown: &[usize]) -> Vec<(usize, usize, f64)> {
    edges_among_with(files, shown, RankWeights::M6)
}

/// Рёбра графа только между файлами `shown` (обзор WebUI, `format=json`) по правилам `w`:
/// `(src, dst, weight)` с индексами входного среза, по убыванию веса, при равенстве — по
/// `(src, dst)`. `d` и множитель имени — по всем файлам. Стоимость — O(определений + ссылок
/// показанных × файлов группы).
pub fn edges_among_with(
    files: &[RankFile],
    shown: &[usize],
    w: RankWeights,
) -> Vec<(usize, usize, f64)> {
    let in_shown: HashSet<usize> = shown.iter().copied().filter(|&i| i < files.len()).collect();
    // группа — (имя, класс В1); `None` — все определения имени
    let mut groups: HashMap<(&str, Option<usize>), GroupFiles> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for (name, kind, e) in &f.defs {
            let class = def_class(*kind).filter(|_| w.bind_kind);
            for key in [Some(None), class.map(Some)].into_iter().flatten() {
                let g = groups.entry((name.as_str(), key)).or_default();
                if g.files.last() != Some(&i) {
                    g.files.push(i);
                }
                if visible(*e) && g.open.last() != Some(&i) {
                    g.open.push(i);
                }
            }
        }
    }
    let mut sum: HashMap<(usize, usize), f64> = HashMap::new();
    let mut src: Vec<usize> = in_shown.iter().copied().collect();
    src.sort_unstable();
    for a in src {
        let mut by_group: HashMap<(&str, Option<usize>), f64> = HashMap::new();
        for (name, kind, count, recv) in &files[a].refs {
            if *count == 0 {
                continue;
            }
            let class = ref_class(*kind)
                .filter(|c| w.bind_kind && groups.contains_key(&(name.as_str(), Some(*c))));
            *by_group.entry((name.as_str(), class)).or_default() +=
                w.contribution(*kind, *count, *recv);
        }
        let mut by_group: Vec<_> = by_group.into_iter().collect();
        by_group.sort_by(|x, y| x.0.cmp(&y.0));
        for (key, c) in by_group {
            let Some(g) = groups.get(&key) else {
                continue;
            };
            let rcv = if w.bind_export { &g.open } else { &g.files };
            let self_def = g.files.binary_search(&a).is_ok();
            let self_rcv = rcv.binary_search(&a).is_ok();
            let d = rcv.len() + usize::from(self_def && !self_rcv);
            if rcv.len() == usize::from(self_rcv) {
                continue;
            }
            let wt = c.sqrt() * ident_mult(key.0, g.files.len()) / d as f64;
            for &b in rcv.iter().filter(|&&b| b != a && in_shown.contains(&b)) {
                *sum.entry((a, b)).or_default() += wt;
            }
        }
    }
    let mut edges: Vec<_> = sum.into_iter().map(|((a, b), w)| (a, b, w)).collect();
    edges.sort_by(|x, y| y.2.total_cmp(&x.2).then((x.0, x.1).cmp(&(y.0, y.1))));
    edges
}

/// Слова-идентификаторы текста: `[\p{Alphabetic}_][\p{Alphanumeric}_]*` длиной ≥ 3 символов,
/// без повторов, в порядке появления.
pub fn focus_idents(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let words = text.split(|c: char| !(c.is_alphanumeric() || c == '_'));
    for w in words {
        let first_ok = w
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_');
        if first_ok && w.chars().count() >= FOCUS_IDENT_MIN && seen.insert(w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Масса перезапуска фокуса: +1 файлу из `paths`, +1 файлу с определением любого из `idents`,
/// и за каждое имя из `focus_idents(text)` (кроме явных `idents`) — масса имени `1/d`,
/// разделённая между его d определителями (`1/d²` каждому): общее слово текста («new», «tests»)
/// не размывает фокус. Совпадения точные. Нет совпадений — `None` (глобальный ранг).
pub fn focus_restart(files: &[RankFile], f: &Focus) -> Option<Vec<f64>> {
    let paths: HashSet<&str> = f.paths.iter().map(String::as_str).collect();
    let idents: HashSet<&str> = f.idents.iter().map(String::as_str).collect();
    let text_idents = f.text.as_deref().map(focus_idents).unwrap_or_default();
    let text: HashSet<&str> = text_idents
        .iter()
        .map(String::as_str)
        .filter(|w| !idents.contains(w))
        .collect();
    let mut d: HashMap<&str, usize> = HashMap::new();
    let defined: Vec<HashSet<&str>> = files
        .iter()
        .map(|file| file.defs.iter().map(|(n, _, _)| n.as_str()).collect())
        .collect();
    for names in &defined {
        for n in names.iter().filter(|n| text.contains(*n)) {
            *d.entry(n).or_default() += 1;
        }
    }
    let mass: Vec<f64> = files
        .iter()
        .zip(&defined)
        .map(|(file, names)| {
            let mut m = 0.0;
            if paths.contains(file.path.as_str()) {
                m += 1.0;
            }
            if names.iter().any(|n| idents.contains(n)) {
                m += 1.0;
            }
            for n in names.iter().filter(|n| text.contains(*n)) {
                let dn = d[n] as f64;
                m += 1.0 / (dn * dn);
            }
            m
        })
        .collect();
    mass.iter().any(|m| *m > 0.0).then_some(mass)
}

/// Вес символа без входящих ссылок: типы ×2, экспортируемые ×2.
fn kind_weight(kind: SymbolKind, exported: Option<bool>) -> f64 {
    let ty = matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
            | SymbolKind::Type
    );
    let k = if ty { 2.0 } else { 1.0 };
    if exported == Some(true) { k * 2.0 } else { k }
}

/// Доли символов файла по входящему потоку `inflow[j]` каждого определения (поток группы делится
/// поровну между `same[j]` принимающими его определениями файла); символам без входящих ссылок —
/// [`UNREFERENCED_SHARE`] по весу вида, или всё, если входящих ссылок у файла нет.
fn shares(file: &RankFile, inflow: &[f64], same: &[f64]) -> Vec<f64> {
    let ref_total: f64 = inflow.iter().zip(same).map(|(f, s)| f / s).sum();
    let cold_total: f64 = file
        .defs
        .iter()
        .zip(inflow)
        .filter(|(_, f)| **f <= 0.0)
        .map(|((_, k, e), _)| kind_weight(*k, *e))
        .sum();
    let (hot_part, cold_part) = match (ref_total > 0.0, cold_total > 0.0) {
        (true, true) => (1.0 - UNREFERENCED_SHARE, UNREFERENCED_SHARE),
        (true, false) => (1.0, 0.0),
        _ => (0.0, 1.0),
    };
    file.defs
        .iter()
        .zip(inflow.iter().zip(same))
        .map(|((_, k, e), (f, s))| {
            if *f > 0.0 {
                hot_part * f / ref_total / s
            } else if cold_total > 0.0 {
                cold_part * kind_weight(*k, *e) / cold_total
            } else {
                0.0
            }
        })
        .collect()
}

/// Ранжирование: граф, PageRank (с персонализацией по `focus`, если есть совпадения), доли
/// символов по входящему потоку ранга через имя (`F_n` без своей части файла).
///
/// С `focus` доли считаются от персонализированного ранга; сервер же хранит доли глобального
/// ранжирования и при `focus` умножает их на [`rank_on_graph`] с [`focus_restart`] — так ранги
/// символов с фокусом и без сопоставимы, а пересчитывать доли на каждый запрос не нужно.
pub fn rank(files: &[RankFile], focus: Option<&Focus>) -> Ranked {
    rank_with(files, focus, RankWeights::M6)
}

/// [`rank`] с правилами весов и связывания `w` (сервер — со своими правилами, абляция замера).
///
/// Доли: поток группы, пришедший в файл (без своей части), делится поровну между определениями
/// файла, которые группа принимает (`same`). Определение, принимающее поток нескольких групп
/// (тип при В1: группа типов и группа всех определений имени), получает сумму частей.
pub fn rank_with(files: &[RankFile], focus: Option<&Focus>, w: RankWeights) -> Ranked {
    let g = Graph::with_weights(files, w);
    let restart = focus.and_then(|f| focus_restart(files, f));
    let file_rank = rank_on_graph(&g, restart.as_deref());
    let mut flow = vec![0.0; g.groups()];
    g.flows(&file_rank, &mut flow);
    let own: HashMap<(usize, usize), f64> = g
        .self_refs
        .iter()
        .map(|e| ((e.src, e.group), file_rank[e.src] * e.wq))
        .collect();
    let mut count = vec![0.0f64; g.groups()];
    let symbol_share = files
        .iter()
        .zip(&g.def_ids)
        .enumerate()
        .map(|(i, (f, ids))| {
            let takers: Vec<[usize; 2]> = f
                .defs
                .iter()
                .zip(ids)
                .map(|((_, kind, e), &id)| g.takers(id, *kind, *e))
                .collect();
            takers.iter().flat_map(each).for_each(|x| count[x] += 1.0);
            // одна группа — (поток, число принимающих), как M6; несколько — сумма частей
            let (inflow, same): (Vec<f64>, Vec<f64>) = takers
                .iter()
                .map(|t| {
                    let mut parts = 0;
                    let mut one = (0.0, 1.0);
                    let mut sum = 0.0;
                    for x in each(t) {
                        let total = flow[x];
                        let v = total - own.get(&(i, x)).copied().unwrap_or(0.0);
                        if v > total * FLOW_EPS {
                            parts += 1;
                            one = (v, count[x]);
                            sum += v / count[x];
                        }
                    }
                    if parts > 1 { (sum, 1.0) } else { one }
                })
                .unzip();
            takers.iter().flat_map(each).for_each(|x| count[x] = 0.0);
            shares(f, &inflow, &same)
        })
        .collect();
    Ranked {
        file_rank,
        symbol_share,
    }
}

/// Центральные символы: первые `n` по рангу символа (ранг файла × доля), не больше трёх из одного
/// файла; `(индекс файла, индекс определения)`, при равенстве — по индексам.
pub fn central(files: &[RankFile], r: &Ranked, n: usize) -> Vec<(usize, usize)> {
    let mut all: Vec<(f64, usize, usize)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let fr = r.file_rank.get(i).copied().unwrap_or(0.0);
        for j in 0..f.defs.len() {
            let share = r.symbol_share.get(i).and_then(|s| s.get(j)).copied();
            all.push((fr * share.unwrap_or(0.0), i, j));
        }
    }
    all.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut per_file: HashMap<usize, usize> = HashMap::new();
    let mut picked = Vec::new();
    for (_, i, j) in all {
        if picked.len() >= n {
            break;
        }
        let c = per_file.entry(i).or_default();
        if *c < CENTRAL_PER_FILE {
            *c += 1;
            picked.push((i, j));
        }
    }
    picked
}
