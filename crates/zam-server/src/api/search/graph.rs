//! `expand=graph` (§8.5, §9.2 п. 7): соседи первых `top_k` элементов выдачи по графу.
//!
//! - Сосед — узел или запись на 1 шаг по видимому ребру в любую сторону, в пределах области
//!   цели элемента (база и её пространства: участнику общего решения — только пространство
//!   решения; Human — только администратору с `include_human`), с фильтрами запроса (виды,
//!   статусы, `certainty`, область, префикс пути): `superseded`/`retracted` — только с
//!   `include_history`. Уже выданные элементы соседями не становятся.
//! - Балл соседа — балл родителя × `search.graph_weight` (0,5); у соседа нескольких родителей
//!   — лучший. Карточка — первый лист соседа; `sources: ["graph"]`, `via: {type, from}`
//!   (тип ребра и родитель).
//! - Добавляется не больше `ceil(top_k / 2)` соседей: сначала действующие insight
//!   (`active`/`revisit`) по `certainty` (`axiom`, `theorem`, `hypothesis`, без статуса), затем
//!   заметки, затем остальные записи; внутри группы — по баллу.
//! - Сбой чтения базы чужого решения исключает решение ([`Health::soft`]).
use std::collections::{BTreeMap, HashMap, HashSet};

use zam_core::{Id, TargetKind, TargetRef};
use zam_search::order_contradicted;
use zam_store::{
    Certainty, ChunkScope, EdgeType, EntryStatus, FtsHit, GraphStore, TargetFilter, count_tokens,
    first_leaves, live_chunks, text_snippet,
};

use super::{Health, Item};
use crate::error::ApiError;

/// Группа соседа в порядке выдачи: действующий insight, заметка, прочее.
fn group(h: &FtsHit) -> u8 {
    match (&h.entry, h.target.kind) {
        (Some(e), _) if e.kind == "insight" => match e.status.as_deref() {
            Some("active" | "revisit") => 0,
            _ => 2,
        },
        (None, TargetKind::Node) => 1,
        // прочие записи; вложение соседом не бывает (концом связи не является) — тоже в конце
        (Some(_), _) | (None, TargetKind::Entry | TargetKind::Attachment) => 2,
    }
}

/// Цель элемента может быть концом связи графа: узел или запись, не документ вложения.
fn in_graph(h: &FtsHit) -> bool {
    matches!(h.target.kind, TargetKind::Node | TargetKind::Entry)
}

/// Порядок `certainty`: `axiom`, `theorem`, `hypothesis`, без статуса.
fn certainty_rank(c: Option<&str>) -> u8 {
    match c {
        Some("axiom") => 0,
        Some("theorem") => 1,
        Some("hypothesis") => 2,
        _ => 3,
    }
}

/// Сосед-кандидат.
struct Cand {
    target: usize,
    hit: FtsHit,
    score: f64,
    via: (EdgeType, TargetRef),
}

/// Соседи первых `top_k` элементов `items` (см. модуль). `with_text` — режим `chunks`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn expand_graph(
    health: &mut Health<'_>,
    items: &[Item],
    top_k: usize,
    weight: f64,
    kinds: &TargetFilter,
    prefix: Option<String>,
    needles: &[&str],
    with_text: bool,
) -> Result<Vec<Item>, ApiError> {
    let targets = health.targets;
    let seen: HashSet<TargetRef> = items.iter().map(|it| it.hit.target).collect();
    // родители по базам: цель → лучший балл (документ вложения связей не имеет)
    let mut parents: BTreeMap<usize, HashMap<TargetRef, f64>> = BTreeMap::new();
    for it in items.iter().take(top_k).filter(|it| in_graph(&it.hit)) {
        let s = parents
            .entry(it.target)
            .or_default()
            .entry(it.hit.target)
            .or_insert(it.score);
        *s = s.max(it.score);
    }
    let mut cands: Vec<Cand> = Vec::new();
    for (ti, starts) in parents {
        if !health.alive[ti] {
            continue;
        }
        let t = &targets[ti];
        let spaces = t.space_ids();
        let refs: Vec<TargetRef> = starts.keys().copied().collect();
        let r = t.db.neighbors(&refs, 1, &[], &spaces).await;
        let Some(g) = health.soft(ti, r)? else {
            continue;
        };
        // сосед → (балл, тип ребра, родитель)
        let mut best: HashMap<TargetRef, (f64, EdgeType, TargetRef)> = HashMap::new();
        for e in &g.edges {
            let Some(dst) = e.dst else {
                continue;
            };
            for (parent, n) in [(e.src, dst), (dst, e.src)] {
                let Some(&ps) = starts.get(&parent) else {
                    continue;
                };
                if seen.contains(&n) {
                    continue;
                }
                let s = ps * weight;
                let slot = best.entry(n).or_insert((s, e.kind, parent));
                if s > slot.0 {
                    *slot = (s, e.kind, parent);
                }
            }
        }
        if best.is_empty() {
            continue;
        }
        let r = first_leaves(&t.db, best.keys().copied().collect()).await;
        let Some(leaves) = health.soft(ti, r)? else {
            continue;
        };
        let scope = ChunkScope {
            space_ids: spaces,
            path_prefix: prefix.clone(),
            indexable_only: false,
            targets: kinds.clone(),
        };
        let r = live_chunks(&t.db, leaves.into_values().collect(), scope).await;
        let Some(hits) = health.soft(ti, r)? else {
            continue;
        };
        for hit in hits {
            if let Some(&(score, kind, from)) = best.get(&hit.target) {
                cands.push(Cand {
                    target: ti,
                    hit,
                    score,
                    via: (kind, from),
                });
            }
        }
    }
    // элементы исключённых на этом шаге баз — прочь
    cands.retain(|c| health.alive[c.target]);
    cands.sort_by(|a, b| {
        (group(&a.hit), certainty_rank(a.hit.certainty.as_deref()))
            .cmp(&(group(&b.hit), certainty_rank(b.hit.certainty.as_deref())))
            .then(b.score.total_cmp(&a.score))
            .then(a.hit.target.cmp(&b.hit.target))
    });
    cands.truncate(top_k.div_ceil(2));
    Ok(cands
        .into_iter()
        .map(|c| {
            let mut hit = c.hit;
            if hit.snippet.is_empty() {
                hit.snippet = text_snippet(&hit.text, needles);
            }
            let text = with_text.then(|| (hit.text.clone(), count_tokens(&hit.text)));
            Item {
                target: c.target,
                chunk_ids: vec![hit.chunk_id.clone()],
                hit,
                score: c.score,
                sources: vec!["graph"],
                l0: None,
                text,
                rerank_score: None,
                via: Some(c.via),
                folder: None,
                seen: false,
                seen_kept: false,
                in_cache: false,
                stale_refs: Vec::new(),
            }
        })
        .collect())
}

/// Надёжность для порядка по `contradicts`: `axiom` 3, `theorem` 2, `hypothesis` 1, без
/// `certainty` (заметка, term, task…) 0 — ниже гипотезы. Та же шкала — в проверке набора eval
/// (`xtask/src/eval/insights.rs::reliability`).
fn reliability(c: Option<Certainty>) -> u8 {
    match c {
        Some(Certainty::Axiom) => 3,
        Some(Certainty::Theorem) => 2,
        Some(Certainty::Hypothesis) => 1,
        None => 0,
    }
}

/// Пара `contradicts` переставляется: цель действует, это `theorem` или `axiom` и она строго
/// надёжнее источника ([`reliability`]): hypothesis → theorem, hypothesis → axiom, theorem →
/// axiom, заметка → theorem/axiom — да; axiom → axiom, theorem → theorem — нет (до задачи 15
/// M4.5 пара «аксиома → аксиома» переставлялась).
pub(super) fn contradicts_applies(src_rel: u8, dst_rel: u8, dst_live: bool) -> bool {
    dst_live && dst_rel >= reliability(Some(Certainty::Theorem)) && dst_rel > src_rel
}

/// Надёжность ([`reliability`]) и «действует» узлов подграфа: у заметки статуса нет, запись
/// действует при `active`/`revisit`; удалённое не действует.
fn reliabilities(g: &zam_store::Graph) -> HashMap<Id, (u8, bool)> {
    g.nodes
        .iter()
        .map(|n| {
            let live = !n.deleted
                && matches!(
                    n.status,
                    None | Some(EntryStatus::Active | EntryStatus::Revisit)
                );
            (n.r.id, (reliability(n.certainty), live))
        })
        .collect()
}

/// Кандидат поиска для [`kind_boost_exempt`]: индекс цели поиска, цель элемента и получил бы
/// он буст вида.
pub(super) struct KindCandidate {
    pub target: usize,
    pub r: TargetRef,
    pub boosted: bool,
}

/// Предупреждение ответа поиска: граф `contradicts` элементов выдачи обрезан пределом подграфа —
/// исключения буста вида `unless_contradicted` ([`kind_boost_exempt`]) и порядок по
/// противоречиям ([`order_by_contradicts`]) могли примениться не полностью.
pub const WARN_KIND_GRAPH_TRUNCATED: &str = "contradicts graph truncated";

/// Граф `contradicts` глубины 1 от `refs` в базе цели `ti` — одно чтение на шаг поиска
/// ([`kind_boost_exempt`], [`order_by_contradicts`]) с общими правилами (ревью задач 10 и 16):
/// - сбой чтения графа **чужого** решения — `None` и WARN в журнал: решение из выдачи не
///   исключается (в отличие от [`Health::soft`]), шаг для него просто не применяется; сбой
///   личной базы — ошибка запроса;
/// - подграф обрезан пределами `zam_store::GRAPH_MAX_NODES` / `GRAPH_MAX_EDGES` — WARN в журнал
///   и предупреждение ответа [`WARN_KIND_GRAPH_TRUNCATED`] (одно на ответ); найденные связи
///   действуют.
async fn contradicts_graph(
    health: &mut Health<'_>,
    ti: usize,
    refs: &[TargetRef],
) -> Result<Option<zam_store::Graph>, ApiError> {
    let t = &health.targets[ti];
    let r =
        t.db.neighbors(refs, 1, &[EdgeType::Contradicts], &t.space_ids())
            .await;
    let g = match r {
        Ok(g) => g,
        Err(e) => match &t.solution {
            Some(_) => {
                tracing::warn!(
                    error = %e,
                    "поиск: граф contradicts чужого решения не прочитан — шаг пропущен"
                );
                return Ok(None);
            }
            None => return Err(e.into()),
        },
    };
    if g.truncated {
        tracing::warn!(
            nodes = g.nodes.len(),
            edges = g.edges.len(),
            "поиск: граф contradicts обрезан пределом — связи учтены не все"
        );
        if !health
            .warnings
            .iter()
            .any(|w| w == WARN_KIND_GRAPH_TRUNCATED)
        {
            health.warnings.push(WARN_KIND_GRAPH_TRUNCATED.into());
        }
    }
    Ok(Some(g))
}

/// Цели без буста вида при `search.kind_boost_mode = unless_contradicted` (M6 §10): кандидат с
/// бустом вида, связанный `contradicts` (в любую сторону **[решение исполнителя]**: связь —
/// противоречие, направление в данных — от гипотезы к теореме) с действующей записью, которая
/// надёжнее его по [`contradicts_applies`] и тоже среди кандидатов `cands` — всех после слияния,
/// до отсечения `top_k` (не только видимых). Граф читается от кандидатов с бустом.
///
/// - Сбой чтения графа **чужого** решения решение не исключает (ревью задачи 10): исключений у
///   его кандидатов нет — буст как при `always`, в журнал — WARN. Сбой личной базы — ошибка
///   запроса, как у остальных чтений.
/// - Подграф обрезан пределами `zam_store::GRAPH_MAX_NODES` / `GRAPH_MAX_EDGES` (`truncated`):
///   связь сверх предела не видна, и исключение по ней не применяется — найденные исключения
///   действуют, в журнал — WARN, в ответ — предупреждение [`WARN_KIND_GRAPH_TRUNCATED`].
///
/// Оба правила — в [`contradicts_graph`], общем с [`order_by_contradicts`].
pub(super) async fn kind_boost_exempt(
    health: &mut Health<'_>,
    cands: &[KindCandidate],
) -> Result<HashSet<TargetRef>, ApiError> {
    let mut by_db: BTreeMap<usize, (Vec<TargetRef>, HashSet<TargetRef>)> = BTreeMap::new();
    for c in cands {
        if !health.alive[c.target] || !matches!(c.r.kind, TargetKind::Node | TargetKind::Entry) {
            continue;
        }
        let (boosted, all) = by_db.entry(c.target).or_default();
        all.insert(c.r);
        if c.boosted && !boosted.contains(&c.r) {
            boosted.push(c.r);
        }
    }
    let mut out = HashSet::new();
    for (ti, (boosted, all)) in by_db {
        if boosted.is_empty() || all.len() < 2 {
            continue;
        }
        // чужое решение с нечитаемым графом остаётся в выдаче: буст вида — как при `always`
        let Some(g) = contradicts_graph(health, ti, &boosted).await? else {
            continue;
        };
        let rel = reliabilities(&g);
        for e in &g.edges {
            let Some(d) = e.dst.filter(|_| e.kind == EdgeType::Contradicts) else {
                continue;
            };
            for (x, y) in [(e.src, d), (d, e.src)] {
                let xr = rel.get(&x.id).map_or(0, |r| r.0);
                if boosted.contains(&x)
                    && all.contains(&y)
                    && let Some(&(yr, live)) = rel.get(&y.id)
                    && contradicts_applies(xr, yr, live)
                {
                    out.insert(x);
                }
            }
        }
    }
    Ok(out)
}

/// Порядок по `contradicts` (M4.5, §5.2): элемент выдачи с исходящим `contradicts` к действующей
/// записи той же выдачи, которая надёжнее его (`axiom` — любого элемента, кроме другой
/// аксиомы; `theorem` — гипотезы или заметки), не стоит выше неё
/// ([`zam_search::order_contradicted`]); задача 15: запрос `r-08` набора eval — гипотеза против
/// `theorem`. Сдвинутой записи балл поднимается до балла следующего элемента, чтобы баллы не шли
/// вразрез с порядком.
/// Сбой чтения графа чужого решения решение не исключает: его элементы остаются без
/// перестановки; обрезанный подграф — предупреждение ответа ([`contradicts_graph`]).
pub(super) async fn order_by_contradicts(
    health: &mut Health<'_>,
    items: &mut Vec<Item>,
) -> Result<(), ApiError> {
    let mut by_db: BTreeMap<usize, Vec<TargetRef>> = BTreeMap::new();
    for it in items.iter() {
        // связи — между узлами и записями: документ вложения концом связи не бывает
        if it.folder.is_none() && health.alive[it.target] && in_graph(&it.hit) {
            let refs = by_db.entry(it.target).or_default();
            if !refs.contains(&it.hit.target) {
                refs.push(it.hit.target);
            }
        }
    }
    let mut pairs: Vec<(Id, Id)> = Vec::new();
    for (ti, refs) in by_db {
        if refs.len() < 2 {
            continue;
        }
        let Some(g) = contradicts_graph(health, ti, &refs).await? else {
            continue;
        };
        let rel = reliabilities(&g);
        for e in &g.edges {
            if e.kind != EdgeType::Contradicts {
                continue;
            }
            let src = rel.get(&e.src.id).map_or(0, |r| r.0);
            if let Some(d) = e.dst
                && let Some(&(dst, live)) = rel.get(&d.id)
                && contradicts_applies(src, dst, live)
                && refs.contains(&e.src)
                && refs.contains(&d)
            {
                pairs.push((e.src.id, d.id));
            }
        }
    }
    if pairs.is_empty() {
        return Ok(());
    }
    if order_contradicted(items, |it| it.hit.target.id, &pairs) > 0 {
        let moved: HashSet<Id> = pairs.iter().map(|p| p.1).collect();
        for i in (0..items.len().saturating_sub(1)).rev() {
            let next = items[i + 1].score;
            if moved.contains(&items[i].hit.target.id) && items[i].score < next {
                items[i].score = next;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contradicts_rule_table() {
        let r = |c: Option<Certainty>| reliability(c);
        let (h, t, a) = (
            r(Some(Certainty::Hypothesis)),
            r(Some(Certainty::Theorem)),
            r(Some(Certainty::Axiom)),
        );
        let note = r(None);
        // (источник, цель, цель действует, переставляется)
        let cases = [
            ("hypothesis → theorem", h, t, true, true),
            ("hypothesis → axiom", h, a, true, true),
            ("theorem → axiom", t, a, true, true),
            ("заметка → theorem", note, t, true, true),
            ("заметка → axiom", note, a, true, true),
            ("axiom → axiom", a, a, true, false),
            ("theorem → theorem", t, t, true, false),
            ("axiom → theorem", a, t, true, false),
            ("theorem → hypothesis", t, h, true, false),
            ("заметка → hypothesis", note, h, true, false),
            (
                "hypothesis → замещённая/отозванная axiom",
                h,
                a,
                false,
                false,
            ),
            (
                "hypothesis → замещённая/отозванная theorem",
                h,
                t,
                false,
                false,
            ),
        ];
        for (name, src, dst, live, want) in cases {
            assert_eq!(contradicts_applies(src, dst, live), want, "{name}");
        }
    }

    #[test]
    fn certainty_order() {
        let mut v = vec![None, Some("hypothesis"), Some("axiom"), Some("theorem")];
        v.sort_by_key(|c| certainty_rank(*c));
        assert_eq!(
            v,
            vec![Some("axiom"), Some("theorem"), Some("hypothesis"), None]
        );
    }
}
