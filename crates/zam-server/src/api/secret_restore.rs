//! Обвязка восстановления значений под `‹secret›` при записи агентом (спецификация M7 §4.3.2,
//! задача 6 M7.1): алгоритм — [`zam_secrets::restore`] (фича `restore` крейта `zam-secrets`, с
//! круга правок 1); здесь — кто пишет, база правки, поля записи журнала, предупреждение и отказ.
//!
//! - Агент по токену ([`agent_view::masks`]) правит маскированный текст; человек в сессии WebUI
//!   видит исходник и пишет как есть.
//! - Метка без пары (дописана, пришла из другого текста, при создании — каждая) записывается как
//!   есть: ответ получает предупреждение [`SECRET_MARK_KEPT`] (решение координатора §4.3.2 п. 4 и
//!   по задаче 6: при создании — тоже).
//! - Правка, которая выводит восстановленное значение из-под маски (снят забор кода, URL с
//!   паролем переписан, у ключа снят разделитель), отклоняется: 422 [`SECRET_CONTEXT_CHANGED`]
//!   с номерами строк маскированного вида, без значений; данные не теряются и не утекают
//!   (решение координатора, круг правок 1). Так же — метка без пары (или со спорной парой из
//!   другого участка) там, где в том же изменённом участке осталась маска базы без пары: это
//!   правка окружения существующего секрета, литералом значение молча пропало бы из текущей
//!   ревизии (решения координатора по сомнению 1 круга правок 1 и по кругу правок 2). Строки
//!   массивов `data` и теги — как строки текста ([`restore_items`], элемент — строка); объекты в
//!   массиве — только по виду целиком. Литерал `‹secret›` в базе — не маска секрета.
//! - Поля записи журнала, которые агент видит маской и присылает обратно, — один список
//!   ([`EntryTexts`]) для правки, замещения и создания.
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{Db, Entry, StoreError, node_content};

pub use zam_secrets::restore::{MaskedBase, Restored, has_mark, marks_in, restore, restore_items};

use super::agent_view::{self, BLOCKING_FROM};
use crate::error::ApiError;

/// Предупреждение ответа записи: в новом тексте есть `‹secret›` без пары в базе — записаны как
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
        "{SECRET_MARK_KEPT}: {kept} — метка ‹secret› без прежнего значения на этом месте записана \
         как есть; ключ — из Vault (memory_vault get)"
    )
}

/// Места правки, выводящие секрет из-под маски: поле (`None` — текст заметки) и строки
/// маскированного вида (с 1).
#[derive(Default)]
pub struct Exposed(Vec<(Option<String>, Vec<usize>)>);

impl Exposed {
    /// Места итога восстановления `r` поля `field`.
    pub fn add(&mut self, field: Option<&str>, r: &Restored) {
        if !r.exposed.is_empty() {
            self.0.push((field.map(str::to_string), r.exposed.clone()));
        }
    }

    /// Поле целиком (массив `data`, теги): метка без пары там, где осталась маска без пары.
    fn add_field(&mut self, field: &str) {
        self.0.push((Some(field.to_string()), Vec::new()));
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
                Some(f) if l.is_empty() => f.clone(),
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

/// Текст поля `new`, прежний текст которого `base` (`None` — поля не было): восстановление на
/// месте, места выхода из-под маски — в `ex`; меток без пары.
pub fn restore_in_place(
    base: Option<&str>,
    new: &mut String,
    field: &str,
    ex: &mut Exposed,
) -> usize {
    if !has_mark(new) {
        return 0;
    }
    let r = restore(base.unwrap_or(""), new);
    ex.add(Some(field), &r);
    *new = r.text;
    r.kept
}

/// Текстовые поля записи журнала, которые агент видит маской ([`agent_view::ENTRY_FIELDS`]) и
/// присылает обратно: заголовок, тело, дописка, строки `data` (и её поля, переданные отдельно —
/// `next_step`, `blocked_by` задачи), теги, область. Один список для правки (`PATCH`), замещения
/// и создания.
#[derive(Default)]
pub struct EntryTexts<'a> {
    pub title: Option<&'a mut String>,
    pub body: Option<&'a mut String>,
    /// Дописка тела (`append_body`) — новый текст: её метки без пары.
    pub append_body: Option<&'a str>,
    pub data: Option<&'a mut Value>,
    /// Поля `data` верхнего уровня, переданные отдельно.
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

    /// Меток во всех полях — при создании каждая без пары.
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

    /// Восстановление по прежней записи `base`: меток без пары; правка, выводящая секрет из-под
    /// маски, — 422 [`SECRET_CONTEXT_CHANGED`] (ничего не записано).
    pub fn restore(self, base: EntryBase) -> Result<usize, ApiError> {
        let mut ex = Exposed::default();
        let mut kept = 0;
        if let Some(t) = self.title {
            kept += restore_in_place(Some(base.title), t, "title", &mut ex);
        }
        if let Some(b) = self.body {
            kept += restore_in_place(Some(base.body), b, "body", &mut ex);
        }
        kept += self.append_body.map_or(0, marks_in);
        if let Some(d) = self.data {
            let mut path = String::from("data");
            kept += restore_value(base.data, d, &mut path, &mut ex);
        }
        for (k, s) in self.data_fields {
            let b = base.data.get(k).and_then(Value::as_str);
            kept += restore_in_place(b, s, &format!("data.{k}"), &mut ex);
        }
        if let Some(t) = self.tags {
            let olds: Vec<&str> = base.tags.iter().map(String::as_str).collect();
            kept += restore_strings(&olds, t, "tags", None, &mut ex);
        }
        if let Some(a) = self.area {
            kept += restore_in_place(base.area, a, "area", &mut ex);
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

/// Значение `data` записи журнала, правленное агентом по маске прежнего `base` (маска выдачи —
/// у каждой строки своя, [`agent_view::mask_value`]): строка — по строке того же пути (ключи
/// объекта); строки массива — как строки текста ([`restore_strings`]: элемент — строка
/// выравнивания, правленый элемент — по правилу ключа, круг правок 2, B3; по индексу значения не
/// подставляются, круг правок 1, M2); прочие элементы массива (объекты, массивы) — только прежний
/// элемент, вид которого совпал целиком (на любом месте, каждый — не больше одной пары), иначе их
/// метки — без пары, а если при этом остался такой прежний элемент с маской без пары — место
/// отказа (массив целиком). Места выхода из-под маски — в `ex` с путём поля.
fn restore_value(base: &Value, new: &mut Value, path: &mut String, ex: &mut Exposed) -> usize {
    match new {
        Value::String(s) => restore_in_place(base.as_str(), s, path, ex),
        Value::Array(a) => {
            let olds: &[Value] = base.as_array().map_or(&[], |v| v.as_slice());
            let old_strings: Vec<&str> = olds.iter().filter_map(Value::as_str).collect();
            let mut at: Vec<usize> = Vec::new();
            let mut strings: Vec<String> = Vec::new();
            for (k, x) in a.iter_mut().enumerate() {
                if let Value::String(s) = x {
                    at.push(k);
                    strings.push(std::mem::take(s));
                }
            }
            let mut kept = restore_strings(&old_strings, &mut strings, path, Some(&at), ex);
            for (k, s) in at.into_iter().zip(strings) {
                a[k] = Value::String(s);
            }
            let mut list = Listed::new(olds.iter().filter(|v| !v.is_string()).collect());
            let unpaired: usize = a
                .iter_mut()
                .filter(|x| !x.is_string())
                .map(|x| list.restore(x))
                .sum();
            // метка без пары при прежнем элементе с маской без пары — правка окружения
            // существующего секрета (решение координатора по сомнению 1 круга правок 1)
            if unpaired > 0 && list.lost() {
                ex.add_field(path);
            }
            kept += unpaired;
            kept
        }
        Value::Object(o) => o
            .iter_mut()
            .map(|(k, x)| {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                let kept = restore_value(base.get(k).unwrap_or(&Value::Null), x, path, ex);
                path.truncate(len);
                kept
            })
            .sum(),
        _ => 0,
    }
}

/// Строки списка `new` (теги, строки массива `data` по порядку), правленные по маске прежних
/// `olds`: [`restore_items`] — элемент как строка текста; места выхода из-под маски — в `ex` с
/// путём `field[i]` (`i` — индекс элемента в массиве: `at[n]` у `n`-й строки, без `at` — `n`);
/// меток без пары.
fn restore_strings(
    olds: &[&str],
    new: &mut [String],
    field: &str,
    at: Option<&[usize]>,
    ex: &mut Exposed,
) -> usize {
    if !new.iter().any(|s| has_mark(s)) {
        return 0;
    }
    let edited: Vec<&str> = new.iter().map(String::as_str).collect();
    let rs = restore_items(olds, &edited);
    let mut kept = 0;
    for (n, (s, r)) in new.iter_mut().zip(rs).enumerate() {
        if !r.exposed.is_empty() {
            let i = at.map_or(n, |a| a[n]);
            ex.add(Some(&format!("{field}[{i}]")), &r);
        }
        kept += r.kept;
        *s = r.text;
    }
    kept
}

/// Элементы прежнего массива (не строки) для элементов нового: вид каждого (маска — по
/// требованию) и занят ли он.
struct Listed<'b> {
    olds: Vec<&'b Value>,
    shown: Vec<Option<Value>>,
    taken: Vec<bool>,
}

impl<'b> Listed<'b> {
    fn new(olds: Vec<&'b Value>) -> Self {
        let n = olds.len();
        Listed {
            olds,
            shown: vec![None; n],
            taken: vec![false; n],
        }
    }

    /// Элемент `x` нового массива: вид совпал целиком с видом свободного прежнего элемента —
    /// прежний элемент; иначе — как есть, его метки без пары. Вид целиком — значит и значения
    /// под маской прежние: из-под маски они не выходят.
    fn restore(&mut self, x: &mut Value) -> usize {
        let marks = marks_in_value(x);
        if marks == 0 {
            return 0;
        }
        for j in 0..self.olds.len() {
            if self.taken[j] {
                continue;
            }
            let shown = self.shown[j].get_or_insert_with(|| {
                let mut v = self.olds[j].clone();
                agent_view::mask_value(&mut v);
                v
            });
            if shown == x {
                self.taken[j] = true;
                *x = self.olds[j].clone();
                return 0;
            }
        }
        marks
    }

    /// Остался прежний элемент с маской (значением секрета) без пары: меток в его виде больше,
    /// чем литералов `‹secret›` в исходнике (литерал — не маска, круг правок 2, B2).
    fn lost(&mut self) -> bool {
        (0..self.olds.len()).any(|j| {
            let old = self.olds[j];
            !self.taken[j]
                && marks_in_value(self.shown[j].get_or_insert_with(|| {
                    let mut v = old.clone();
                    agent_view::mask_value(&mut v);
                    v
                })) > marks_in_value(old)
        })
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

    /// `data` записи: строки по пути; строки массива — как строки текста (переставлены — на
    /// места, правлены с тем же ключом — значения на месте, круг правок 2, B3); новое поле — как
    /// есть; теги — так же. Элемент с другим ключом на месте прежнего с маской — отказ с путём
    /// элемента (по индексу значение не подставляется, M2; молча пропасть ему нельзя — решение
    /// координатора по сомнению 1 круга правок 1). Объект в массиве — только по виду целиком;
    /// литерал `‹secret›` в нём — не маска (B2).
    #[test]
    fn restore_entry_fields() {
        let (k1, k2) = (key(21), key(22));
        let base_data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {k1}"), "n": 3},
            "options": [format!("a {k1}"), format!("b {k2}"), "c"],
        });
        let mut shown = base_data.clone();
        agent_view::mask_value(&mut shown);
        let tags = vec![format!("t-{k1}"), "ops".to_string()];
        let base = entry("Ключи", "", &base_data, &tags);
        let mut data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {M} (prod)"), "n": 3},
            "options": ["c", shown["options"][1].clone(), shown["options"][0].clone()],
            "new": format!("x {M}"),
        });
        let mut seen: Vec<String> = tags
            .iter()
            .map(|t| agent_view::mask(t).into_owned())
            .collect();
        seen.reverse();
        seen.push(M.to_string());
        let t = EntryTexts {
            data: Some(&mut data),
            tags: Some(&mut seen),
            ..EntryTexts::default()
        };
        assert_eq!(t.marks(), 6);
        let kept = t.restore(base).unwrap();
        // новое поле и лишняя метка тегов (прежние теги все в паре) — без пары
        assert_eq!(kept, 2);
        assert_eq!(data["evidence"]["checked"], format!("token: {k1} (prod)"));
        assert_eq!(
            data["options"],
            json!(["c", format!("b {k2}"), format!("a {k1}")])
        );
        assert_eq!(data["new"], format!("x {M}"));
        assert_eq!(
            seen,
            vec!["ops".to_string(), format!("t-{k1}"), M.to_string()]
        );
        // «a ‹secret› (правлено)» и перенос: ключ тот же — значение на месте
        let mut data = json!({"options": ["c", format!("a {M} (правлено)")]});
        let t = EntryTexts {
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        assert_eq!(t.restore(base).unwrap(), 0);
        assert_eq!(data["options"], json!(["c", format!("a {k1} (правлено)")]));
        // «z ‹secret›» на месте «a <ключ>»: ключ другой, маска прежнего элемента без пары — отказ
        let mut data = json!({"options": [format!("z {M}"), shown["options"][1].clone(), "c"]});
        let t = EntryTexts {
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        let e = t.restore(base).unwrap_err();
        assert!(e.to_string().contains("(data.options[0]: строка 1)"), "{e}");
        assert!(!e.to_string().contains(&k1), "{e}");
        // объект в массиве: литерал в исходнике — не маска, правленый объект с меткой — литерал
        let lit = json!({"o": [{"v": format!("x {M}")}]});
        let base = entry("t", "", &lit, &[]);
        let mut data = json!({"o": [{"v": format!("y {M}")}]});
        let t = EntryTexts {
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        assert_eq!(t.restore(base).unwrap(), 1);
        let sec = json!({"o": [{"v": format!("x {k2}")}]});
        let base = entry("t", "", &sec, &[]);
        let mut data = json!({"o": [{"v": format!("y {M}")}]});
        let t = EntryTexts {
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        assert!(
            t.restore(base)
                .unwrap_err()
                .to_string()
                .contains("(data.o)")
        );
    }

    /// Поле, где значение вышло бы из-под маски, — 422 с путём поля и строкой, без значения.
    #[test]
    fn restore_entry_exposed() {
        let p = ["hunter", "41Qz9x"].concat();
        let (body0, data0) = (
            format!("ok\npassword = {p}\n"),
            json!({"e": {"m": format!("password = {p}")}}),
        );
        let base = entry("t", &body0, &data0, &[]);
        let mut body = format!("ok\npassword {M}\n");
        let mut data = json!({"e": {"m": format!("password {M}")}});
        let t = EntryTexts {
            body: Some(&mut body),
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        let e = t.restore(base).unwrap_err();
        let s = e.to_string();
        assert!(matches!(e, ApiError::SecretContextChanged(_)), "{s}");
        assert!(
            s.contains("body: строка 2") && s.contains("data.e.m: строка 1"),
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
