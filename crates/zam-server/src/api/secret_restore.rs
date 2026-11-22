//! Обвязка восстановления значений при записи агентом (спецификация M7 §4.3.2, задача 6 M7.1; с
//! круга правок 3 — **по номеру метки**, решение координатора): алгоритм — [`zam_secrets::restore`]
//! (фича `restore` крейта `zam-secrets`); здесь — кто пишет, база правки, поля записи журнала,
//! предупреждение и отказ.
//!
//! - Агент по токену ([`agent_view::masks`]) правит редактируемый вид, где каждая маска поля —
//!   `‹secret-N›` (N — номер маски в поле ревизии, [`agent_view::edit_view_owned`],
//!   [`agent_view::number_entries`]); человек в сессии WebUI видит исходник и пишет как есть.
//! - `‹secret-N›` нового текста — значение N-й маски того же поля базы (повтор номера — повтор
//!   значения); маска, номер которой не вернулся, удалена правкой. Метка без номера маски (без
//!   номера, номер вне поля, при создании — каждая) записывается как есть: ответ получает
//!   предупреждение [`SECRET_MARK_KEPT`].
//! - Правка, которая выводит восстановленное значение из-под маски (снят забор кода, URL с
//!   паролем переписан, у ключа снят разделитель), отклоняется: 422 [`SECRET_CONTEXT_CHANGED`]
//!   с номерами строк редактируемого вида, без значений; ничего не записано.
//! - Поля записи журнала — [`EntryTexts`] (правка, замещение, создание): заголовок, тело, область
//!   — каждое своё поле; строки `data` (и `next_step`, `blocked_by` задачи — строки той же
//!   `data`) — одно поле со сквозной нумерацией по порядку обхода; теги — одно поле.
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{Db, Entry, StoreError, node_content};

pub use zam_secrets::restore::{Deferred, FieldMasks, Restored, has_mark, marks_in, restore};

use super::agent_view::{self, BLOCKING_FROM};
use crate::error::ApiError;

/// Предупреждение ответа записи: в новом тексте есть метка без номера маски базы — записаны как
/// есть; число таких мест — в строке предупреждения и в поле ответа с тем же именем.
pub const SECRET_MARK_KEPT: &str = "secret_mark_kept";

/// Код отказа записи: правка выводит секрет из-под маски (422).
pub const SECRET_CONTEXT_CHANGED: &str = "secret_context_changed";

/// [`restore`] своих текстов: вместе больше [`BLOCKING_FROM`] — в пуле блокирующих задач (пул
/// снят — 503); база возвращается вызывающему (находки `secrets_detected` — по ней).
pub async fn restore_owned(base: String, edited: String) -> Result<(Restored, String), ApiError> {
    if !has_mark(&edited) {
        return Ok((same(edited), base));
    }
    if base.len() + edited.len() <= BLOCKING_FROM {
        let r = restore(&base, &edited);
        return Ok((r, base));
    }
    agent_view::blocking(move || {
        let r = restore(&base, &edited);
        (r, base)
    })
    .await
}

/// Текст как есть: меток без пары нет.
fn same(text: String) -> Restored {
    Restored {
        text,
        kept: 0,
        exposed: Vec::new(),
    }
}

/// Текст ревизии `rev` узла `id`; нет ревизии — `None` (запись всё равно отклонит хранилище:
/// 409 или 404).
pub async fn node_base(db: &Db, id: Id, rev: i64) -> Result<Option<String>, ApiError> {
    match node_content(db, id, Some(rev)).await {
        Ok((_, t)) => Ok(Some(t)),
        Err(StoreError::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Предупреждение [`SECRET_MARK_KEPT`] в ответ-объект `v` (строка `warnings` и число); при
/// `kept == 0` ответ не меняется.
pub fn attach_kept(v: &mut Value, kept: usize) {
    if kept == 0 {
        return;
    }
    super::glossary::attach_warnings(v, vec![kept_warning(kept)]);
    if let Value::Object(o) = v {
        o.insert(SECRET_MARK_KEPT.into(), json!(kept));
    }
}

/// Строка предупреждения [`SECRET_MARK_KEPT`].
pub fn kept_warning(kept: usize) -> String {
    format!(
        "{SECRET_MARK_KEPT}: {kept} — метка ‹secret› без номера маски этого поля записана как есть; \
         ключ — из Vault (memory_vault get)"
    )
}

/// Места правки, выводящие секрет из-под маски: поле (`None` — текст заметки) и строки
/// редактируемого вида (с 1).
#[derive(Default)]
pub struct Exposed(Vec<(Option<String>, Vec<usize>)>);

impl Exposed {
    /// Места итога восстановления `r` поля `field`.
    pub fn add(&mut self, field: Option<&str>, r: &Restored) {
        if !r.exposed.is_empty() {
            self.0.push((field.map(str::to_string), r.exposed.clone()));
        }
    }

    /// Нет мест — `Ok`; иначе 422 [`SECRET_CONTEXT_CHANGED`] без значений.
    pub fn check(self) -> Result<(), ApiError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let lines = |l: &[usize]| {
            let n: Vec<String> = l.iter().map(usize::to_string).collect();
            format!(
                "{} {}",
                if l.len() == 1 {
                    "строка"
                } else {
                    "строки"
                },
                n.join(", ")
            )
        };
        let places: Vec<String> = self
            .0
            .iter()
            .map(|(f, l)| match f {
                Some(f) => format!("{f}: {}", lines(l)),
                None => lines(l),
            })
            .collect();
        Err(ApiError::SecretContextChanged(format!(
            "правка выводит секрет из-под маски ({}): сохраните контекст ключа — `ключ: значение`, \
             блок кода, URL — или правьте эту часть в WebUI",
            places.join("; ")
        )))
    }
}

/// Текст поля `new` по маскам поля базы `masks`: восстановление на месте, места выхода из-под
/// маски — в `ex` под именем `field`; меток без номера маски.
pub fn restore_in_place(
    masks: &FieldMasks,
    new: &mut String,
    field: &str,
    ex: &mut Exposed,
) -> usize {
    if !has_mark(new) {
        return 0;
    }
    let r = masks.restore(new);
    ex.add(Some(field), &r);
    *new = r.text;
    r.kept
}

/// Текстовые поля записи журнала, которые агент видит редактируемым видом
/// ([`agent_view::EDIT_ENTRY_FIELDS`]) и присылает обратно: заголовок, тело, дописка, строки
/// `data` (и её поля, переданные отдельно — `next_step`, `blocked_by` задачи), теги, область.
/// Один список для правки (`PATCH`), замещения и создания.
#[derive(Default)]
pub struct EntryTexts<'a> {
    pub title: Option<&'a mut String>,
    pub body: Option<&'a mut String>,
    /// Дописка тела (`append_body`) — новый текст: её метки без значения.
    pub append_body: Option<&'a str>,
    pub data: Option<&'a mut Value>,
    /// Поля `data` верхнего уровня, переданные отдельно (строки той же нумерации `data`).
    pub data_fields: Vec<(&'static str, &'a mut String)>,
    pub tags: Option<&'a mut Vec<String>>,
    pub area: Option<&'a mut String>,
}

impl<'a> EntryTexts<'a> {
    /// Поля созданной записи `e` (создание: базы нет).
    pub fn of(e: &'a mut Entry) -> Self {
        EntryTexts {
            title: Some(&mut e.title),
            body: Some(&mut e.body),
            data: Some(&mut e.data),
            tags: Some(&mut e.tags),
            area: e.area.as_mut(),
            ..EntryTexts::default()
        }
    }

    /// Меток во всех полях — при создании каждая без значения.
    pub fn marks(&self) -> usize {
        self.title.as_deref().map_or(0, |s| marks_in(s))
            + self.body.as_deref().map_or(0, |s| marks_in(s))
            + self.append_body.map_or(0, marks_in)
            + self.data.as_deref().map_or(0, marks_in_value)
            + self
                .data_fields
                .iter()
                .map(|(_, s)| marks_in(s))
                .sum::<usize>()
            + self
                .tags
                .as_deref()
                .map_or(0, |t| t.iter().map(|s| marks_in(s)).sum())
            + self.area.as_deref().map_or(0, |s| marks_in(s))
    }

    /// Восстановление по прежней записи `base` (каждое поле — по маскам того же поля): меток без
    /// значения; правка, выводящая секрет из-под маски, — 422 [`SECRET_CONTEXT_CHANGED`] (ничего
    /// не записано).
    pub fn restore(self, base: EntryBase) -> Result<usize, ApiError> {
        let mut ex = Exposed::default();
        let mut kept = 0;
        if let Some(t) = self.title {
            kept += restore_in_place(&FieldMasks::of(base.title), t, "title", &mut ex);
        }
        if let Some(b) = self.body {
            kept += restore_in_place(&FieldMasks::of(base.body), b, "body", &mut ex);
        }
        kept += self.append_body.map_or(0, marks_in);
        if self.data.is_some() || !self.data_fields.is_empty() {
            let mut masks = FieldMasks::new();
            add_value(base.data, &mut masks);
            if let Some(d) = self.data {
                let mut path = String::from("data");
                kept += restore_value(&masks, d, &mut path, &mut ex);
            }
            for (k, s) in self.data_fields {
                kept += restore_in_place(&masks, s, &format!("data.{k}"), &mut ex);
            }
        }
        if let Some(t) = self.tags {
            let mut masks = FieldMasks::new();
            for s in base.tags {
                masks.add(s);
            }
            for (i, s) in t.iter_mut().enumerate() {
                kept += restore_in_place(&masks, s, &format!("tags[{i}]"), &mut ex);
            }
        }
        if let Some(a) = self.area {
            let masks = FieldMasks::of(base.area.unwrap_or(""));
            kept += restore_in_place(&masks, a, "area", &mut ex);
        }
        ex.check()?;
        Ok(kept)
    }
}

/// Прежние тексты записи журнала — база восстановления [`EntryTexts::restore`].
#[derive(Clone, Copy)]
pub struct EntryBase<'b> {
    pub title: &'b str,
    pub body: &'b str,
    pub data: &'b Value,
    pub tags: &'b [String],
    pub area: Option<&'b str>,
}

impl<'b> From<&'b Entry> for EntryBase<'b> {
    fn from(e: &'b Entry) -> Self {
        EntryBase {
            title: &e.title,
            body: &e.body,
            data: &e.data,
            tags: &e.tags,
            area: e.area.as_deref(),
        }
    }
}

/// Меток во всех строках значения (рекурсивно; ключи объектов — нет).
pub fn marks_in_value(v: &Value) -> usize {
    match v {
        Value::String(s) => marks_in(s),
        Value::Array(a) => a.iter().map(marks_in_value).sum(),
        Value::Object(o) => o.values().map(marks_in_value).sum(),
        _ => 0,
    }
}

/// Маски всех строк значения `v` — в поле `masks` по порядку обхода (тот же, что у вида —
/// [`agent_view::number_value`]: массивы по элементам, объекты по полям).
fn add_value<'b>(v: &'b Value, masks: &mut FieldMasks<'b>) {
    match v {
        Value::String(s) => masks.add(s),
        Value::Array(a) => a.iter().for_each(|x| add_value(x, masks)),
        Value::Object(o) => o.values().for_each(|x| add_value(x, masks)),
        _ => {}
    }
}

/// Строки значения `data`, правленного агентом по редактируемому виду: каждая — по маскам всей
/// прежней `data` (`masks`, сквозная нумерация): элемент или поле, перенесённые в другое место, —
/// со своим значением. Места выхода из-под маски — в `ex` с путём строки (`data.options[0]`).
fn restore_value(
    masks: &FieldMasks,
    new: &mut Value,
    path: &mut String,
    ex: &mut Exposed,
) -> usize {
    match new {
        Value::String(s) => restore_in_place(masks, s, path, ex),
        Value::Array(a) => a
            .iter_mut()
            .enumerate()
            .map(|(i, x)| {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                let kept = restore_value(masks, x, path, ex);
                path.truncate(len);
                kept
            })
            .sum(),
        Value::Object(o) => o
            .iter_mut()
            .map(|(k, x)| {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                let kept = restore_value(masks, x, path, ex);
                path.truncate(len);
                kept
            })
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключ из частей (в исходнике целиком его нет — push protection); `n` — отличие ключей.
    fn key(n: u32) -> String {
        format!("gh{}_{}{n:04}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF")
    }

    const M: &str = zam_markdown::SECRET_MARK;

    fn m(n: u32) -> String {
        format!("‹secret-{n}›")
    }

    fn entry<'b>(
        title: &'b str,
        body: &'b str,
        data: &'b Value,
        tags: &'b [String],
    ) -> EntryBase<'b> {
        EntryBase {
            title,
            body,
            data,
            tags,
            area: None,
        }
    }

    /// Вид записи агенту и его правка обратно: редактируемые поля — `‹secret-N›` (строки `data` и
    /// теги — сквозная нумерация по обходу), прочие — `‹secret›`; строка `data`, перенесённая в
    /// другой элемент или поле, и тег на другом месте — со своим значением; номер вне поля и
    /// метка без номера — как есть (`kept`).
    #[test]
    fn restore_entry_fields() {
        let (k1, k2) = (key(21), key(22));
        let base_data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {k1}"), "n": 3},
            "options": [format!("a {k1}"), format!("b {k2}"), "c"],
        });
        let tags = vec![format!("t-{k1}"), "ops".to_string()];
        let mut shown = json!({"title": format!("Ключ {k2}"), "data": base_data.clone(),
                               "tags": tags.clone(), "reason": format!("из-за {k1}")});
        agent_view::number_entries(&mut shown);
        assert_eq!(shown["title"], format!("Ключ {}", m(1)));
        assert_eq!(shown["reason"], format!("из-за {M}"));
        // порядок обхода `data`: поля объекта — по порядку карты
        let numbered = shown["data"].to_string();
        assert!(
            (1..=3).all(|n| numbered.contains(&m(n))) && !numbered.contains(&m(4)),
            "{numbered}"
        );
        // находка тега — весь тег (правило энтропии берёт и `t-`)
        assert_eq!(shown["tags"][0], m(1));
        let num = |v: &Value| v.as_str().unwrap().to_string();
        let (n_checked, n_a, n_b) = (
            num(&shown["data"]["evidence"]["checked"]),
            num(&shown["data"]["options"][0]),
            num(&shown["data"]["options"][1]),
        );
        let base = entry("Ключи", "", &base_data, &tags);
        let mut data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("{n_checked} (prod)"), "n": 3},
            "options": ["c", n_b.clone(), n_a.replace("a ", "a (правлено) ")],
            "new": format!("x {M} и {}", m(9)),
        });
        let mut seen = vec!["ops".to_string(), m(1), M.to_string()];
        let t = EntryTexts {
            data: Some(&mut data),
            tags: Some(&mut seen),
            ..EntryTexts::default()
        };
        assert_eq!(t.marks(), 7);
        let kept = t.restore(base).unwrap();
        assert_eq!(kept, 3);
        assert_eq!(data["evidence"]["checked"], format!("token: {k1} (prod)"));
        assert_eq!(
            data["options"],
            json!(["c", format!("b {k2}"), format!("a (правлено) {k1}")])
        );
        assert_eq!(data["new"], format!("x {M} и {}", m(9)));
        assert_eq!(
            seen,
            vec!["ops".to_string(), format!("t-{k1}"), M.to_string()]
        );
    }

    /// Поле, где значение вышло бы из-под маски, — 422 с путём поля и строкой, без значения.
    #[test]
    fn restore_entry_exposed() {
        let p = ["hunter", "41Qz9x"].concat();
        let (body0, data0) = (
            format!("ok\npassword = {p}\n"),
            json!({"e": {"m": format!("password = {p}")}, "o": [format!("password = {p}")]}),
        );
        let base = entry("t", &body0, &data0, &[]);
        let mut body = format!("ok\npassword {}\n", m(1));
        let mut data = json!({"e": {"m": format!("password {}", m(1))},
                              "o": [format!("пароль — {}", m(2))]});
        let t = EntryTexts {
            body: Some(&mut body),
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        let e = t.restore(base).unwrap_err();
        let s = e.to_string();
        assert!(matches!(e, ApiError::SecretContextChanged(_)), "{s}");
        assert!(
            s.contains("body: строка 2")
                && s.contains("data.e.m: строка 1")
                && s.contains("data.o[0]: строка 1"),
            "{s}"
        );
        assert!(!s.contains(&p), "{s}");
    }

    /// Предупреждение: строка в `warnings` (к прежним) и число; без меток — ответ как есть.
    #[test]
    fn attach_kept_warning() {
        let mut v = json!({"id": "x", "warnings": ["term_alias: a"]});
        attach_kept(&mut v, 0);
        assert_eq!(v, json!({"id": "x", "warnings": ["term_alias: a"]}));
        attach_kept(&mut v, 2);
        assert_eq!(v[SECRET_MARK_KEPT], 2);
        let w = v["warnings"].as_array().unwrap();
        assert_eq!(w[0], "term_alias: a");
        assert!(
            w[1].as_str().unwrap().starts_with("secret_mark_kept: 2"),
            "{v}"
        );
    }
}
