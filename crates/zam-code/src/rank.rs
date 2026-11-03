//! Ранжирование карты кода (спецификация M5.6 §7): граф файлов, PageRank, ранги символов.
//!
//! Алгоритм — по образцу repo map проекта Aider (<https://github.com/Aider-AI/aider>,
//! `aider/repomap.py`, © Paul Gauthier, лицензия Apache-2.0): граф «файл ссылается на имя,
//! определённое в другом файле», веса √count × множитель имени / число определяющих файлов,
//! PageRank с персонализацией. Код не заимствован — переписан по описанию алгоритма; отличия:
//! доли символов считаются потоком ранга по имени, а не распределением по рёбрам, фокус — только
//! вектор перезапуска (без ×10 упомянутым именам).
//!
//! Функции чистые: сервер считает их в `spawn_blocking`, сохраняет `edges` и доли и потом
//! персонализирует по сохранённым рёбрам ([`rank_on_edges`]) без пересборки графа.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::SymbolKind;

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

/// Файл как вход ранжирования: определения и агрегированные ссылки по имени.
#[derive(Debug, Clone, PartialEq)]
pub struct RankFile {
    /// Идентификатор строки `code_files` (для сервера; ранжирование работает по индексу).
    pub id: i64,
    pub path: String,
    /// `(name, kind, exported)` в порядке символов файла.
    pub defs: Vec<(String, SymbolKind, Option<bool>)>,
    /// `(name, count)`: число ссылок файла на имя.
    pub refs: Vec<(String, u32)>,
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
    /// Рёбра графа файлов `(src, dst, weight)`, по возрастанию `(src, dst)`.
    pub edges: Vec<(usize, usize, f64)>,
}

/// Множитель имени: ×10 — длинное (≥ 8) имя в snake/camel-регистре, ×0,1 — имя с `_` в начале,
/// ×0,1 — имя определено больше чем в 5 файлах (общее слово). Множители перемножаются.
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

/// Вклад одной ссылки: `(src, dst, name, weight)`.
type NamedEdge<'a> = (usize, usize, &'a str, f64);

/// Рёбра по именам: для ссылки `(name, count)` файла A и каждого B ≠ A, где имя определено, —
/// `√count × ident_mult(name, d) / d`, где d — число файлов с определением имени.
fn named_edges(files: &[RankFile]) -> Vec<NamedEdge<'_>> {
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for (name, _, _) in &f.defs {
            let list = definers.entry(name.as_str()).or_default();
            if list.last() != Some(&i) {
                list.push(i);
            }
        }
    }
    let mut out = Vec::new();
    for (a, f) in files.iter().enumerate() {
        for (name, count) in &f.refs {
            let Some(defs) = definers.get(name.as_str()) else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            let d = defs.len();
            let w = f64::from(*count).sqrt() * ident_mult(name, d) / d as f64;
            for &b in defs.iter().filter(|&&b| b != a) {
                out.push((a, b, name.as_str(), w));
            }
        }
    }
    out
}

/// Граф файлов: рёбра A → B с суммарным весом ссылок по именам, по `(src, dst)`.
pub fn file_edges(files: &[RankFile]) -> Vec<(usize, usize, f64)> {
    let mut sum: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (a, b, _, w) in named_edges(files) {
        *sum.entry((a, b)).or_default() += w;
    }
    sum.into_iter().map(|((a, b), w)| (a, b, w)).collect()
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

/// Годные рёбра: индексы в пределах, вес конечный и положительный.
fn valid_edges(n: usize, edges: &[(usize, usize, f64)]) -> Vec<(usize, usize, f64)> {
    edges
        .iter()
        .copied()
        .filter(|&(s, d, w)| s < n && d < n && w.is_finite() && w > 0.0)
        .collect()
}

/// PageRank степенным методом: затухание 0,85, ≤ 50 итераций, остановка по L1 < 1e-6. Масса
/// висячих узлов (без исходящих рёбер) уходит по вектору перезапуска (`restart` или равномерно).
/// Мусорные рёбра (индекс вне `n`, вес ≤ 0 или не конечный) отбрасываются. Сумма итога = 1.
pub fn pagerank(n: usize, edges: &[(usize, usize, f64)], restart: Option<&[f64]>) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let p = restart_vector(n, restart);
    let edges = valid_edges(n, edges);
    let mut out = vec![0.0; n];
    for &(s, _, w) in &edges {
        out[s] += w;
    }
    let mut r = vec![1.0 / n as f64; n];
    for _ in 0..MAX_ITER {
        let dangling: f64 = (0..n).filter(|&i| out[i] == 0.0).map(|i| r[i]).sum();
        let base = (1.0 - DAMPING) + DAMPING * dangling;
        let mut next: Vec<f64> = p.iter().map(|pi| base * pi).collect();
        for &(s, d, w) in &edges {
            next[d] += DAMPING * r[s] * w / out[s];
        }
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

/// PageRank по сохранённым рёбрам (персонализация на сервере без пересборки графа).
pub fn rank_on_edges(n: usize, edges: &[(usize, usize, f64)], restart: Option<&[f64]>) -> Vec<f64> {
    pagerank(n, edges, restart)
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

/// Масса перезапуска фокуса: 1 файлам из `paths` и файлам с определениями `idents` и имён из
/// `focus_idents(text)` (точное совпадение). Нет совпадений — `None` (глобальный ранг).
pub fn focus_restart(files: &[RankFile], f: &Focus) -> Option<Vec<f64>> {
    let paths: HashSet<&str> = f.paths.iter().map(String::as_str).collect();
    let text_idents = f.text.as_deref().map(focus_idents).unwrap_or_default();
    let idents: HashSet<&str> = f
        .idents
        .iter()
        .chain(&text_idents)
        .map(String::as_str)
        .collect();
    let mass: Vec<f64> = files
        .iter()
        .map(|file| {
            let hit = paths.contains(file.path.as_str())
                || file
                    .defs
                    .iter()
                    .any(|(n, _, _)| idents.contains(n.as_str()));
            if hit { 1.0 } else { 0.0 }
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

/// Доли символов файла: входящий поток ранга по имени (делится поровну между одноимёнными
/// определениями); символам без входящих ссылок — [`UNREFERENCED_SHARE`] по весу вида, или всё,
/// если входящих ссылок у файла нет.
fn shares(file: &RankFile, inflow: Option<&HashMap<&str, f64>>) -> Vec<f64> {
    let flow = |name: &str| inflow.and_then(|m| m.get(name)).copied().unwrap_or(0.0);
    let mut same_name: HashMap<&str, usize> = HashMap::new();
    for (n, _, _) in &file.defs {
        *same_name.entry(n.as_str()).or_default() += 1;
    }
    let ref_total: f64 = same_name.keys().map(|n| flow(n)).sum();
    let cold_total: f64 = file
        .defs
        .iter()
        .filter(|(n, _, _)| flow(n) <= 0.0)
        .map(|(_, k, e)| kind_weight(*k, *e))
        .sum();
    let (hot_part, cold_part) = match (ref_total > 0.0, cold_total > 0.0) {
        (true, true) => (1.0 - UNREFERENCED_SHARE, UNREFERENCED_SHARE),
        (true, false) => (1.0, 0.0),
        _ => (0.0, 1.0),
    };
    file.defs
        .iter()
        .map(|(n, k, e)| {
            let f = flow(n);
            if f > 0.0 {
                hot_part * f / ref_total / same_name[n.as_str()] as f64
            } else if cold_total > 0.0 {
                cold_part * kind_weight(*k, *e) / cold_total
            } else {
                0.0
            }
        })
        .collect()
}

/// Ранжирование: граф файлов, PageRank (с персонализацией по `focus`, если есть совпадения),
/// доли символов по входящему потоку ранга `r[src] × w / out(src)` через имя.
pub fn rank(files: &[RankFile], focus: Option<&Focus>) -> Ranked {
    let n = files.len();
    let named = named_edges(files);
    let edges = file_edges(files);
    let restart = focus.and_then(|f| focus_restart(files, f));
    let file_rank = pagerank(n, &edges, restart.as_deref());
    let mut out = vec![0.0; n];
    for &(s, _, w) in &edges {
        out[s] += w;
    }
    let mut inflow: HashMap<usize, HashMap<&str, f64>> = HashMap::new();
    for (s, d, name, w) in named {
        if out[s] > 0.0 {
            *inflow.entry(d).or_default().entry(name).or_default() += file_rank[s] * w / out[s];
        }
    }
    let symbol_share = files
        .iter()
        .enumerate()
        .map(|(i, f)| shares(f, inflow.get(&i)))
        .collect();
    Ranked {
        file_rank,
        symbol_share,
        edges,
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
