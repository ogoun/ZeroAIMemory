//! Виды внутренних работ и типы предложений человеку (M7.3 §6 «Общее», §6.3 п. 1, §6.7 п. 1;
//! миграция 0027).
//!
//! В схеме у `works.kind` и `proposals.type` CHECK нет: допустимые значения — эти перечисления.
//! Строку неизвестного значения читатель пропускает с WARN ([`WorkKind::parse`] — `None`), так
//! виды уборки M7.5 добавятся без миграции. Тестовый вид `echo` есть только с фичей
//! `test-hooks` (решение координатора 73): `test` занят видом строки журнала отправок проверки
//! подключения (§6.2 п. 9).

use serde::{Deserialize, Serialize};

/// Вид внутренней работы (`works.kind`, настройка `works.kind.<вид>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkKind {
    /// Пробные вопросы ночной проверки (§6.4 п. 2).
    Probes,
    /// Кандидаты в Rules (§6.7 п. 3).
    Rules,
    /// Тестовый вид: пакет и итог без смысла вида (только `test-hooks`).
    #[cfg(feature = "test-hooks")]
    Echo,
}

impl WorkKind {
    /// Все виды в порядке ночи (пробы, затем кандидаты в Rules; §6.3 п. 2).
    #[cfg(not(feature = "test-hooks"))]
    pub const ALL: &'static [WorkKind] = &[WorkKind::Probes, WorkKind::Rules];
    /// Все виды в порядке ночи (пробы, затем кандидаты в Rules; §6.3 п. 2) и тестовый `echo`.
    #[cfg(feature = "test-hooks")]
    pub const ALL: &'static [WorkKind] = &[WorkKind::Probes, WorkKind::Rules, WorkKind::Echo];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkKind::Probes => "probes",
            WorkKind::Rules => "rules",
            #[cfg(feature = "test-hooks")]
            WorkKind::Echo => "echo",
        }
    }

    /// Вид по строке; неизвестная строка (в том числе `tidy` до M7.5 и тип предложения
    /// `rule`) — `None`.
    pub fn parse(s: &str) -> Option<WorkKind> {
        WorkKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// Тип предложения человеку (`proposals.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalType {
    /// Кандидат в Rules: `payload {path, content}` (§6.7 п. 1).
    Rule,
}

impl ProposalType {
    pub const ALL: &'static [ProposalType] = &[ProposalType::Rule];

    pub fn as_str(self) -> &'static str {
        match self {
            ProposalType::Rule => "rule",
        }
    }

    /// Тип по строке; неизвестная — `None`.
    pub fn parse(s: &str) -> Option<ProposalType> {
        ProposalType::ALL.iter().copied().find(|t| t.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse` и `as_str` взаимно обратны; чужие строки (`tidy` до M7.5, тип предложения `rule`,
    /// вид журнала проверки подключения `test`) — `None`; serde — та же строка.
    #[test]
    fn work_kind_parse_cases() {
        for k in WorkKind::ALL {
            assert_eq!(WorkKind::parse(k.as_str()), Some(*k));
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::json!(k.as_str())
            );
        }
        assert_eq!(WorkKind::parse("probes"), Some(WorkKind::Probes));
        assert_eq!(WorkKind::parse("rules"), Some(WorkKind::Rules));
        for bad in ["tidy", "rule", "test", "", "Probes", " probes"] {
            assert_eq!(WorkKind::parse(bad), None, "{bad}");
        }
        for t in ProposalType::ALL {
            assert_eq!(ProposalType::parse(t.as_str()), Some(*t));
        }
        assert_eq!(ProposalType::parse("rule"), Some(ProposalType::Rule));
        for bad in ["rules", "tidy", ""] {
            assert_eq!(ProposalType::parse(bad), None, "{bad}");
        }
    }
}
