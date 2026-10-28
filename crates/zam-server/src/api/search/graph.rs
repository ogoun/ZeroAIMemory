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
        _ => 2,
    }
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
    // родители по базам: цель → лучший балл
    let mut parents: BTreeMap<usize, HashMap<TargetRef, f64>> = BTreeMap::new();
    for it in items.iter().take(top_k) {
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
            }
        })
        .collect())
}

/// Порядок по `contradicts` (M4.5, §5.2): записи выдачи с исходящим `contradicts` к действующей
/// аксиоме той же выдачи не стоят выше неё ([`zam_search::order_contradicted`]); сдвинутой
/// аксиоме балл поднимается до балла следующего элемента, чтобы баллы не шли вразрез с порядком.
/// Сбой чтения графа чужого решения исключает решение ([`Health::soft`]).
pub(super) async fn order_by_contradicts(
    health: &mut Health<'_>,
    items: &mut Vec<Item>,
) -> Result<(), ApiError> {
    let targets = health.targets;
    let mut by_db: BTreeMap<usize, Vec<TargetRef>> = BTreeMap::new();
    for it in items.iter() {
        if it.folder.is_none() && health.alive[it.target] {
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
        let t = &targets[ti];
        let r =
            t.db.neighbors(&refs, 1, &[EdgeType::Contradicts], &t.space_ids())
                .await;
        let Some(g) = health.soft(ti, r)? else {
            continue;
        };
        let axioms: HashSet<Id> = g
            .nodes
            .iter()
            .filter(|n| {
                n.certainty == Some(Certainty::Axiom)
                    && !n.deleted
                    // у заметки статуса нет; у записи — только действующая
                    && matches!(
                        n.status,
                        None | Some(EntryStatus::Active | EntryStatus::Revisit)
                    )
            })
            .map(|n| n.r.id)
            .collect();
        for e in &g.edges {
            if e.kind != EdgeType::Contradicts {
                continue;
            }
            if let Some(d) = e.dst
                && axioms.contains(&d.id)
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
    fn certainty_order() {
        let mut v = vec![None, Some("hypothesis"), Some("axiom"), Some("theorem")];
        v.sort_by_key(|c| certainty_rank(*c));
        assert_eq!(
            v,
            vec![Some("axiom"), Some("theorem"), Some("hypothesis"), None]
        );
    }
}
