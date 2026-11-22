//! Обвязка восстановления значений при записи агентом (спецификация M7 §4.3.2, задача 6 M7.1;
//! решения координатора 10, круги правок 3–5): алгоритм — [`zam_secrets::restore`] (фича
//! `restore` крейта `zam-secrets`); здесь — кто пишет, база правки, поля записи журнала,
//! предупреждение и отказ.
//!
//! - Агент по токену ([`agent_view::masks`]) правит вид, где каждая маска объекта — `‹secret-T-N›`
//!   (T — метка объекта и ревизии [`agent_view::object_tag`], N — сквозной номер маски по
//!   редактируемым полям объекта, [`agent_view::edit_view_owned`],
//!   [`agent_view::number_entries`]); человек в сессии WebUI видит исходник и пишет как есть.
//! - Метка этого объекта и **любой его ревизии** (финальное ревью M7.1, I-2) — значение маски той
//!   ревизии в любом поле объекта (перенос строки между полями сохраняет значение, вторая правка
//!   подряд из того же вида — тоже): ревизию по T находит [`revisions_by_tag`] среди ревизий
//!   объекта (у узла — [`node_revisions`], у записи — [`EntryRevs`]); две ревизии с одним T, чужой
//!   T (другой объект, вид до обновления сервера), номер вне той ревизии, `‹secret›`, прежняя
//!   `‹secret-N›` — записываются как есть. Предупреждение [`SECRET_MARK_KEPT`] поля —
//!   меток-литералов в итоге сверх их числа в исходнике текущей ревизии того же поля; при создании
//!   — каждая метка.
//! - Восстановление — после проверки доступа к объекту (пространство, вид записи, не отчёт) и
//!   после 428/409 обработчика: о чужом объекте агент получает тот же 404, что у чтения.
//! - Покрытие по происхождению (§4.3.2 п. 5): каждая позиция итога, где стоит восстановленное
//!   значение (и у `PATCH` — маска текущей ревизии, перенесённая без правки), должна быть целиком
//!   покрыта находкой; иначе — 422 [`SECRET_CONTEXT_CHANGED`] с номерами строк итога, без
//!   значений; ничего не записано. Набранный агентом текст не проверяется.
//! - Поля записи журнала — [`EntryTexts`] (правка, замещение, создание) в порядке нумерации:
//!   заголовок, тело, строки `data` (и `next_step`, `blocked_by` задачи — строки той же `data`),
//!   теги, область.
use std::collections::BTreeSet;

use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{Db, Entry, EntryRevision, StoreError, node_content};

pub use zam_secrets::restore::{
    Deferred, Filled, ObjectMasks, Restored, has_mark, literals_in, marks_in, numbered_view,
    restore, revisions_by_tag, secret_ranges, tags_in, uncovered_lines, unnumbered,
};

use super::agent_view::{self, BLOCKING_FROM};
use crate::error::ApiError;

/// Предупреждение ответа записи: в новом тексте меток-литералов (без значения: чужая метка, номер
/// вне базы, `‹secret›`) больше, чем было в поле, — записаны как есть; число — в строке
/// предупреждения и в поле ответа с тем же именем.
pub const SECRET_MARK_KEPT: &str = "secret_mark_kept";

/// Код отказа записи: правка выводит секрет из-под маски (422).
pub const SECRET_CONTEXT_CHANGED: &str = "secret_context_changed";

/// Предупреждение ответа записи: значения меток взяты из ревизий объекта, отличных от базовой
/// (финальное ревью M7.1, N-3): секрет мог смениться после той ревизии; номера — в строке
/// предупреждения и в поле ответа с тем же именем (список).
pub const SECRET_MARK_FROM_REV: &str = "secret_mark_from_rev";

/// Ревизий объекта, кроме текущей, по меткам одной записи — не больше (финальное ревью M7.1,
/// N-2): T вычисляется публично, и метки, сфабрикованные для каждой ревизии, иначе подняли бы в
/// память и скан все ревизии. Сверх — метки литералом (`secret_mark_kept`), без ошибки; первыми
/// берутся ревизии, ближайшие к текущей.
pub const RESTORE_REVS_MAX: usize = 8;

/// Исходник загружаемых прежних ревизий одной записи — не больше `limits.note_max_bytes` × это
/// число (N-2).
pub const RESTORE_BYTES_FACTOR: usize = 4;

/// Ревизии `found` (T и номер) по пределу [`RESTORE_REVS_MAX`]: текущая `current` — всегда,
/// прочие — ближайшие к ней, по убыванию номера.
pub fn nearest_revisions(found: Vec<(String, i64)>, current: i64) -> Vec<(String, i64)> {
    let mut found = found;
    found.sort_unstable_by_key(|(_, k)| std::cmp::Reverse(*k));
    let mut others = 0;
    found.retain(|(_, k)| {
        if *k == current {
            return true;
        }
        others += 1;
        others <= RESTORE_REVS_MAX
    });
    found
}

/// Ревизия объекта, метки которой есть в правке агента (M7 §4.3.2 п. 2): её T, номер и, у узла,
/// текст.
#[derive(Debug, Clone)]
pub struct RevText {
    pub tag: String,
    pub rev: i64,
    /// Текст ревизии; `None` — ревизия, текст которой уже прочитал вызывающий (`known` у
    /// [`node_revisions`]).
    pub text: Option<String>,
}

/// Ревизии узла `id` (`1..=max_rev`, текущая — `max_rev`) по меткам `‹secret-T-N›` текстов
/// `edited` (§4.3.2 п. 2, финальное ревью M7.1, I-2): каждой T правки — ревизия с этой T, если
/// она одна ([`revisions_by_tag`]); не больше [`RESTORE_REVS_MAX`] прежних, ближайшие к текущей
/// ([`nearest_revisions`]), и тексты читаются, пока их сумма не больше `max_bytes` (N-2: сверх —
/// метки литералом); ревизии `known` не читаются (их тексты у вызывающего). Ревизии нет в базе —
/// пропуск (метки — литералы). Без меток с T — ни одного чтения.
pub async fn node_revisions(
    db: &Db,
    id: Id,
    max_rev: i64,
    edited: &[&str],
    known: &[i64],
    max_bytes: usize,
) -> Result<Vec<RevText>, ApiError> {
    let found: Vec<(String, i64)> = {
        let tags = tags_in(edited.iter().copied());
        revisions_by_tag(&tags, max_rev, |k| agent_view::object_tag(&id, k))
            .into_iter()
            .map(|(t, k)| (t.to_string(), k))
            .collect()
    };
    let mut out = Vec::new();
    let mut loaded = 0usize;
    for (tag, rev) in nearest_revisions(found, max_rev) {
        let text = if known.contains(&rev) {
            None
        } else {
            match node_base(db, id, rev).await? {
                Some(t) if loaded + t.len() <= max_bytes => {
                    loaded += t.len();
                    Some(t)
                }
                // сверх суммы — эта и более старые ревизии без значений
                Some(_) => break,
                None => continue,
            }
        };
        out.push(RevText { tag, rev, text });
    }
    Ok(out)
}

/// Номера ревизий, давших значения (`from` — их T, [`Restored::from`]), кроме базовой `base_rev`,
/// по возрастанию; `revs` — T и номер каждой ревизии масок.
pub fn from_revs<'t>(
    from: impl IntoIterator<Item = &'t String>,
    revs: &[(String, i64)],
    base_rev: i64,
) -> Vec<i64> {
    let mut out: Vec<i64> = from
        .into_iter()
        .filter_map(|t| revs.iter().find(|(tag, _)| tag == t).map(|(_, k)| *k))
        .filter(|&k| k != base_rev)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Предупреждение [`SECRET_MARK_FROM_REV`] в ответ-объект `v` (строка `warnings` и список номеров);
/// пустой список — ответ не меняется.
pub fn attach_from_rev(v: &mut Value, revs: &[i64]) {
    if revs.is_empty() {
        return;
    }
    let list = revs
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    super::glossary::attach_warnings(
        v,
        vec![format!(
            "{SECRET_MARK_FROM_REV}: {list} — значения меток взяты из прежних ревизий объекта, а не \
             из базовой: если секрет с тех пор меняли, проверьте его в WebUI"
        )],
    );
    if let Value::Object(o) = v {
        o.insert(SECRET_MARK_FROM_REV.into(), json!(revs));
    }
}

/// Маски ревизий `revs` узла: текст ревизии без своего (`None`) — из `known` (номер, текст); нет
/// там — ревизия пропущена.
pub fn node_masks<'a>(revs: &'a [RevText], known: &[(i64, &'a str)]) -> ObjectMasks<'a> {
    let mut m = ObjectMasks::default();
    for r in revs {
        let text = match &r.text {
            Some(t) => t.as_str(),
            None => match known.iter().find(|(k, _)| *k == r.rev) {
                Some((_, t)) => t,
                None => continue,
            },
        };
        m.revision(&r.tag).add(text);
    }
    m
}

/// Восстановление текста узла `edited` агентом по маскам ревизий `revs` (`base` — исходник
/// ревизии `base_rev`, `If-Match`; предупреждение — сверх литералов `base`): вместе больше
/// [`BLOCKING_FROM`] — в пуле блокирующих задач (пул снят — 503); база возвращается вызывающему
/// (находки `secrets_detected` — по ней).
pub async fn restore_owned(
    revs: Vec<RevText>,
    base_rev: i64,
    base: String,
    edited: String,
) -> Result<(Restored, String), ApiError> {
    if !has_mark(&edited) {
        return Ok((same(edited), base));
    }
    let size = base.len()
        + edited.len()
        + revs
            .iter()
            .map(|r| r.text.as_ref().map_or(0, String::len))
            .sum::<usize>();
    let run = move || {
        let r = node_masks(&revs, &[(base_rev, base.as_str())])
            .fill_marks(&edited)
            .into_restored(literals_in(&base));
        (r, base)
    };
    if size <= BLOCKING_FROM {
        return Ok(run());
    }
    agent_view::blocking(run).await
}

/// Текст как есть: литералов нет.
fn same(text: String) -> Restored {
    Restored {
        text,
        ..Restored::default()
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
        "{SECRET_MARK_KEPT}: {kept} — метка без значения (не из вида этого объекта и ревизии) \
         записана как есть; ключ — из Vault (memory_vault get)"
    )
}

/// Места правки, выводящие секрет из-под маски: поле (`None` — текст заметки) и строки итога
/// (с 1).
#[derive(Default)]
pub struct Exposed(Vec<(Option<String>, Vec<usize>)>);

impl Exposed {
    /// Места итога восстановления `r` поля `field`.
    pub fn add(&mut self, field: Option<&str>, r: &Restored) {
        self.lines(field, &r.exposed);
    }

    /// Строки `lines` итога поля `field`.
    fn lines(&mut self, field: Option<&str>, lines: &[usize]) {
        if !lines.is_empty() {
            self.0.push((field.map(str::to_string), lines.to_vec()));
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

/// Строки поля записи — `new` (путь строки и сама строка) — по маскам объекта `masks`: значения
/// на место, покрытие — по позициям восстановленных значений (места — в `ex` с путём строки);
/// итог — предупреждение поля сверх `current_literals` (литералов исходника поля); T ревизий,
/// давших значения, — в `from`.
fn restore_field(
    masks: &ObjectMasks,
    new: Vec<(String, &mut String)>,
    current_literals: usize,
    ex: &mut Exposed,
    from: &mut Vec<String>,
) -> usize {
    let mut literals = 0;
    for (path, s) in new {
        if !has_mark(s) {
            continue;
        }
        let f = masks.fill_marks(s);
        ex.lines(Some(&path), &uncovered_lines(&f.text, &f.at));
        literals += f.literals;
        from.extend(f.from);
        *s = f.text;
    }
    literals.saturating_sub(current_literals)
}

/// Текстовые поля записи журнала, которые агент видит редактируемым видом
/// ([`agent_view::EDIT_ENTRY_FIELDS`]) и присылает обратно: заголовок, тело, дописка, строки
/// `data` (и её поля, переданные отдельно — `next_step`, `blocked_by` задачи), теги, область.
/// Один список для правки (`PATCH`), замещения и создания.
#[derive(Default)]
pub struct EntryTexts<'a> {
    pub title: Option<&'a mut String>,
    pub body: Option<&'a mut String>,
    /// Дописка тела (`append_body`): метки объекта — значения, покрытие — в итоге тела (прежнее
    /// тело и дописка через пустую строку, как у хранилища).
    pub append_body: Option<&'a mut String>,
    pub data: Option<&'a mut Value>,
    /// Поля `data` верхнего уровня, переданные отдельно (строки той же `data`).
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
            + self.append_body.as_deref().map_or(0, |s| marks_in(s))
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

    /// Метки объекта T меток `‹secret-T-N›` всех полей (по ним [`EntryRevs::load`] находит
    /// ревизии).
    pub fn tags(&self) -> BTreeSet<String> {
        let mut texts: Vec<&str> = Vec::new();
        texts.extend(self.title.as_deref().map(String::as_str));
        texts.extend(self.body.as_deref().map(String::as_str));
        texts.extend(self.append_body.as_deref().map(String::as_str));
        if let Some(d) = self.data.as_deref() {
            strings_ref(d, &mut texts);
        }
        texts.extend(self.data_fields.iter().map(|(_, s)| s.as_str()));
        if let Some(t) = self.tags.as_deref() {
            texts.extend(t.iter().map(String::as_str));
        }
        texts.extend(self.area.as_deref().map(String::as_str));
        tags_in(texts).into_iter().map(str::to_string).collect()
    }

    /// Восстановление по маскам ревизий `revs` записи (у каждой — по всем её редактируемым полям
    /// в порядке нумерации, метка — `id` и ревизия; ревизии нашёл [`EntryRevs`]); `current` —
    /// текущая запись (литералы её полей — порог предупреждения, тело — основа дописки):
    /// предупреждение `secret_mark_kept` и номера ревизий, давших значения, кроме базовой
    /// `base_rev` (`secret_mark_from_rev`, N-3); правка, выводящая секрет из-под маски, — 422
    /// [`SECRET_CONTEXT_CHANGED`] (ничего не записано).
    pub fn restore(
        self,
        current: EntryBase,
        revs: &[EntryBase],
        base_rev: i64,
    ) -> Result<(usize, Vec<i64>), ApiError> {
        let mut masks = ObjectMasks::default();
        let mut tags: Vec<(String, i64)> = Vec::with_capacity(revs.len());
        for r in revs {
            let tag = agent_view::object_tag(&r.id, r.rev);
            masks.revision(&tag);
            tags.push((tag, r.rev));
            masks.add(r.title);
            masks.add(r.body);
            add_value(r.data, &mut masks);
            for t in r.tags {
                masks.add(t);
            }
            masks.add(r.area.unwrap_or(""));
        }
        let base = current;
        let mut ex = Exposed::default();
        let mut from: Vec<String> = Vec::new();
        let mut kept = 0;
        if let Some(t) = self.title {
            kept += restore_field(
                &masks,
                vec![("title".into(), t)],
                literals_in(base.title),
                &mut ex,
                &mut from,
            );
        }
        if let Some(b) = self.body {
            kept += restore_field(
                &masks,
                vec![("body".into(), b)],
                literals_in(base.body),
                &mut ex,
                &mut from,
            );
        }
        if let Some(more) = self.append_body
            && has_mark(more)
        {
            let f = masks.fill_marks(more);
            // итог тела — как `appended` хранилища: прежнее тело без хвостовых пробелов, пустая
            // строка, дописка (у пустого тела — сама дописка)
            let head = base.body.trim_end();
            let (text, off, skip) = if head.is_empty() {
                (f.text.clone(), 0, 0)
            } else {
                let text = format!("{head}\n\n{}", f.text);
                (text, head.len() + 2, head.matches('\n').count() + 2)
            };
            let at: Vec<_> = f.at.iter().map(|r| r.start + off..r.end + off).collect();
            let lines: Vec<usize> = uncovered_lines(&text, &at)
                .into_iter()
                .map(|l| l - skip)
                .collect();
            ex.lines(Some("append_body"), &lines);
            kept += f.literals;
            from.extend(f.from);
            *more = f.text;
        }
        if self.data.is_some() || !self.data_fields.is_empty() {
            // строки нового `data` с путями и поля `data`, переданные отдельно
            let mut keys: Vec<String> = self
                .data_fields
                .iter()
                .map(|(k, _)| (*k).to_string())
                .collect();
            let mut new: Vec<(String, &mut String)> = Vec::new();
            if let Some(d) = self.data {
                if let Value::Object(o) = &*d {
                    keys.extend(o.keys().cloned());
                }
                let mut path = String::from("data");
                strings_mut(d, &mut path, &mut new);
            }
            for (k, s) in self.data_fields {
                new.push((format!("data.{k}"), s));
            }
            // литералы исходника — у прежних строк тех же полей верхнего уровня: прочие слияние
            // не трогает
            let mut cur: Vec<&str> = Vec::new();
            match base.data {
                Value::Object(o) => {
                    keys.sort_unstable();
                    keys.dedup();
                    keys.iter()
                        .filter_map(|k| o.get(k))
                        .for_each(|v| strings_ref(v, &mut cur));
                }
                v => strings_ref(v, &mut cur),
            }
            let lit: usize = cur.iter().map(|s| literals_in(s)).sum();
            kept += restore_field(&masks, new, lit, &mut ex, &mut from);
        }
        if let Some(t) = self.tags {
            let lit: usize = base.tags.iter().map(|s| literals_in(s)).sum();
            let new: Vec<(String, &mut String)> = t
                .iter_mut()
                .enumerate()
                .map(|(i, s)| (format!("tags[{i}]"), s))
                .collect();
            kept += restore_field(&masks, new, lit, &mut ex, &mut from);
        }
        if let Some(a) = self.area {
            let lit = literals_in(base.area.unwrap_or(""));
            kept += restore_field(&masks, vec![("area".into(), a)], lit, &mut ex, &mut from);
        }
        ex.check()?;
        Ok((kept, from_revs(&from, &tags, base_rev)))
    }
}

/// Прежняя запись журнала — база восстановления [`EntryTexts::restore`]: её `id`, ревизия (метка
/// объекта) и тексты.
#[derive(Clone, Copy)]
pub struct EntryBase<'b> {
    pub id: Id,
    pub rev: i64,
    pub title: &'b str,
    pub body: &'b str,
    pub data: &'b Value,
    pub tags: &'b [String],
    pub area: Option<&'b str>,
}

impl<'b> From<&'b Entry> for EntryBase<'b> {
    fn from(e: &'b Entry) -> Self {
        EntryBase {
            id: e.id,
            rev: e.rev,
            title: &e.title,
            body: &e.body,
            data: &e.data,
            tags: &e.tags,
            area: e.area.as_deref(),
        }
    }
}

impl<'b> EntryBase<'b> {
    /// Прежняя ревизия `r` записи `id` (снимок `entry_revisions`).
    pub fn revision(id: Id, r: &'b EntryRevision) -> Self {
        EntryBase {
            id,
            rev: r.rev,
            title: &r.title,
            body: &r.body,
            data: &r.data,
            tags: &r.tags,
            area: r.area.as_deref(),
        }
    }
}

/// Ревизии записи журнала, метки которых агент вернул правкой или замещением (M7 §4.3.2 п. 2,
/// финальное ревью M7.1, I-2): номера ревизий с T меток правки (у каждой T — единственная
/// ревизия, [`revisions_by_tag`]) и снимки прежних из них; текущая ревизия — сама запись.
#[derive(Debug, Clone, Default)]
pub struct EntryRevs {
    revs: Vec<i64>,
    older: Vec<EntryRevision>,
}

impl EntryRevs {
    /// Ревизии записи `e` (`1..=e.rev`) по T меток правки `tags` ([`EntryTexts::tags`]): не
    /// больше [`RESTORE_REVS_MAX`] прежних, ближайшие к текущей, и снимки — пока их сумма не
    /// больше `max_bytes` (N-2; сверх — метки литералом); без меток с T — ни одного чтения.
    pub async fn load(
        db: &Db,
        e: &Entry,
        tags: &BTreeSet<String>,
        max_bytes: usize,
    ) -> Result<Self, ApiError> {
        let set: BTreeSet<&str> = tags.iter().map(String::as_str).collect();
        let found: Vec<(String, i64)> =
            revisions_by_tag(&set, e.rev, |k| agent_view::object_tag(&e.id, k))
                .into_iter()
                .map(|(t, k)| (t.to_string(), k))
                .collect();
        let near = nearest_revisions(found, e.rev);
        let older: Vec<i64> = near
            .iter()
            .map(|(_, k)| *k)
            .filter(|&k| k != e.rev)
            .collect();
        let mut rows = zam_store::entry_revisions_at(db, e.id, older).await?;
        // ближайшие к текущей — первыми; сверх суммы — эта и более старые без значений
        rows.sort_unstable_by_key(|r| std::cmp::Reverse(r.rev));
        let mut loaded = 0usize;
        let mut kept_rows = Vec::with_capacity(rows.len());
        for r in rows {
            let size = r.title.len()
                + r.body.len()
                + r.data.to_string().len()
                + r.tags.iter().map(String::len).sum::<usize>()
                + r.area.as_ref().map_or(0, String::len);
            if loaded + size > max_bytes {
                break;
            }
            loaded += size;
            kept_rows.push(r);
        }
        let mut revs: Vec<i64> = near
            .into_iter()
            .map(|(_, k)| k)
            .filter(|&k| k == e.rev || kept_rows.iter().any(|r| r.rev == k))
            .collect();
        revs.sort_unstable();
        Ok(EntryRevs {
            revs,
            older: kept_rows,
        })
    }

    /// Базы восстановления [`EntryTexts::restore`] — по возрастанию ревизии: текущая — запись `e`,
    /// прежние — снимки (снимка нет — ревизия пропущена).
    pub fn bases<'b>(&'b self, e: &'b Entry) -> Vec<EntryBase<'b>> {
        self.revs
            .iter()
            .filter_map(|&k| {
                if k == e.rev {
                    Some(EntryBase::from(e))
                } else {
                    self.older
                        .iter()
                        .find(|r| r.rev == k)
                        .map(|r| EntryBase::revision(e.id, r))
                }
            })
            .collect()
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

/// Маски всех строк значения `v` — в объект `masks` по порядку обхода (тот же, что у вида —
/// [`agent_view::number_value`]: массивы по элементам, объекты по полям).
fn add_value<'b>(v: &'b Value, masks: &mut ObjectMasks<'b>) {
    match v {
        Value::String(s) => {
            masks.add(s);
        }
        Value::Array(a) => a.iter().for_each(|x| add_value(x, masks)),
        Value::Object(o) => o.values().for_each(|x| add_value(x, masks)),
        _ => {}
    }
}

/// Строки значения `v` по порядку обхода (тот же, что у вида) — с путями, для правки на месте.
fn strings_mut<'v>(v: &'v mut Value, path: &mut String, out: &mut Vec<(String, &'v mut String)>) {
    match v {
        Value::String(s) => out.push((path.clone(), s)),
        Value::Array(a) => {
            for (i, x) in a.iter_mut().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                strings_mut(x, path, out);
                path.truncate(len);
            }
        }
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                strings_mut(x, path, out);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

/// Строки значения `v` по порядку обхода.
fn strings_ref<'v>(v: &'v Value, out: &mut Vec<&'v str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| strings_ref(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings_ref(x, out)),
        _ => {}
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

    fn id() -> Id {
        "01M7GA12W6ER5SSFDZQ98PCTC9".parse().unwrap()
    }

    /// Метка объекта `id()` ревизии 3 с номером `n`.
    fn m(n: u32) -> String {
        format!("‹secret-{}-{n}›", agent_view::object_tag(&id(), 3))
    }

    fn entry<'b>(
        title: &'b str,
        body: &'b str,
        data: &'b Value,
        tags: &'b [String],
    ) -> EntryBase<'b> {
        EntryBase {
            id: id(),
            rev: 3,
            title,
            body,
            data,
            tags,
            area: None,
        }
    }

    /// Вид записи агенту и его правка обратно (круг правок 5): метки `‹secret-T-N›` сквозные по
    /// полям (заголовок, тело, строки `data`, теги, область), прочие поля — `‹secret›`; метка,
    /// перенесённая в другое поле, строку `data` или тег, — со своим значением; номер вне объекта,
    /// чужая метка и `‹secret›` — литерал, предупреждение — сверх литералов исходника поля.
    #[test]
    fn restore_entry_fields() {
        let (k1, k2) = (key(21), key(22));
        let base_data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {k1}"), "n": 3},
            "options": [format!("a {k1}"), format!("b {k2}"), "c"],
        });
        let tags = vec![format!("t-{k1}"), "ops".to_string()];
        let mut shown = json!({"id": id().to_string(), "rev": 3, "title": format!("Ключ {k2}"),
                               "data": base_data.clone(), "tags": tags.clone(),
                               "reason": format!("из-за {k1}")});
        agent_view::number_entries(&mut shown);
        assert_eq!(shown["title"], format!("Ключ {}", m(1)));
        assert_eq!(shown["reason"], format!("из-за {M}"));
        let numbered = shown["data"].to_string();
        assert!(
            (2..=4).all(|n| numbered.contains(&m(n))) && !numbered.contains(&m(5)),
            "{numbered}"
        );
        // находка тега — весь тег (правило энтропии берёт и `t-`); нумерация — после `data`
        assert_eq!(shown["tags"][0], m(5));
        let num = |v: &Value| v.as_str().unwrap().to_string();
        let (n_checked, n_a, n_b) = (
            num(&shown["data"]["evidence"]["checked"]),
            num(&shown["data"]["options"][0]),
            num(&shown["data"]["options"][1]),
        );
        let title0 = format!("Ключ {k2}");
        let base = entry(&title0, "", &base_data, &tags);
        // строка `data` перенесена и в заголовок
        let mut title = format!("{} / {n_checked}", num(&shown["title"]));
        let mut data = json!({
            "kind": "decision",
            "evidence": {"checked": format!("{n_checked} (prod)"), "n": 3},
            "options": ["c", n_b.clone(), n_a.replace("a ", "a (правлено) ")],
            "new": format!("x {M} и {} и ‹secret-ffffff-1›", m(9)),
        });
        let mut seen = vec!["ops".to_string(), m(5), M.to_string()];
        let t = EntryTexts {
            title: Some(&mut title),
            data: Some(&mut data),
            tags: Some(&mut seen),
            ..EntryTexts::default()
        };
        assert_eq!(t.marks(), 10);
        let kept = t.restore(base, &[base], 3).unwrap().0;
        assert_eq!(kept, 4);
        assert_eq!(title, format!("Ключ {k2} / token: {k1}"));
        assert_eq!(data["evidence"]["checked"], format!("token: {k1} (prod)"));
        assert_eq!(
            data["options"],
            json!(["c", format!("b {k2}"), format!("a (правлено) {k1}")])
        );
        assert_eq!(data["new"], format!("x {M} и {} и ‹secret-ffffff-1›", m(9)));
        assert_eq!(
            seen,
            vec!["ops".to_string(), format!("t-{k1}"), M.to_string()]
        );
    }

    /// Метки двух ревизий записи в одной правке (финальное ревью M7.1, I-2): каждая — значение
    /// своей ревизии по её номеру (у прежней ключ `k2` — маска 2, у текущей — 3); ревизия, которой
    /// нет среди баз, — литерал.
    #[test]
    fn restore_entry_marks_of_two_revisions() {
        let (k1, k2, k3) = (key(31), key(32), key(33));
        let (t2, b2) = (format!("Ключ {k1}"), format!("token: {k2}\n"));
        let (t3, b3) = (format!("Ключ {k1}"), format!("новый: {k3}\ntoken: {k2}\n"));
        let null = Value::Null;
        let mut old = entry(&t2, &b2, &null, &[]);
        old.rev = 2;
        let cur = entry(&t3, &b3, &null, &[]);
        let m2 = |n: u32| format!("‹secret-{}-{n}›", agent_view::object_tag(&id(), 2));
        let mut body = format!(
            "из 2: token: {}\nиз 3: новый: {}\nиз 1: {}\n",
            m2(2),
            m(2),
            format_args!("‹secret-{}-1›", agent_view::object_tag(&id(), 1))
        );
        let t = EntryTexts {
            body: Some(&mut body),
            ..EntryTexts::default()
        };
        assert_eq!(t.tags().len(), 3);
        // значение из ревизии 2 при базе 3 — `secret_mark_from_rev: 2` (N-3)
        assert_eq!(t.restore(cur, &[old, cur], 3).unwrap(), (1, vec![2]));
        assert!(
            body.starts_with(&format!(
                "из 2: token: {k2}\nиз 3: новый: {k3}\nиз 1: ‹secret-"
            )),
            "{body}"
        );
    }

    /// Поле, где восстановленное значение вышло бы из-под маски, — 422 с путём поля и строкой,
    /// без значения.
    #[test]
    fn restore_entry_exposed() {
        let p = ["hunter", "41Qz9x"].concat();
        let (body0, data0) = (
            format!("ok\npassword = {p}\n"),
            json!({"e": {"m": format!("password = {p}")}, "o": [format!("password = {p}")]}),
        );
        let base = entry("t", &body0, &data0, &[]);
        let mut body = format!("ok\npassword {}\n", m(1));
        let mut data = json!({"e": {"m": format!("password {}", m(2))},
                              "o": [format!("пароль — {}", m(3))]});
        let t = EntryTexts {
            body: Some(&mut body),
            data: Some(&mut data),
            ..EntryTexts::default()
        };
        let e = t.restore(base, &[base], 3).unwrap_err();
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

    /// Дописка тела (круг правок 5): метка объекта — значение, покрытие — в итоге тела, строки
    /// отказа — дописки; чужая метка — литерал с предупреждением.
    #[test]
    fn restore_append_body() {
        let p = ["hunter", "52Qz9x"].concat();
        let body0 = format!("Стенд\n\npassword = {p}\n");
        let base = entry("t", &body0, &Value::Null, &[]);
        let mut more = format!("прод: password = {} и ‹secret-ffffff-1›", m(1));
        let t = EntryTexts {
            append_body: Some(&mut more),
            ..EntryTexts::default()
        };
        assert_eq!(t.restore(base, &[base], 3).unwrap().0, 1);
        assert_eq!(more, format!("прод: password = {p} и ‹secret-ffffff-1›"));
        let mut more = format!("прод:\nтекст\nпароль — {}", m(1));
        let t = EntryTexts {
            append_body: Some(&mut more),
            ..EntryTexts::default()
        };
        let s = t.restore(base, &[base], 3).unwrap_err().to_string();
        assert!(
            s.contains("append_body: строка 3") && !s.contains(&p),
            "{s}"
        );
        // у незакрытого забора в прежнем теле дописка — в коде: значение покрыто находкой флага
        let body1 = format!("```\nmysql -uroot -p{p} db\n");
        let base = entry("t", &body1, &Value::Null, &[]);
        let mut more = format!("mysql -uroot -p{} db2", m(1));
        let t = EntryTexts {
            append_body: Some(&mut more),
            ..EntryTexts::default()
        };
        assert_eq!(t.restore(base, &[base], 3).unwrap().0, 0);
    }

    /// Предел ревизий по меткам (N-2): текущая — всегда, прежних — не больше
    /// [`RESTORE_REVS_MAX`], ближайшие к текущей; номера ревизий значений (N-3) — без базовой, по
    /// возрастанию, без повторов; предупреждение — строка и список.
    #[test]
    fn nearest_revisions_and_from_rev() {
        let found: Vec<(String, i64)> = (1..=20).map(|k| (format!("t{k}"), k)).collect();
        let near = nearest_revisions(found, 20);
        let revs: Vec<i64> = near.iter().map(|(_, k)| *k).collect();
        assert_eq!(revs, (12..=20).rev().collect::<Vec<_>>());
        // текущей среди меток нет — прежних всё равно не больше предела
        let found: Vec<(String, i64)> = (1..=20).map(|k| (format!("t{k}"), k)).collect();
        assert_eq!(nearest_revisions(found, 30).len(), RESTORE_REVS_MAX);
        let pairs = vec![
            ("a".to_string(), 3),
            ("b".to_string(), 5),
            ("c".to_string(), 7),
        ];
        let from = [
            "c".to_string(),
            "a".to_string(),
            "c".to_string(),
            "x".to_string(),
        ];
        assert_eq!(from_revs(&from, &pairs, 7), vec![3]);
        assert_eq!(from_revs(&from, &pairs, 5), vec![3, 7]);
        let mut v = json!({"id": "x"});
        attach_from_rev(&mut v, &[]);
        assert_eq!(v, json!({"id": "x"}));
        attach_from_rev(&mut v, &[3, 7]);
        assert_eq!(v[SECRET_MARK_FROM_REV], json!([3, 7]));
        assert!(
            v["warnings"][0]
                .as_str()
                .unwrap()
                .starts_with("secret_mark_from_rev: 3, 7 — "),
            "{v}"
        );
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
