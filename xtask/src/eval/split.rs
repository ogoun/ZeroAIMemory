//! Разбиение сценариев на dev и holdout (спецификация M6 §3.5, решение составителя плана 6).
//!
//! Сценарии ТС-1 и сессии ТС-2 делятся на dev (⅔) и holdout (⅓) с расслоением по виду: внутри
//! каждого вида id сортируются по `blake3(SPLIT_SALT ‖ id)`, первые ⌈n/3⌉ — holdout. Соль
//! фиксирована: принадлежность сценария определяют только id его вида (порядок файла и другие
//! виды не влияют). Сетки подбираются на dev, приёмка — только на holdout (`--grid` с
//! `--split holdout` — ошибка, [`crate::eval::grid::refuse_holdout`]).

use std::collections::{BTreeMap, BTreeSet};

/// Соль разбиения.
pub const SPLIT_SALT: &str = "m6-2026-11-04";

/// Часть набора прогона.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Dev,
    Holdout,
}

impl Part {
    pub fn as_str(self) -> &'static str {
        match self {
            Part::Dev => "dev",
            Part::Holdout => "holdout",
        }
    }
}

/// Ключ сортировки id внутри вида.
fn key(id: &str) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    h.update(SPLIT_SALT.as_bytes());
    h.update(id.as_bytes());
    h.finalize()
}

/// id holdout из пар `(id, вид)`: в каждом виде первые ⌈n/3⌉ по [`key`].
pub fn split(items: &[(String, String)]) -> BTreeSet<String> {
    let mut by_kind: BTreeMap<&str, Vec<(blake3::Hash, &str)>> = BTreeMap::new();
    for (id, kind) in items {
        by_kind.entry(kind).or_default().push((key(id), id));
    }
    let mut out = BTreeSet::new();
    for mut v in by_kind.into_values() {
        v.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(b.1)));
        let n = v.len().div_ceil(3);
        out.extend(v.into_iter().take(n).map(|(_, id)| id.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Три вида по 30 id — в holdout ровно по 10 каждого вида; повтор — тот же результат.
    #[test]
    fn split_is_third_per_kind() {
        let mut items = Vec::new();
        for kind in ["long", "insight", "multi"] {
            for i in 0..30 {
                items.push((format!("{kind}-{i:02}"), kind.to_string()));
            }
        }
        let h = split(&items);
        assert_eq!(h.len(), 30);
        for kind in ["long", "insight", "multi"] {
            let n = h.iter().filter(|id| id.starts_with(kind)).count();
            assert_eq!(n, 10, "{kind}");
        }
        assert_eq!(split(&items), h);
        // ⌈n/3⌉: из 4 — 2, из 1 — 1
        let small: Vec<(String, String)> =
            (0..4).map(|i| (format!("s{i}"), "k".to_string())).collect();
        assert_eq!(split(&small).len(), 2);
        assert_eq!(split(&small[..1]).len(), 1);
        assert_eq!(SPLIT_SALT, "m6-2026-11-04");
    }
}
