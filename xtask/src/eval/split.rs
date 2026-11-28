//! Разбиение сценариев на dev и holdout (спецификация M6 §3.5, решение составителя плана 6).
//!
//! Сценарии ТС-1 и сессии ТС-2 делятся на dev (⅔) и holdout (⅓) с расслоением по виду: внутри
//! каждого вида id сортируются по `blake3(SPLIT_SALT ‖ id)`, первые ⌈n/3⌉ — holdout. Соль
//! фиксирована: принадлежность сценария определяют только id его вида (порядок файла и другие
//! виды не влияют). Сетки подбираются на dev, приёмка — только на holdout (`--grid` с
//! `--split holdout` — ошибка, [`crate::eval::grid::refuse_holdout`]).
//!
//! С M7.3 (§6.4 п. 2) функция разбиения — `zam_search::eval::split_by_rank` (рядом с
//! поэлементным разбиением проб ночной проверки сервера); здесь — прежнее имя [`split`] и часть
//! прогона [`Part`] с разбором флага `--split`. Поведение прежнее — сторож
//! `split_by_rank_unchanged`.

pub use zam_search::eval::split_by_rank as split;

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
        assert_eq!(zam_search::eval::SPLIT_SALT, "m6-2026-11-04");
    }

    /// Holdout ТС-1 (сценарии `eval/scenarios.toml`, по виду сценария) — до переноса функции.
    const HOLDOUT_TS1: &[&str] = &[
        "s-07", "s-10", "s-11", "s-12", "s-13", "s-14", "s-19", "s-23", "s-24", "s-29", "s-30",
        "s-31", "s-35", "s-36", "s-41", "s-43", "s-44", "s-45",
    ];
    /// Holdout ТС-2 (сессии `eval/sessions.toml`, по профилю) — до переноса функции.
    const HOLDOUT_TS2: &[&str] = &[
        "sh-02", "sh-03", "ss-02", "ss-08", "ss-10", "ss-11", "ss-14", "ss-15",
    ];

    /// Сторож переноса (M7.3 §6.4 п. 2): ранговое разбиение переехало в `zam_search::eval` без
    /// изменения поведения — множества holdout ТС-1 и ТС-2 прежние.
    #[test]
    fn split_by_rank_unchanged() {
        let repo = crate::eval::tests_repo();
        let (sc, _) =
            crate::eval::tokens::load_scenarios(&repo.join("eval").join("scenarios.toml")).unwrap();
        let ts1: Vec<(String, String)> = sc
            .iter()
            .map(|s| (s.id.clone(), s.kind().to_string()))
            .collect();
        let set =
            crate::eval::sessions::load_set(&repo.join("eval").join("sessions.toml")).unwrap();
        let ts2: Vec<(String, String)> = set
            .iter()
            .map(|s| (s.id.clone(), s.profile.as_str().to_string()))
            .collect();
        let got1: Vec<String> = split(&ts1).into_iter().collect();
        let got2: Vec<String> = split(&ts2).into_iter().collect();
        assert_eq!(got1, HOLDOUT_TS1, "ТС-1: {got1:?}");
        assert_eq!(got2, HOLDOUT_TS2, "ТС-2: {got2:?}");
    }
}
