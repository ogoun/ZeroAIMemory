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

/// Правила весов ссылок. Продукт — только [`RankWeights::M6`]; прочие — для справочной абляции
/// замера (`cargo xtask code-bench`, M6 §12а): выбор из абляции не делается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankWeights {
    /// Таблица весов вида [`ref_kind_weight`]; `false` — все виды 1,0 (как M5.7).
    pub kind_table: bool,
    /// Вызов по получателю с весом [`RECV_CALL_WEIGHT`]; `false` — как прямой (как M5.7).
    pub recv: bool,
}

impl RankWeights {
    /// Правила M6 (§12а).
    pub const M6: RankWeights = RankWeights {
        kind_table: true,
        recv: true,
    };
    /// Правила M5.7: виды равны, получатель не различается.
    pub const M57: RankWeights = RankWeights {
        kind_table: false,
        recv: false,
    };

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

/// Ссылки файла по имени: `(name, c)`, где `c` — сумма вкладов по видам
/// ([`RankWeights::contribution`]); по имени.
fn agg_refs(file: &RankFile, w: RankWeights) -> Vec<(&str, f64)> {
    let mut by_name: HashMap<&str, f64> = HashMap::new();
    for (name, kind, count, recv) in &file.refs {
        if *count > 0 {
            *by_name.entry(name.as_str()).or_default() += w.contribution(*kind, *count, *recv);
        }
    }
    let mut out: Vec<_> = by_name.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(b.0));
    out
}

/// Вес ссылки файла на имя: `√c × ident_mult(name, d) / d`.
fn ref_weight(name: &str, c: f64, d: usize) -> f64 {
    c.sqrt() * ident_mult(name, d) / d as f64
}

/// Ссылка графа: файл, id имени, `w_An / out[A]` (доля исходящего веса файла).
#[derive(Debug, Clone, Copy, PartialEq)]
struct GraphRef {
    src: usize,
    name: usize,
    wq: f64,
}

/// Граф файлов в факторизованном виде: интернированные имена, определители, ссылки по id имени,
/// исходящий вес файла. Строится за O(ссылок + определений), по одному поиску имени на ссылку;
/// заимствует имена у входного среза.
#[derive(Debug, Clone, PartialEq)]
pub struct Graph<'a> {
    n: usize,
    /// Интернирование имён: `std` `HashMap` (SipHash со случайным ключом) — имена приходят
    /// от клиентов, детерминированный хэш позволил бы подобрать коллизии (HashDoS).
    ids: HashMap<&'a str, usize>,
    /// Определители имени `id`: `def_files[def_start[id]..def_start[id + 1]]`, по возрастанию.
    def_start: Vec<usize>,
    def_files: Vec<usize>,
    /// id имени каждого определения файла (параллельно `RankFile::defs`).
    def_ids: Vec<Vec<usize>>,
    refs: Vec<GraphRef>,
    /// Ссылки файла на имена, которые он сам определяет (ребра A → A нет — их часть вычитается).
    self_refs: Vec<GraphRef>,
    out: Vec<f64>,
}

impl<'a> Graph<'a> {
    /// Граф из входов ранжирования по правилам M6; ссылки на неопределённые имена и на имена,
    /// определённые только в самом файле, рёбер не дают.
    pub fn new(files: &'a [RankFile]) -> Graph<'a> {
        Graph::with_weights(files, RankWeights::M6)
    }

    /// Граф с правилами весов `w` (абляция замера; продукт — [`Graph::new`]).
    pub fn with_weights(files: &'a [RankFile], w: RankWeights) -> Graph<'a> {
        let total_defs: usize = files.iter().map(|f| f.defs.len()).sum();
        let mut ids: HashMap<&'a str, usize> = HashMap::new();
        ids.reserve(total_defs);
        let mut def_ids = Vec::with_capacity(files.len());
        // число определителей имени; `last` — последний учтённый файл (повтор имени в файле)
        let mut count: Vec<usize> = Vec::new();
        let mut last: Vec<usize> = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let mut own = Vec::with_capacity(f.defs.len());
            for (name, _, _) in &f.defs {
                let next = count.len();
                let id = *ids.entry(name.as_str()).or_insert(next);
                if id == next {
                    count.push(0);
                    last.push(usize::MAX);
                }
                if last[id] != i {
                    last[id] = i;
                    count[id] += 1;
                }
                own.push(id);
            }
            def_ids.push(own);
        }
        let mut def_start = Vec::with_capacity(count.len() + 1);
        def_start.push(0);
        for c in &count {
            def_start.push(def_start.last().copied().unwrap_or(0) + c);
        }
        let mut fill = def_start.clone();
        let mut def_files = vec![0; fill.last().copied().unwrap_or(0)];
        last.iter_mut().for_each(|x| *x = usize::MAX);
        for (i, own) in def_ids.iter().enumerate() {
            for &id in own {
                if last[id] != i {
                    last[id] = i;
                    def_files[fill[id]] = i;
                    fill[id] += 1;
                }
            }
        }
        let mut mult = vec![0.0; count.len()];
        for (name, &id) in &ids {
            mult[id] = ident_mult(name, count[id]);
        }
        let mut refs = Vec::new();
        let mut self_refs = Vec::new();
        let mut out = vec![0.0; files.len()];
        let mut by_id: Vec<(usize, f64)> = Vec::new();
        for (a, f) in files.iter().enumerate() {
            by_id.clear();
            for (name, kind, count, recv) in &f.refs {
                if *count > 0
                    && let Some(&id) = ids.get(name.as_str())
                {
                    by_id.push((id, w.contribution(*kind, *count, *recv)));
                }
            }
            by_id.sort_unstable_by_key(|x| x.0);
            let first = refs.len();
            let mut k = 0;
            while k < by_id.len() {
                let id = by_id[k].0;
                let mut c = 0.0;
                while k < by_id.len() && by_id[k].0 == id {
                    c += by_id[k].1;
                    k += 1;
                }
                let defs = &def_files[def_start[id]..def_start[id + 1]];
                let self_def = defs.binary_search(&a).is_ok();
                let targets = defs.len() - usize::from(self_def);
                if targets == 0 {
                    continue;
                }
                let w = c.sqrt() * mult[id] / defs.len() as f64;
                out[a] += w * targets as f64;
                let e = GraphRef {
                    src: a,
                    name: id,
                    wq: w,
                };
                refs.push(e);
                if self_def {
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
            def_ids,
            refs,
            self_refs,
            out,
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

    /// Число файлов, где имя определено.
    pub fn definers_count(&self, name: &str) -> usize {
        self.ids
            .get(name)
            .map_or(0, |&id| self.def_start[id + 1] - self.def_start[id])
    }

    /// Число интернированных (определённых) имён.
    fn names(&self) -> usize {
        self.def_start.len() - 1
    }

    /// Поток каждого имени `F_n = Σ_A r[A] × w_An / out[A]` (без затухания).
    fn flows(&self, r: &[f64], flow: &mut [f64]) {
        flow.iter_mut().for_each(|x| *x = 0.0);
        for e in &self.refs {
            flow[e.name] += r[e.src] * e.wq;
        }
    }
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
    let mut flow = vec![0.0; g.names()];
    power_iteration(g.n, restart, &g.out, |r, next| {
        g.flows(r, &mut flow);
        for e in &g.self_refs {
            next[e.src] -= DAMPING * r[e.src] * e.wq;
        }
        for (id, f) in flow.iter().enumerate() {
            if *f != 0.0 {
                let add = DAMPING * f;
                for &b in &g.def_files[g.def_start[id]..g.def_start[id + 1]] {
                    next[b] += add;
                }
            }
        }
    })
}

/// Рёбра графа только между файлами `shown` (обзор WebUI, `format=json`): `(src, dst, weight)`
/// с индексами входного среза, по убыванию веса, при равенстве — по `(src, dst)`. `d(name)` —
/// по всем файлам. Стоимость — O(ссылок показанных × определителей).
pub fn edges_among(files: &[RankFile], shown: &[usize]) -> Vec<(usize, usize, f64)> {
    let w = RankWeights::M6;
    let in_shown: HashSet<usize> = shown.iter().copied().filter(|&i| i < files.len()).collect();
    let mut d: HashMap<&str, usize> = HashMap::new();
    let mut shown_definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let mut seen = HashSet::new();
        for (name, _, _) in &f.defs {
            if seen.insert(name.as_str()) {
                *d.entry(name.as_str()).or_default() += 1;
                if in_shown.contains(&i) {
                    shown_definers.entry(name.as_str()).or_default().push(i);
                }
            }
        }
    }
    let mut sum: HashMap<(usize, usize), f64> = HashMap::new();
    let mut src: Vec<usize> = in_shown.iter().copied().collect();
    src.sort_unstable();
    for a in src {
        for (name, c) in agg_refs(&files[a], w) {
            let Some(defs) = shown_definers.get(name) else {
                continue;
            };
            let w = ref_weight(name, c, d[name]);
            for &b in defs.iter().filter(|&&b| b != a) {
                *sum.entry((a, b)).or_default() += w;
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

/// Доли символов файла по входящему потоку `inflow[j]` каждого определения (поток имени делится
/// поровну между `same[j]` одноимёнными определениями); символам без входящих ссылок —
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

/// [`rank`] с правилами весов `w` — справочная абляция замера (M6 §12а); продукт — [`rank`].
pub fn rank_with(files: &[RankFile], focus: Option<&Focus>, w: RankWeights) -> Ranked {
    let g = Graph::with_weights(files, w);
    let restart = focus.and_then(|f| focus_restart(files, f));
    let file_rank = rank_on_graph(&g, restart.as_deref());
    let mut flow = vec![0.0; g.names()];
    g.flows(&file_rank, &mut flow);
    let own: HashMap<(usize, usize), f64> = g
        .self_refs
        .iter()
        .map(|e| ((e.src, e.name), file_rank[e.src] * e.wq))
        .collect();
    let mut count = vec![0.0f64; g.names()];
    let symbol_share = files
        .iter()
        .zip(&g.def_ids)
        .enumerate()
        .map(|(i, (f, ids))| {
            ids.iter().for_each(|&id| count[id] += 1.0);
            let inflow: Vec<f64> = ids
                .iter()
                .map(|&id| {
                    let total = flow[id];
                    let x = total - own.get(&(i, id)).copied().unwrap_or(0.0);
                    if x > total * FLOW_EPS { x } else { 0.0 }
                })
                .collect();
            let same: Vec<f64> = ids.iter().map(|&id| count[id]).collect();
            ids.iter().for_each(|&id| count[id] = 0.0);
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
