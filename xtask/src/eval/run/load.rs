//! Загрузчик insight набора M3 (см. модуль [`super`]): создание, замещение и отзыв записей
//! токеном прогона (с M4 `axiom` ставит и агент, §7.5).

use std::collections::BTreeMap;
use std::fs;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{ENTRIES_FILE, Session};
use crate::eval::corpus::{EVAL_SOLUTION, Insight};
use crate::smoke::api;

impl Session {
    /// Создаёт, замещает и отзывает insight набора (порядок файла: замещаемая запись
    /// определена раньше); проверяет итоговый `certainty`.
    pub(super) fn load_insights(&mut self, insights: &[Insight]) -> Result<()> {
        if insights.is_empty() {
            return Ok(());
        }
        let token = self.token.clone();
        let mut ids: BTreeMap<&str, String> = BTreeMap::new();
        for i in insights {
            let mut data = json!({ "kind": i.kind });
            if let Some(e) = &i.evidence {
                data["evidence"] = json!(e);
            }
            let mut body = json!({
                "title": i.title,
                "body": i.body,
                "data": data,
                "triggers": i.triggers,
                "force": true,
            });
            if let Some(a) = &i.area {
                body["area"] = json!(a);
            }
            if let Some(c) = &i.certainty {
                body["certainty"] = json!(c);
            }
            let (path, entry_field) = match &i.supersedes {
                Some(old) => {
                    let old_id = ids
                        .get(old.as_str())
                        .with_context(|| format!("insight {}: {old} ещё не создан", i.key))?;
                    (format!("/api/v1/entries/{old_id}/supersede"), "entry")
                }
                None => {
                    body["kind"] = json!("insight");
                    body["scope"] = json!(i.scope);
                    (format!("/api/v1/spaces/{EVAL_SOLUTION}/entries"), "")
                }
            };
            let (st, v) = api(&self.base, &token, &path, Some(&body))?;
            if st != 201 {
                bail!("insight {}: POST {path}: HTTP {st}: {v}", i.key);
            }
            let e = if entry_field.is_empty() {
                &v
            } else {
                &v[entry_field]
            };
            let id = e["id"]
                .as_str()
                .with_context(|| format!("insight {}: нет id в ответе {v}", i.key))?
                .to_string();
            let want = i.certainty.as_deref().unwrap_or("hypothesis");
            if e["certainty"] != want {
                bail!(
                    "insight {}: certainty {}, ожидался {want}",
                    i.key,
                    e["certainty"]
                );
            }
            self.entry_keys.insert(id.clone(), i.key.clone());
            ids.insert(&i.key, id);
        }
        for i in insights {
            if let Some(reason) = &i.retract_reason {
                let id = &ids[i.key.as_str()];
                let (st, v) = self.api(
                    &format!("/api/v1/entries/{id}/retract"),
                    Some(&json!({ "reason": reason })),
                )?;
                if st != 200 {
                    bail!("insight {}: отзыв: HTTP {st}: {v}", i.key);
                }
            }
        }
        let dir = self.data.parent().context("каталог прогона")?;
        fs::write(
            dir.join(ENTRIES_FILE),
            serde_json::to_string(&self.entry_keys)?,
        )?;
        Ok(())
    }
}
