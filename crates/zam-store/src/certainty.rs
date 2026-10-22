//! Эпистемический статус `certainty` (§7.5 ТЗ): насколько записи или заметке можно доверять —
//! `axiom` (утверждено человеком), `theorem` (проверено, есть доказательство), `hypothesis`
//! (предположение). Ложный вывод в памяти не должен сбивать модели, поэтому права на статус
//! — матрица «агент / человек» ([`check_certainty`]).
//!
//! Проверка выполняется **внутри транзакции писателя** каждой операции хранилища, меняющей
//! запись журнала или заметку (создание, правка, PATCH, откат, замещение, отзыв, удаление,
//! восстановление, перенос, замена триггеров, вложение): текущий статус читается в той же
//! транзакции, поэтому параллельная правка не обходит правило, а новый путь записи через
//! хранилище не может его забыть **[решение исполнителя]**. Сервер лишь определяет автора
//! (`AuthorKind`) и отображает [`CertaintyError`] в 403/422.
//!
//! Матрица (агент — токен без флага `human`; человек — сессия WebUI или токен `human`;
//! `system` — внутренние операции, права как у агента):
//!
//! | Действие | Агент | Человек |
//! |---|---|---|
//! | создать `hypothesis` (или без статуса) | да | да |
//! | создать `theorem` | только с доказательством | да |
//! | создать `axiom` | 403 | да |
//! | `hypothesis`/нет → `theorem` | доказательство и причина | причина |
//! | `theorem` → `hypothesis` | причина | причина |
//! | `*` → `axiom` | 403 | причина |
//! | любое изменение `axiom` | 403 | да |
//! | снять статус (`→ null`) | только у `hypothesis`, с причиной | да, с причиной |
//! | нет → `hypothesis` | причина | причина |
//!
//! Каждая смена статуса — новая ревизия с причиной: у записи — `entry_revisions.reason`, у
//! заметки — сообщение ревизии (`message`).

use serde::Serialize;

use crate::entries::{Certainty, EntryKind};
use crate::nodes::AuthorKind;

/// Доказательство перевода в `theorem` (`data.evidence` записи или `evidence:` заметки):
/// что проверено, каким методом и с каким результатом — все три поля непустые.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub checked: String,
    pub method: String,
    pub result: String,
}

impl Evidence {
    /// Все три поля непустые (после обрезки пробелов).
    pub fn is_complete(&self) -> bool {
        [&self.checked, &self.method, &self.result]
            .iter()
            .all(|s| !s.trim().is_empty())
    }

    /// `data.evidence {checked, method, result}` записи журнала; поля не строки, пустые
    /// или отсутствуют — `None`.
    pub fn from_data(data: &serde_json::Value) -> Option<Evidence> {
        let ev = data.get("evidence")?;
        let field = |k: &str| {
            ev.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let e = Evidence {
            checked: field("checked")?,
            method: field("method")?,
            result: field("result")?,
        };
        e.is_complete().then_some(e)
    }

    /// `evidence:` во frontmatter заметки — свободный текст (скаляр или блок `|`): он и есть
    /// описание проверки, метода и результата, поэтому заполняет все три поля
    /// **[решение исполнителя]**. Пустой — `None`.
    pub fn from_note(text: Option<&str>) -> Option<Evidence> {
        let t = text.map(str::trim).filter(|t| !t.is_empty())?;
        Some(Evidence {
            checked: t.to_string(),
            method: t.to_string(),
            result: t.to_string(),
        })
    }
}

/// Что делает операция со статусом цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertaintyChange {
    /// Создание записи или заметки с этим статусом.
    Create(Option<Certainty>),
    /// Смена статуса существующей цели (`from != to`).
    Transition {
        from: Option<Certainty>,
        to: Option<Certainty>,
    },
    /// Любое другое изменение: правка содержимого, supersede, retract, delete, restore,
    /// move, триггеры, вложение.
    ModifyOther,
}

/// Нарушение матрицы прав.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CertaintyError {
    /// 403: создать `axiom`, перевести в неё или изменить её может только человек.
    #[error("axiom создаёт и меняет только человек (WebUI или человеческий токен)")]
    AxiomRequiresHuman,
    /// 422: `theorem` от агента — только с доказательством `{checked, method, result}`.
    #[error(
        "theorem от агента требует доказательства: data.evidence {{checked, method, result}} \
         у записи или непустое evidence: во frontmatter заметки"
    )]
    EvidenceRequired,
    /// 422: смена статуса — только с причиной (`reason` записи, `message` заметки).
    #[error("смена certainty требует причины: reason у записи журнала, message у заметки")]
    ReasonRequired,
    /// 422: переход не разрешён (агент снимает статус не у `hypothesis`).
    #[error("недопустимая смена certainty: агент снимает статус только у hypothesis")]
    InvalidTransition,
}

impl CertaintyError {
    /// Машинный код для ответа API (`code` в problem).
    pub fn code(self) -> &'static str {
        match self {
            CertaintyError::AxiomRequiresHuman => "axiom-requires-human",
            CertaintyError::EvidenceRequired => "evidence-required",
            CertaintyError::ReasonRequired => "reason-required",
            CertaintyError::InvalidTransition => "invalid-transition",
        }
    }
}

/// Проверка операции по матрице прав (см. модуль). `current` — статус цели до операции
/// (`None` у новой цели), `evidence` — доказательство в итоговом состоянии, `reason` —
/// причина операции (пустая равна отсутствию).
pub fn check_certainty(
    author: AuthorKind,
    current: Option<Certainty>,
    change: &CertaintyChange,
    evidence: Option<&Evidence>,
    reason: Option<&str>,
) -> Result<(), CertaintyError> {
    let human = author == AuthorKind::Human;
    let has_evidence = evidence.is_some_and(Evidence::is_complete);
    let has_reason = reason.is_some_and(|r| !r.trim().is_empty());
    // любое изменение цели со статусом axiom — только человеку
    if !human && current == Some(Certainty::Axiom) {
        return Err(CertaintyError::AxiomRequiresHuman);
    }
    match *change {
        CertaintyChange::ModifyOther => Ok(()),
        CertaintyChange::Create(c) => match c {
            None | Some(Certainty::Hypothesis) => Ok(()),
            Some(Certainty::Axiom) if !human => Err(CertaintyError::AxiomRequiresHuman),
            Some(Certainty::Theorem) if !human && !has_evidence => {
                Err(CertaintyError::EvidenceRequired)
            }
            Some(_) => Ok(()),
        },
        CertaintyChange::Transition { from, to } => {
            if from == to {
                return Ok(());
            }
            if !human && (from == Some(Certainty::Axiom) || to == Some(Certainty::Axiom)) {
                return Err(CertaintyError::AxiomRequiresHuman);
            }
            if !human && to.is_none() && from != Some(Certainty::Hypothesis) {
                return Err(CertaintyError::InvalidTransition);
            }
            if !human && to == Some(Certainty::Theorem) && !has_evidence {
                return Err(CertaintyError::EvidenceRequired);
            }
            if !has_reason {
                return Err(CertaintyError::ReasonRequired);
            }
            Ok(())
        }
    }
}

/// Статус новой записи без явного `certainty`: insight агента (и системы) — `hypothesis`,
/// insight человека — `axiom` **[решение исполнителя]**; у остальных видов статуса нет.
pub fn default_certainty(author: AuthorKind, kind: EntryKind) -> Option<Certainty> {
    match (kind, author) {
        (EntryKind::Insight, AuthorKind::Human) => Some(Certainty::Axiom),
        (EntryKind::Insight, _) => Some(Certainty::Hypothesis),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AuthorKind::{Agent, Human, System};
    use Certainty::{Axiom, Hypothesis, Theorem};
    use CertaintyChange::{Create, ModifyOther, Transition};
    use CertaintyError::*;

    fn ev() -> Evidence {
        Evidence {
            checked: "c".into(),
            method: "m".into(),
            result: "r".into(),
        }
    }

    fn tr(from: Option<Certainty>, to: Option<Certainty>) -> CertaintyChange {
        Transition { from, to }
    }

    /// Строка матрицы: `(текущий, изменение, доказательство, причина, агент, человек)`.
    type Row = (
        Option<Certainty>,
        CertaintyChange,
        bool,
        bool,
        Result<(), CertaintyError>,
        Result<(), CertaintyError>,
    );

    #[test]
    fn matrix_agent_vs_human() {
        let rows: Vec<Row> = vec![
            // создать hypothesis / без статуса
            (None, Create(Some(Hypothesis)), false, false, Ok(()), Ok(())),
            (None, Create(None), false, false, Ok(()), Ok(())),
            // создать theorem: агенту — только с доказательством
            (
                None,
                Create(Some(Theorem)),
                false,
                false,
                Err(EvidenceRequired),
                Ok(()),
            ),
            (None, Create(Some(Theorem)), true, false, Ok(()), Ok(())),
            // создать axiom: агенту — 403
            (
                None,
                Create(Some(Axiom)),
                true,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            // hypothesis → theorem: агенту — доказательство и причина, человеку — причина
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), Some(Theorem)),
                true,
                true,
                Ok(()),
                Ok(()),
            ),
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), Some(Theorem)),
                false,
                true,
                Err(EvidenceRequired),
                Ok(()),
            ),
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), Some(Theorem)),
                true,
                false,
                Err(ReasonRequired),
                Err(ReasonRequired),
            ),
            // theorem → hypothesis: причина обоим
            (
                Some(Theorem),
                tr(Some(Theorem), Some(Hypothesis)),
                false,
                true,
                Ok(()),
                Ok(()),
            ),
            (
                Some(Theorem),
                tr(Some(Theorem), Some(Hypothesis)),
                false,
                false,
                Err(ReasonRequired),
                Err(ReasonRequired),
            ),
            // * → axiom: агенту — 403, человеку — с причиной
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), Some(Axiom)),
                true,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            (
                Some(Theorem),
                tr(Some(Theorem), Some(Axiom)),
                true,
                false,
                Err(AxiomRequiresHuman),
                Err(ReasonRequired),
            ),
            (
                None,
                tr(None, Some(Axiom)),
                false,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            // любое изменение axiom: агенту — 403
            (
                Some(Axiom),
                ModifyOther,
                true,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            (
                Some(Axiom),
                tr(Some(Axiom), Some(Hypothesis)),
                true,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            (
                Some(Axiom),
                tr(Some(Axiom), None),
                false,
                true,
                Err(AxiomRequiresHuman),
                Ok(()),
            ),
            // прочие изменения не-axiom — всем
            (Some(Theorem), ModifyOther, false, false, Ok(()), Ok(())),
            (Some(Hypothesis), ModifyOther, false, false, Ok(()), Ok(())),
            (None, ModifyOther, false, false, Ok(()), Ok(())),
            // снять статус: агенту — только у hypothesis, с причиной
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), None),
                false,
                true,
                Ok(()),
                Ok(()),
            ),
            (
                Some(Hypothesis),
                tr(Some(Hypothesis), None),
                false,
                false,
                Err(ReasonRequired),
                Err(ReasonRequired),
            ),
            (
                Some(Theorem),
                tr(Some(Theorem), None),
                false,
                true,
                Err(InvalidTransition),
                Ok(()),
            ),
            // поставить статус заметке без статуса
            (
                None,
                tr(None, Some(Hypothesis)),
                false,
                true,
                Ok(()),
                Ok(()),
            ),
            (
                None,
                tr(None, Some(Theorem)),
                false,
                true,
                Err(EvidenceRequired),
                Ok(()),
            ),
            (None, tr(None, Some(Theorem)), true, true, Ok(()), Ok(())),
            // без смены статуса — не переход
            (
                Some(Theorem),
                tr(Some(Theorem), Some(Theorem)),
                false,
                false,
                Ok(()),
                Ok(()),
            ),
        ];
        for (i, (cur, change, e, r, agent, human)) in rows.into_iter().enumerate() {
            let evidence = e.then(ev);
            let reason = r.then_some("причина");
            for (author, want) in [(Agent, agent), (System, agent), (Human, human)] {
                assert_eq!(
                    check_certainty(author, cur, &change, evidence.as_ref(), reason),
                    want,
                    "строка {i}: {author:?} {cur:?} {change:?} evidence={e} reason={r}"
                );
            }
        }
    }

    #[test]
    fn evidence_requires_all_three_fields() {
        let full = serde_json::json!({"evidence": {"checked": "a", "method": "b", "result": "c"}});
        assert_eq!(Evidence::from_data(&full), Some(ev_abc()));
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"evidence": {}}),
            serde_json::json!({"evidence": {"checked": "a", "method": "b"}}),
            serde_json::json!({"evidence": {"checked": "a", "method": " ", "result": "c"}}),
            serde_json::json!({"evidence": {"checked": "a", "method": 1, "result": "c"}}),
            serde_json::json!({"evidence": "строка"}),
        ] {
            assert_eq!(Evidence::from_data(&bad), None, "{bad}");
        }
        // неполное доказательство не засчитывается и в самой проверке
        let partial = Evidence {
            checked: "a".into(),
            method: String::new(),
            result: "c".into(),
        };
        assert_eq!(
            check_certainty(
                Agent,
                None,
                &Create(Some(Theorem)),
                Some(&partial),
                Some("r")
            ),
            Err(EvidenceRequired)
        );
        assert_eq!(Evidence::from_note(Some("  ")), None);
        assert_eq!(Evidence::from_note(None), None);
        assert!(Evidence::from_note(Some("прогон CI")).is_some_and(|e| e.is_complete()));
    }

    fn ev_abc() -> Evidence {
        Evidence {
            checked: "a".into(),
            method: "b".into(),
            result: "c".into(),
        }
    }

    #[test]
    fn default_certainty_by_author_and_kind() {
        assert_eq!(
            default_certainty(Agent, EntryKind::Insight),
            Some(Hypothesis)
        );
        assert_eq!(
            default_certainty(System, EntryKind::Insight),
            Some(Hypothesis)
        );
        assert_eq!(default_certainty(Human, EntryKind::Insight), Some(Axiom));
        for k in [EntryKind::Changelog, EntryKind::Release] {
            for a in [Agent, Human, System] {
                assert_eq!(default_certainty(a, k), None);
            }
        }
    }

    #[test]
    fn whitespace_reason_is_absent() {
        assert_eq!(
            check_certainty(
                Human,
                Some(Theorem),
                &tr(Some(Theorem), Some(Hypothesis)),
                None,
                Some("  ")
            ),
            Err(ReasonRequired)
        );
    }
}
