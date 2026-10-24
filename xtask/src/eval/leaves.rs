//! Листы выдачи `mode = chunks` и их собственный текст для chunk-recall@5 (ТЗ §18.2, задача 16
//! M3): лист читается из SQLite прогона, начало, повторяющее конец предыдущего листа того же
//! узла (перекрытие `chunk_overlap_sentences`), отрезается — доказательство, попавшее в лист
//! только перекрытием, основная метрика не засчитывает (справочная — засчитывает).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::corpus::{Kind, Query};
use super::metrics::leaf_contains;
use super::run::TOP_K;

/// Лист выдачи `mode = chunks` (запросы `chunk`).
#[derive(Debug, Clone, Serialize)]
pub struct LeafHit {
    pub chunk_id: String,
    pub path: String,
    /// Собственный текст листа (без перекрытия) содержит `evidence` целиком.
    pub evidence: bool,
    /// С перекрытием — справочно: доказательство попало в лист только перекрытием.
    pub evidence_with_overlap: bool,
}

/// Собственный текст листа: без начала, совпадающего с концом предыдущего листа того же
/// узла (перекрытие `chunk_overlap_sentences`, не короче [`OVERLAP_MIN`] байт). Сравнение — только
/// по узлу, не по `heading_path`: перекрытие не пересекает группу нарезки, а путь заголовков
/// листа внутри группы может меняться (H4–H6 в `heading_path` после задачи 17), и тогда
/// сравнение по пути не отрезало бы перекрытие и завысило бы собственный текст.
#[derive(Debug, Clone)]
pub struct LeafText {
    pub path: String,
    pub text: String,
    pub core: String,
}

/// Короче — совпадение конца и начала листов считается случайным, а не перекрытием.
const OVERLAP_MIN: usize = 10;

/// Длина перекрытия: самый длинный префикс `cur`, которым кончается `prev`.
pub fn overlap_len(prev: &str, cur: &str) -> usize {
    let max = prev.len().min(cur.len());
    (OVERLAP_MIN..=max)
        .rev()
        .find(|&l| cur.is_char_boundary(l) && prev.ends_with(&cur[..l]))
        .unwrap_or(0)
}

/// Лист из SQLite: id чанка, узел, путь заметки, текст (в порядке узла и `ordinal`).
pub struct LeafRow {
    pub id: String,
    pub node: String,
    pub path: String,
    pub text: String,
}

/// Собственный текст листов: начало листа, которым кончается предыдущий лист того же узла,
/// отрезается.
pub fn cores_of(rows: impl IntoIterator<Item = LeafRow>) -> HashMap<String, LeafText> {
    let mut out = HashMap::new();
    let mut prev: Option<(String, String)> = None;
    for LeafRow {
        id,
        node,
        path,
        text,
    } in rows
    {
        let cut = match &prev {
            Some((pn, pt)) if *pn == node => overlap_len(pt, &text),
            _ => 0,
        };
        let core = text[cut..].to_string();
        prev = Some((node, text.clone()));
        out.insert(id, LeafText { path, text, core });
    }
    out
}

/// Листы текущих ревизий заметок из SQLite прогона: id чанка → путь, текст, свой текст.
pub fn leaf_cores(data: &Path) -> Result<HashMap<String, LeafText>> {
    let c = rusqlite::Connection::open_with_flags(
        super::gate::user_db(data)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut st = c.prepare(
        "SELECT c.id, c.node_id, n.path, c.text FROM chunks c
         JOIN nodes n ON n.id = c.node_id
         WHERE c.level = 'chunk' AND n.deleted_at IS NULL AND c.rev = n.current_rev
         ORDER BY c.node_id, c.ordinal",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(LeafRow {
                id: r.get(0)?,
                node: r.get(1)?,
                path: r.get(2)?,
                text: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cores_of(rows))
}

/// Разрезанные доказательства: `evidence` запроса `chunk` не помещается целиком в собственный
/// текст ни одного листа ожидаемой заметки (потолок chunk-recall@5 — `1 − split / queries`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SplitStats {
    pub queries: usize,
    /// Нет листа, собственный текст которого содержит `evidence`.
    pub split: usize,
    /// Из разрезанных — целиком в листе вместе с перекрытием.
    pub split_whole_with_overlap: usize,
    /// Листов длинных заметок (`<язык>/long/`).
    pub long_leaves: usize,
}

/// [`SplitStats`] набора по листам прогона.
pub fn split_stats(leaves: &HashMap<String, LeafText>, queries: &[Query]) -> SplitStats {
    let mut s = SplitStats {
        long_leaves: leaves
            .values()
            .filter(|l| super::corpus::is_long_path(&l.path))
            .count(),
        ..SplitStats::default()
    };
    for q in queries.iter().filter(|q| q.kind == Kind::Chunk) {
        let (Some(ev), Some(note)) = (q.evidence.as_deref(), q.expect.first()) else {
            continue;
        };
        s.queries += 1;
        let of_note = || leaves.values().filter(|l| &l.path == note);
        if !of_note().any(|l| leaf_contains(&l.core, ev)) {
            s.split += 1;
            if of_note().any(|l| leaf_contains(&l.text, ev)) {
                s.split_whole_with_overlap += 1;
            }
        }
    }
    s
}

/// Листы выдачи `mode = chunks` (первые 10 элементов уровня `chunk`) и их собственные
/// тексты: лист заметки — из `leaves` (без перекрытия), лист записи журнала — текст ответа.
pub fn chunk_leaves(
    v: &Value,
    leaves: &HashMap<String, LeafText>,
    evidence: &str,
) -> (Vec<LeafHit>, Vec<String>) {
    let mut hits = Vec::new();
    let mut cores = Vec::new();
    for i in v["items"].as_array().into_iter().flatten() {
        if i["level"] != "chunk" {
            continue;
        }
        let ids: Vec<&str> = i["chunk_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let found = ids.iter().find_map(|id| leaves.get(*id).map(|l| (*id, l)));
        let (id, path, text, core) = match found {
            Some((id, l)) => (id, l.path.clone(), l.text.clone(), l.core.clone()),
            None => {
                let t = i["text"].as_str().unwrap_or_default().to_string();
                (
                    ids.first().copied().unwrap_or_default(),
                    i["path"].as_str().unwrap_or_default().to_string(),
                    t.clone(),
                    t,
                )
            }
        };
        hits.push(LeafHit {
            chunk_id: id.to_string(),
            path,
            evidence: leaf_contains(&core, evidence),
            evidence_with_overlap: leaf_contains(&text, evidence),
        });
        cores.push(core);
        if cores.len() == TOP_K {
            break;
        }
    }
    (hits, cores)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::metrics::chunk_scores;
    use serde_json::json;

    fn row(id: &str, node: &str, text: &str) -> LeafRow {
        LeafRow {
            id: id.into(),
            node: node.into(),
            path: format!("en/long/{node}"),
            text: text.into(),
        }
    }

    /// Перекрытие отрезается по узлу, даже если путь заголовков соседних листов разный
    /// (H4–H6 в `heading_path`), и не отрезается между разными узлами.
    #[test]
    fn overlap_is_cut_by_node_not_by_heading_path() {
        let shared = "Shared sentence that ends the first leaf.";
        let cores = cores_of([
            row("a1", "n1", &format!("Intro text. {shared}")),
            row("a2", "n1", &format!("{shared} Body of the second leaf.")),
            row("b1", "n2", &format!("{shared} Other node.")),
        ]);
        assert_eq!(cores["a2"].core, " Body of the second leaf.");
        assert_eq!(cores["b1"].core, format!("{shared} Other node."));
        // разрезанное доказательство: целиком только вместе с перекрытием
        let mut q = crate::eval::corpus::tests_query("k1", "en/long/n1");
        q.evidence = Some(format!("{shared} Body of the second"));
        let s = split_stats(&cores, &[q]);
        assert_eq!((s.queries, s.split, s.split_whole_with_overlap), (1, 1, 1));
    }

    #[test]
    fn overlap_is_the_longest_shared_suffix_prefix() {
        let prev = "Первое предложение. Второе предложение о повторах.";
        let cur = "Второе предложение о повторах. Новый текст листа.";
        let n = overlap_len(prev, cur);
        assert_eq!(&cur[..n], "Второе предложение о повторах.");
        // случайное совпадение короче предела — не перекрытие
        assert_eq!(overlap_len("abc. x", "x and more"), 0);
        assert_eq!(overlap_len("", "text"), 0);
    }

    fn leaf(path: &str, text: &str, core: &str) -> LeafText {
        LeafText {
            path: path.into(),
            text: text.into(),
            core: core.into(),
        }
    }

    /// Листы выдачи: только уровень `chunk`, свой текст — из SQLite, перекрытие отрезано.
    #[test]
    fn chunk_leaves_use_core_text_and_skip_summaries() {
        let ev = "batch is retried four times";
        let leaves: HashMap<String, LeafText> = [
            (
                "c1".to_string(),
                leaf("en/long/a", "Intro. The batch is", "Intro. The batch is"),
            ),
            (
                "c2".to_string(),
                // начало листа — перекрытие с c1: доказательство целиком только с ним
                leaf(
                    "en/long/a",
                    "Intro. The batch is retried four times. Done.",
                    "retried four times. Done.",
                ),
            ),
            (
                "c3".to_string(),
                leaf(
                    "en/long/b",
                    "So the batch is retried four times here.",
                    "So the batch is retried four times here.",
                ),
            ),
        ]
        .into_iter()
        .collect();
        let v = json!({"items": [
            {"level": "section", "chunk_ids": ["s1"], "path": "en/long/a"},
            {"level": "chunk", "chunk_ids": ["c2"], "path": "en/long/a"},
            {"level": "chunk", "chunk_ids": ["e1"], "path": "", "text": "entry text"},
            {"level": "chunk", "chunk_ids": ["s9", "c3"], "path": "en/long/b"},
        ]});
        let (hits, cores) = chunk_leaves(&v, &leaves, ev);
        let ids: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
        assert_eq!(ids, ["c2", "e1", "c3"]);
        assert_eq!(
            hits.iter()
                .map(|h| (h.evidence, h.evidence_with_overlap))
                .collect::<Vec<_>>(),
            [(false, true), (false, false), (true, true)]
        );
        assert_eq!(cores[1], "entry text");
        let (r5, n10) = chunk_scores(&cores, ev);
        assert_eq!(r5, 1.0);
        assert!((n10 - 1.0 / 4f64.log2()).abs() < 1e-12);
    }
}
