//! Insight набора M3 (`eval/insights.toml`): разбор, состояние записей после загрузки
//! (замещение, отзыв, итоговый `certainty`) и проверка разметки (задача 16 M3).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::corpus::{Corpus, Lang, valid_key};

/// Доказательство `theorem` (`data.evidence` записи).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub checked: String,
    pub method: String,
    pub result: String,
}

impl Evidence {
    fn is_complete(&self) -> bool {
        [&self.checked, &self.method, &self.result]
            .iter()
            .all(|s| !s.trim().is_empty())
    }
}

/// Insight набора M3 (`eval/insights.toml`, `[[insight]]`): загрузчик создаёт его
/// `POST /spaces/{решение}/entries`, замещает (`supersedes`) и отзывает (`retract_reason`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Insight {
    /// Ключ набора: `insight:<key>` в запросах.
    pub key: String,
    pub lang: Lang,
    /// `data.kind`: `gotcha | failure | discovery | decision | experiment | pattern`.
    pub kind: String,
    /// `solution` (пространство решения набора) | `global` (Knowledge).
    pub scope: String,
    #[serde(default)]
    pub area: Option<String>,
    /// `axiom` | `theorem` (с `evidence`) | `hypothesis`; без
    /// поля — статус по умолчанию агента (`hypothesis`).
    #[serde(default)]
    pub certainty: Option<String>,
    #[serde(default)]
    pub evidence: Option<Evidence>,
    pub title: String,
    pub body: String,
    /// Триггеры как в API (`{type, pattern, tools?}`).
    #[serde(default)]
    pub triggers: Vec<serde_json::Value>,
    /// Ключ insight, который этот замещает (`POST /entries/{id}/supersede`).
    #[serde(default)]
    pub supersedes: Option<String>,
    /// Отзыв после создания (`POST /entries/{id}/retract`).
    #[serde(default)]
    pub retract_reason: Option<String>,
    /// Ключи insight, которым эта запись противоречит (M4.5, связь `contradicts`, `POST
    /// /edges` после создания всех записей); цель надёжнее записи. Пусто — поля нет в
    /// сериализации: отпечаток корпуса у записей без связи не меняется.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradicts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InsightFile {
    #[serde(default)]
    insight: Vec<Insight>,
}

/// Виды insight (`data.kind`, как в API).
const INSIGHT_KINDS: [&str; 6] = [
    "gotcha",
    "failure",
    "discovery",
    "decision",
    "experiment",
    "pattern",
];
const CERTAINTIES: [&str; 3] = ["axiom", "theorem", "hypothesis"];

/// Надёжность для связи `contradicts` — шкала сервера (`api/search/graph.rs::reliability`):
/// `axiom` 3, `theorem` 2, `hypothesis` 1, без `certainty` 0.
pub fn reliability(c: Option<&str>) -> u8 {
    match c {
        Some("axiom") => 3,
        Some("theorem") => 2,
        Some("hypothesis") => 1,
        _ => 0,
    }
}

impl Corpus {
    /// Insight по ключу.
    pub fn insight(&self, key: &str) -> Option<&Insight> {
        self.insights.iter().find(|i| i.key == key)
    }

    /// Ключи insight, замещённых другими.
    pub fn superseded_keys(&self) -> BTreeSet<&str> {
        self.insights
            .iter()
            .filter_map(|i| i.supersedes.as_deref())
            .collect()
    }

    /// Insight действует (не замещён и не отозван).
    pub fn insight_active(&self, key: &str) -> bool {
        self.insight(key)
            .is_some_and(|i| i.retract_reason.is_none())
            && !self.superseded_keys().contains(key)
    }

    /// Итоговый статус insight по умолчанию агента: явный или `hypothesis`.
    pub fn insight_certainty(&self, key: &str) -> Option<&str> {
        self.insight(key)
            .map(|i| i.certainty.as_deref().unwrap_or("hypothesis"))
    }
}

/// Читает insight из TOML (`[[insight]]`); нет файла — пусто.
pub fn load_insights(path: &Path) -> Result<Vec<Insight>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: InsightFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(f.insight)
}

/// Проверка insight набора: ключи, виды, статусы, доказательства, цепочки замещения без
/// циклов (замещаемая запись определена раньше), отзывы.
pub(super) fn validate_insights(corpus: &Corpus, errs: &mut Vec<String>) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut successor: BTreeMap<&str, &str> = BTreeMap::new();
    for (idx, i) in corpus.insights.iter().enumerate() {
        let mut bad = |m: String| errs.push(format!("insight {}: {m}", i.key));
        if !valid_key(&i.key) {
            bad("ключ — [a-z0-9-]".into());
        }
        if seen.insert(&i.key, idx).is_some() {
            bad("ключ повторяется".into());
        }
        if !INSIGHT_KINDS.contains(&i.kind.as_str()) {
            bad(format!("kind — одно из {}", INSIGHT_KINDS.join(", ")));
        }
        if !matches!(i.scope.as_str(), "solution" | "global") {
            bad("scope — solution или global".into());
        }
        let title = i.title.trim().chars().count();
        if title == 0 || title > 500 || i.body.trim().is_empty() {
            bad("заголовок 1–500 символов и непустое тело".into());
        }
        if i.body.contains('\r') || i.title.contains('\r') {
            bad("переводы строк CRLF".into());
        }
        match i.certainty.as_deref() {
            None | Some("hypothesis") | Some("axiom") => {}
            Some("theorem") => {
                if !i.evidence.as_ref().is_some_and(Evidence::is_complete) {
                    bad("theorem без evidence {checked, method, result}".into());
                }
            }
            Some(c) => bad(format!("certainty «{c}»: {}", CERTAINTIES.join(" | "))),
        }
        if i.retract_reason
            .as_ref()
            .is_some_and(|r| r.trim().is_empty())
        {
            bad("пустая retract_reason".into());
        }
        if let Some(old) = &i.supersedes {
            match seen.get(old.as_str()) {
                Some(&j) if j < idx => {
                    let o = &corpus.insights[j];
                    if o.retract_reason.is_some() {
                        bad(format!("замещает отозванный {old}"));
                    }
                    if o.certainty.as_deref() == Some("axiom")
                        || i.certainty.as_deref() == Some("axiom")
                    {
                        bad("цепочки замещения без axiom (axiom меняет только человек)".into());
                    }
                    if o.scope != i.scope {
                        bad(format!("scope отличается от замещаемого {old}"));
                    }
                }
                _ => bad(format!(
                    "supersedes {old}: нет insight с таким ключом выше по файлу"
                )),
            }
            if let Some(prev) = successor.insert(old, &i.key) {
                bad(format!("{old} уже замещён записью {prev}"));
            }
        }
    }
    // связи `contradicts` (M4.5): цель существует, действует, это theorem/axiom и она строго
    // надёжнее записи — правило сервера (`api/search/graph.rs::contradicts_applies`), та же
    // шкала [`reliability`]; у insight набора без поля `certainty` итоговый статус —
    // `hypothesis` (умолчание агента), поэтому берётся итоговый статус
    let rank = |k: &str| reliability(corpus.insight_certainty(k));
    for i in &corpus.insights {
        for t in &i.contradicts {
            let bad = |m: String| format!("insight {}: contradicts {t}: {m}", i.key);
            match corpus.insight(t) {
                None => errs.push(bad("нет insight с таким ключом".into())),
                Some(o) => {
                    if !corpus.insight_active(t) {
                        errs.push(bad("цель замещена или отозвана".into()));
                    }
                    let (src, dst) = (rank(&i.key), rank(&o.key));
                    if dst < reliability(Some("theorem")) || dst <= src {
                        errs.push(bad(
                            "цель — theorem/axiom, надёжнее записи (axiom > theorem > hypothesis > без certainty)"
                                .into(),
                        ));
                    }
                }
            }
        }
    }
    // циклы: путь по замещениям от любой записи конечен (при «выше по файлу» их нет, но
    // проверка независима от порядка)
    let by_key: BTreeMap<&str, &Insight> = corpus
        .insights
        .iter()
        .map(|i| (i.key.as_str(), i))
        .collect();
    for i in &corpus.insights {
        let mut cur = i;
        let mut steps = 0;
        while let Some(next) = cur.supersedes.as_deref().and_then(|k| by_key.get(k)) {
            steps += 1;
            if steps > corpus.insights.len() {
                errs.push(format!("insight {}: цикл замещений", i.key));
                break;
            }
            cur = next;
        }
    }
}
