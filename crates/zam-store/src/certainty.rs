//! Эпистемический статус `certainty` (§7.5 ТЗ): насколько записи или заметке можно доверять —
//! `axiom` (установлено человеком: в WebUI или агентом по прямому указанию пользователя),
//! `theorem` (проверено, есть доказательство), `hypothesis` (предположение). Права на статус —
//! матрица «агент / человек / system» ([`check_certainty`]).
//!
//! Проверка выполняется **внутри транзакции писателя** каждой операции хранилища, меняющей
//! статус записи журнала или заметки (создание, правка, PATCH, откат, замещение): текущий статус
//! читается в той же транзакции, поэтому параллельная правка не обходит правило
//! **[решение исполнителя]**. Сервер лишь определяет автора (`AuthorKind`) и отображает
//! [`CertaintyError`] в 422.
//!
//! Матрица — **решение владельца 2026-10-26** (агент — любой токен; человек — сессия WebUI
//! администратора или пользователя; `system` — служебные операции: миграции, импорт,
//! восстановление):
//!
//! | Действие | Агент | Человек | system |
//! |---|---|---|---|
//! | создать без статуса / `hypothesis` / `axiom` | да | да | да |
//! | создать `theorem` | только с доказательством | да | да |
//! | любой переход статуса (в т. ч. `→ axiom`, `axiom →`, снятие) | с причиной; `→ theorem` — ещё и с доказательством | причина необязательна | без проверок |
//! | правка, supersede, retract, delete, move, триггеры, вложение у `axiom` | да | да | да |
//! | ребро от `axiom` | да | да | да |
//!
//! `axiom` агент ставит и меняет только по прямому указанию пользователя — это правило
//! инструкции агента (§14.2), сервер его не проверяет. Каждая смена статуса — новая ревизия с
//! причиной: у записи — `entry_revisions.reason` (у человека может быть пустой), у заметки —
//! сообщение ревизии (`message`).

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
    /// move, триггеры, вложение, явное ребро — разрешено всем, в том числе у `axiom`.
    ModifyOther,
}

/// Нарушение матрицы прав (422).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CertaintyError {
    /// 422: `theorem` от агента — только с доказательством `{checked, method, result}`.
    #[error(
        "theorem от агента требует доказательства: data.evidence {{checked, method, result}} \
         у записи или непустое evidence: во frontmatter заметки"
    )]
    EvidenceRequired,
    /// 422: смена статуса агентом — только с причиной (`reason` записи, `message` заметки).
    #[error("смена certainty требует причины: reason у записи журнала, message у заметки")]
    ReasonRequired,
}

impl CertaintyError {
    /// Машинный код для ответа API (`code` в problem).
    pub fn code(self) -> &'static str {
        match self {
            CertaintyError::EvidenceRequired => "evidence-required",
            CertaintyError::ReasonRequired => "reason-required",
        }
    }
}

/// Проверка операции по матрице прав (см. модуль): ограничен только агент. `current` — статус
/// цели до операции (`None` у новой цели): часть контекста проверки, матрица M4 от него не
/// зависит — изменения цели `axiom` разрешены всем (решение владельца 2026-10-26). `evidence` —
/// доказательство в итоговом состоянии, `reason` — причина операции (пустая равна отсутствию).
pub fn check_certainty(
    author: AuthorKind,
    _current: Option<Certainty>,
    change: &CertaintyChange,
    evidence: Option<&Evidence>,
    reason: Option<&str>,
) -> Result<(), CertaintyError> {
    if author != AuthorKind::Agent {
        return Ok(());
    }
    let has_evidence = evidence.is_some_and(Evidence::is_complete);
    let has_reason = reason.is_some_and(|r| !r.trim().is_empty());
    match *change {
        CertaintyChange::ModifyOther => Ok(()),
        CertaintyChange::Create(c) => {
            if c == Some(Certainty::Theorem) && !has_evidence {
                return Err(CertaintyError::EvidenceRequired);
            }
            Ok(())
        }
        CertaintyChange::Transition { from, to } => {
            if from == to {
                return Ok(());
            }
            if to == Some(Certainty::Theorem) && !has_evidence {
                return Err(CertaintyError::EvidenceRequired);
            }
            if !has_reason {
                return Err(CertaintyError::ReasonRequired);
            }
            Ok(())
        }
    }
}

/// Итоговое состояние после правки агентом: если статус цели — `theorem`, доказательство
/// должно быть полным и в итоговом состоянии (`data.evidence` записи, `evidence:` заметки), иначе
/// [`CertaintyError::EvidenceRequired`] — `theorem` не переживает удаление доказательства.
/// Свежесть доказательства не проверяется. Человеку и `system` — без ограничений.
pub fn check_final_state(
    author: AuthorKind,
    certainty: Option<Certainty>,
    evidence: Option<&Evidence>,
) -> Result<(), CertaintyError> {
    if author == AuthorKind::Agent
        && certainty == Some(Certainty::Theorem)
        && !evidence.is_some_and(Evidence::is_complete)
    {
        return Err(CertaintyError::EvidenceRequired);
    }
    Ok(())
}

/// Статус новой записи без явного `certainty`: insight агента — `hypothesis`; insight человека
/// создаётся без статуса (аксиому человек ставит явно), `system` статусы сам не выставляет —
/// **решение владельца 2026-10-26**; у остальных видов статуса нет.
pub fn default_certainty(author: AuthorKind, kind: EntryKind) -> Option<Certainty> {
    match (kind, author) {
        (EntryKind::Insight, AuthorKind::Agent) => Some(Certainty::Hypothesis),
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

    /// Проверка с доказательством `e` и причиной `r`; текущий статус цели — исходный у
    /// перехода, `axiom` у прочих изменений (самый строгий случай M3).
    fn check(a: AuthorKind, c: CertaintyChange, e: bool, r: bool) -> Result<(), CertaintyError> {
        let current = match c {
            Transition { from, .. } => from,
            Create(_) => None,
            ModifyOther => Some(Axiom),
        };
        let evidence = e.then(ev);
        check_certainty(a, current, &c, evidence.as_ref(), r.then_some("причина"))
    }

    /// Строка матрицы §7.5: `(изменение, доказательство, причина, агент, человек, system)`.
    type Row = (
        CertaintyChange,
        bool,
        bool,
        Result<(), CertaintyError>,
        Result<(), CertaintyError>,
        Result<(), CertaintyError>,
    );

    const ALL: [Option<Certainty>; 4] = [None, Some(Hypothesis), Some(Theorem), Some(Axiom)];

    #[test]
    fn matrix_agent_human_system() {
        let mut rows: Vec<Row> = vec![
            // создать без статуса / hypothesis / axiom — всем
            (Create(None), false, false, Ok(()), Ok(()), Ok(())),
            (
                Create(Some(Hypothesis)),
                false,
                false,
                Ok(()),
                Ok(()),
                Ok(()),
            ),
            (Create(Some(Axiom)), false, false, Ok(()), Ok(()), Ok(())),
            // создать theorem: агенту — только с доказательством
            (
                Create(Some(Theorem)),
                false,
                false,
                Err(EvidenceRequired),
                Ok(()),
                Ok(()),
            ),
            (Create(Some(Theorem)), true, false, Ok(()), Ok(()), Ok(())),
            // правка, supersede, retract, delete, move, триггеры, вложение, ребро — всем,
            // в том числе у axiom (операция не меняет статус)
            (ModifyOther, false, false, Ok(()), Ok(()), Ok(())),
        ];
        // любой переход статуса: агенту — с причиной, → theorem — ещё и с доказательством;
        // человеку причина необязательна, system — без проверок
        for from in ALL {
            for to in ALL {
                if from == to {
                    continue;
                }
                let to_theorem = to == Some(Theorem);
                let no_reason = if to_theorem {
                    Err(EvidenceRequired)
                } else {
                    Err(ReasonRequired)
                };
                rows.push((tr(from, to), false, false, no_reason, Ok(()), Ok(())));
                rows.push((
                    tr(from, to),
                    true,
                    false,
                    Err(ReasonRequired),
                    Ok(()),
                    Ok(()),
                ));
                let with_reason = if to_theorem {
                    Err(EvidenceRequired)
                } else {
                    Ok(())
                };
                rows.push((tr(from, to), false, true, with_reason, Ok(()), Ok(())));
                rows.push((tr(from, to), true, true, Ok(()), Ok(()), Ok(())));
            }
        }
        // без смены статуса — не переход
        for c in ALL {
            rows.push((tr(c, c), false, false, Ok(()), Ok(()), Ok(())));
        }
        for (i, (change, e, r, agent, human, system)) in rows.into_iter().enumerate() {
            for (author, want) in [(Agent, agent), (Human, human), (System, system)] {
                assert_eq!(
                    check(author, change, e, r),
                    want,
                    "строка {i}: {author:?} {change:?} evidence={e} reason={r}"
                );
            }
        }
    }

    #[test]
    fn agent_axiom_transition_requires_reason() {
        for change in [
            tr(None, Some(Axiom)),
            tr(Some(Hypothesis), Some(Axiom)),
            tr(Some(Theorem), Some(Axiom)),
            tr(Some(Axiom), Some(Hypothesis)),
            tr(Some(Axiom), None),
        ] {
            assert_eq!(check(Agent, change, false, false), Err(ReasonRequired));
            assert_eq!(check(Agent, change, false, true), Ok(()), "{change:?}");
        }
        // axiom → theorem: причина и доказательство
        let to_theorem = tr(Some(Axiom), Some(Theorem));
        assert_eq!(check(Agent, to_theorem, false, true), Err(EvidenceRequired));
        assert_eq!(check(Agent, to_theorem, true, true), Ok(()));
    }

    #[test]
    fn human_transition_without_reason_ok() {
        for from in ALL {
            for to in ALL {
                assert_eq!(check(Human, tr(from, to), false, false), Ok(()));
                assert_eq!(check(System, tr(from, to), false, false), Ok(()));
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
    fn default_certainty_human_insight_none() {
        assert_eq!(
            default_certainty(Agent, EntryKind::Insight),
            Some(Hypothesis)
        );
        assert_eq!(default_certainty(Human, EntryKind::Insight), None);
        assert_eq!(default_certainty(System, EntryKind::Insight), None);
        for k in [EntryKind::Changelog, EntryKind::Release] {
            for a in [Agent, Human, System] {
                assert_eq!(default_certainty(a, k), None);
            }
        }
    }

    #[test]
    fn final_theorem_needs_evidence_from_agent() {
        assert_eq!(
            check_final_state(Agent, Some(Theorem), None),
            Err(EvidenceRequired)
        );
        assert_eq!(check_final_state(Agent, Some(Theorem), Some(&ev())), Ok(()));
        assert_eq!(check_final_state(Human, Some(Theorem), None), Ok(()));
        assert_eq!(check_final_state(System, Some(Theorem), None), Ok(()));
        for c in [None, Some(Hypothesis), Some(Axiom)] {
            assert_eq!(check_final_state(Agent, c, None), Ok(()));
        }
    }

    #[test]
    fn messages_have_no_double_spaces() {
        for e in [EvidenceRequired, ReasonRequired] {
            let m = e.to_string();
            assert!(!m.contains("  "), "{m}");
            assert!(
                m.contains("{checked, method, result}") || e == ReasonRequired,
                "{m}"
            );
        }
    }

    #[test]
    fn whitespace_reason_is_absent() {
        assert_eq!(
            check_certainty(
                Agent,
                Some(Theorem),
                &tr(Some(Theorem), Some(Hypothesis)),
                None,
                Some("  ")
            ),
            Err(ReasonRequired)
        );
    }
}
