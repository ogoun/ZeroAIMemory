//! Создание отчёта (спецификация M5.5 §4–6): `POST /spaces/{space}/entries` с
//! `multipart/form-data` (CLI `zam report`: часть `meta`, затем части `file`) или JSON с
//! `kind: report` и `images: [{name, data}]` (MCP, файлы в base64). Один путь для двух входов.
//!
//! **Порядок проверок:** доступ к пространству (`Mode::Write`: токен и участник `ro` — 403,
//! Human по токену — 404) → пространство не решения — 422 → сессия WebUI — 403 (отчёты пишет
//! агент, ответ владельца 12) → поле `refs`, заголовок, `data` (только `refs {commits, files,
//! notes, tasks}`), размер тела (413) → ссылки тела ([`report_refs`]) → файлы.
//!
//! **Ссылки тела** (по [`zam_markdown::parse`]: ссылки в коде не видны и не считаются):
//! `http(s):`, `mailto:`, якоря и `[[вики]]` — как есть; картинка с внешним адресом или `data:` —
//! 422 (CSP `img-src 'self'` и PDF её потеряют); `_attachments/…` — 422 (чужое вложение); прочая
//! схема, абсолютный путь, `\`, `..`, кавычка и символы `:*?<>|` — 422; остальная относительная
//! ссылка — файл отчёта, имя — путь из тела после percent-декодирования без ведущих `./`. Нет
//! такого файла — 422 с именем; файл без ссылки — 422 до записи байт; повтор имени — 400;
//! картинка (`![…]`) с MIME не из [`IMAGE_MIMES`] — 422.
//!
//! **Файлы** пишутся потоком в [`BlobStore`] владельца базы (решение участника — к владельцу) с
//! пределом `min(limits.blob_max_bytes, остаток reports.max_total_bytes)` (413); файлов больше
//! `reports.max_files` — 422; у JSON тело и декодированные файлы — до `reports.mcp_max_bytes`
//! (413 с подсказкой `zam report`), байты проверяются до записи. Ссылки тела переписываются на
//! `_attachments/<sha16>/<имя>` ([`rewrite_links`]), имя файла записи — последний сегмент пути;
//! запись, ревизия, FTS и `entry_attachments` — одна транзакция ([`create_report`]).
//!
//! **Ответ** — 201 (повтор `Idempotency-Key` — 200) ровно `{id, url}`: `url` — страница отчёта в
//! WebUI. Тела и ссылки `p:` агент не получает: отчёт для человека (ответ владельца 4). В логи
//! содержимое отчёта не попадает.
//!
//! Страница, файлы и миниатюры ленты — [`view`]; неизменность и права корзины —
//! [`super::scope::deny_report_mutation`], [`super::scope::report_trash_allowed`].
pub mod view;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use axum::Json;
use axum::body::Bytes;
use axum::extract::Multipart;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine as _;
use futures_util::TryStreamExt;
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};
use tokio_util::io::StreamReader;
use zam_markdown::{LinkKind, parse};
use zam_store::{
    BlobStore, EntryKind, NewEntry, NewReportFile, SpaceKind, attachment_link, create_report,
};

use super::blobs::{multipart_err, next_field, pick_mime, put_err, put_too_large, stream_err};
use super::entries::{
    REF_MAX_CHARS, REFS_MAX, check_size, check_title, idempotency_key, unprocessable,
};
use super::scope::{Mode, space_scope};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// MIME картинок отчёта (`![…](…)`): растровые и SVG (SVG — только как `<img>`).
pub const IMAGE_MIMES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/svg+xml",
];
/// Запас части `meta` сверх предела тела записи (JSON-обёртка), байт.
pub const META_OVERHEAD: usize = 64 * 1024;
/// Подсказка агенту при превышении предела MCP.
const CLI_HINT: &str = "большой отчёт — отправьте через zam report <файл.md> (multipart)";
/// Ключи `data.refs` отчёта.
const REFS_KEYS: [&str; 4] = ["commits", "files", "notes", "tasks"];
/// Символы, запрещённые в пути файла отчёта (кроме `\` и управляющих — их отсекает путь узла).
const PATH_FORBIDDEN: [char; 7] = ['"', ':', '*', '?', '<', '>', '|'];

/// Метаданные отчёта: часть `meta` multipart или JSON-тело (без `kind` и `images`).
#[derive(Debug)]
pub struct ReportMeta {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub area: Option<String>,
    pub data: ReportData,
    pub message: Option<String>,
}

/// Файл отчёта, на который ссылается тело.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRef {
    /// Путь как в теле: percent-декодированный, без ведущих `./`.
    pub name: String,
    /// На файл есть ссылка-картинка `![…](…)`.
    pub image: bool,
}

/// Вход создания отчёта.
pub enum ReportInput {
    Multipart(Multipart),
    /// Байты JSON (разбираются сразу в типы, без `Value`).
    Json(Bytes),
}

/// Файл отчёта в JSON (MCP).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportImage {
    name: String,
    /// base64 (стандартный алфавит, с выравниванием).
    data: String,
}

/// `Content-Type: multipart/form-data`.
pub fn is_multipart(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.trim_start()
                .get(..19)
                .is_some_and(|p| p.eq_ignore_ascii_case("multipart/form-data"))
        })
}

/// JSON-тело — отчёт (`"kind": "report"` на верхнем уровне). Без построения `Value`: прочие поля
/// пропускаются (`IgnoredAny`), значение `kind` не-строкой — не отчёт; не JSON или повтор `kind`
/// — не отчёт.
pub fn json_is_report(bytes: &[u8]) -> bool {
    /// Строка `kind` или `None` (значение другого типа пропускается без выделения памяти).
    struct KindStr(Option<String>);
    impl<'de> Deserialize<'de> for KindStr {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = KindStr;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("значение kind")
                }
                fn visit_str<E>(self, v: &str) -> Result<KindStr, E> {
                    Ok(KindStr(Some(v.to_string())))
                }
                fn visit_bool<E>(self, _: bool) -> Result<KindStr, E> {
                    Ok(KindStr(None))
                }
                fn visit_i64<E>(self, _: i64) -> Result<KindStr, E> {
                    Ok(KindStr(None))
                }
                fn visit_u64<E>(self, _: u64) -> Result<KindStr, E> {
                    Ok(KindStr(None))
                }
                fn visit_f64<E>(self, _: f64) -> Result<KindStr, E> {
                    Ok(KindStr(None))
                }
                fn visit_unit<E>(self) -> Result<KindStr, E> {
                    Ok(KindStr(None))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<KindStr, A::Error> {
                    while a.next_element::<IgnoredAny>()?.is_some() {}
                    Ok(KindStr(None))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<KindStr, A::Error> {
                    while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                    Ok(KindStr(None))
                }
            }
            d.deserialize_any(V)
        }
    }

    #[derive(Deserialize)]
    struct Probe {
        kind: Option<KindStr>,
    }
    serde_json::from_slice::<Probe>(bytes)
        .ok()
        .and_then(|p| p.kind)
        .and_then(|k| k.0)
        .is_some_and(|k| EntryKind::parse(&k) == Some(EntryKind::Report))
}

// ---- ссылки тела ----

/// Не больше ссылок тела на файлы отчёта (с повторами); больше — 422. Защищает разбор от тел
/// из сотен тысяч ссылок: 64 файла с запасом на многократные ссылки.
pub const LINKS_MAX: usize = 4096;
/// Кандидатов-определений `[метка]: путь` на метку: первые по тексту (действует первое вне кода),
/// остальные не рассматриваются.
pub const DEFS_PER_LABEL_MAX: usize = 16;

/// Локальная ссылка тела: имя файла и байты назначения в исходнике (что переписывать).
struct LocalLink {
    name: String,
    image: bool,
    anchor: Option<String>,
    dest: Range<usize>,
}

/// Разбор ссылок тела: файлы (без повторов) и назначения для переписывания.
struct Analysis {
    refs: Vec<LocalRef>,
    links: Vec<LocalLink>,
}

impl Analysis {
    fn of(body: &str) -> Result<Analysis, ApiError> {
        let links = local_links(body)?;
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut refs: Vec<LocalRef> = Vec::new();
        for l in &links {
            match index.get(l.name.as_str()) {
                Some(&i) => refs[i].image |= l.image,
                None => {
                    index.insert(&l.name, refs.len());
                    refs.push(LocalRef {
                        name: l.name.clone(),
                        image: l.image,
                    });
                }
            }
        }
        Ok(Analysis { refs, links })
    }

    /// Новое назначение ссылки (длина не зависит от sha256).
    fn target(l: &LocalLink, sha: &str) -> String {
        let mut new = attachment_link(sha, basename(&l.name));
        if let Some(a) = &l.anchor {
            new.push('#');
            new.push_str(&encode_anchor(a));
        }
        new
    }

    /// Длина тела после переписывания всех ссылок на файлы — известна до приёма файлов.
    fn rewritten_len(&self, body: &str) -> usize {
        let zero = "0".repeat(64);
        let mut seen = BTreeSet::new();
        let mut len = body.len();
        for l in &self.links {
            if seen.insert(l.dest.start) {
                len = len - l.dest.len() + Self::target(l, &zero).len();
            }
        }
        len
    }

    /// Тело с назначениями ссылок на файлы `files` (имя, sha256); прочие ссылки — как есть.
    fn rewrite(&self, body: &str, files: &[(String, String)]) -> String {
        let sha_of: HashMap<&str, &str> = files
            .iter()
            .map(|(n, s)| (n.as_str(), s.as_str()))
            .collect();
        let mut reps: BTreeMap<usize, (usize, String)> = BTreeMap::new();
        for l in &self.links {
            if let Some(sha) = sha_of.get(l.name.as_str()) {
                reps.insert(l.dest.start, (l.dest.end, Self::target(l, sha)));
            }
        }
        let mut out = String::with_capacity(body.len() + reps.len() * 32);
        let mut at = 0;
        for (start, (end, new)) in reps {
            out.push_str(&body[at..start]);
            out.push_str(&new);
            at = end;
        }
        out.push_str(&body[at..]);
        out
    }
}

/// Файлы отчёта, на которые ссылается тело, в порядке первого упоминания (без повторов).
/// Нарушение правил ссылок (см. модуль) — 422.
pub fn report_refs(body: &str) -> Result<Vec<LocalRef>, ApiError> {
    Analysis::of(body).map(|a| a.refs)
}

/// Тело с назначениями ссылок на файлы `files` (имя по [`LocalRef::name`], sha256), заменёнными на
/// `_attachments/<sha16>/<последний сегмент имени>` (якорь сохраняется). Форма ссылки (inline,
/// `<…>`, определение ссылки-сноски `[r]: путь`) и остальной текст не меняются; ссылки на
/// прочие имена — как есть. Тело, не прошедшее [`report_refs`], возвращается без изменений.
pub fn rewrite_links(body: &str, files: &[(String, String)]) -> String {
    match Analysis::of(body) {
        Ok(a) => a.rewrite(body, files),
        Err(_) => body.to_string(),
    }
}

/// Якорь ссылки: символы, ломающие синтаксис ссылки или читаемые Markdown особо, — `%XX`
/// (`%` не трогается: якорь уже в виде из исходника).
fn encode_anchor(a: &str) -> String {
    let mut out = String::with_capacity(a.len());
    for ch in a.chars() {
        if ch.is_ascii_control()
            || matches!(
                ch,
                ' ' | '(' | ')' | '<' | '>' | '&' | '\\' | '"' | '\'' | '[' | ']'
            )
        {
            out.push_str(&format!("%{:02X}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Последний сегмент пути — имя файла записи.
fn basename(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Ссылка в разборе: где искать назначение.
enum Dest {
    /// Назначение inline-ссылки (`(путь)` или `(<путь>)`): байты пути.
    Inline(Range<usize>),
    /// Ссылка-сноска `[текст][метка]`, `[метка][]` или `[метка]`: метка.
    Reference(String),
}

/// Где назначение локальной ссылки: inline — диапазон, сноска — номер метки.
enum Loc {
    Inline(Range<usize>),
    Label(usize),
}

/// Локальная ссылка до проверки назначения: индекс в `Doc::links`.
struct Pending {
    idx: usize,
    name: String,
    image: bool,
    anchor: Option<String>,
    loc: Loc,
}

/// `![[…]]` (встраивание Obsidian) вне кода — 422 с подсказкой: разбор его не видит ни
/// картинкой, ни ссылкой, и в отчёте он остался бы текстом. Проверка — разбором копии, где `!`
/// заменён пробелом (длина та же): вики-ссылка там, где в теле стоял `!`, — встраивание.
fn reject_wiki_embeds(body: &str) -> Result<(), ApiError> {
    if !body.contains("![[") {
        return Ok(());
    }
    let probe = body.replace("![[", " [[");
    let b = body.as_bytes();
    for l in parse(&probe).links {
        let bang = l.range.start.checked_sub(1);
        // `\![[…]]` — экранированный `!`: обычная вики-ссылка после текста «!»
        let escaped =
            bang.is_some_and(|i| b[..i].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 1);
        if l.kind == LinkKind::Wiki && bang.is_some_and(|i| b[i] == b'!') && !escaped {
            return Err(unprocessable(format!(
                "встраивание ![[{}]] в отчёте не поддерживается: картинка — ![подпись](путь)",
                short(&l.target)
            )));
        }
    }
    Ok(())
}

fn local_links(body: &str) -> Result<Vec<LocalLink>, ApiError> {
    reject_wiki_embeds(body)?;
    let doc = parse(body);
    let mut pending: Vec<Pending> = Vec::new();
    let mut labels: HashMap<String, usize> = HashMap::new();
    // бюджет поиска назначений: диапазоны ссылок пересекаются только вложением картинки
    let mut budget = body.len().saturating_mul(4).saturating_add(1 << 20);
    for (idx, l) in doc.links.iter().enumerate() {
        let Some(name) = classify(l)? else {
            continue;
        };
        if pending.len() >= LINKS_MAX {
            return Err(unprocessable(format!(
                "ссылок тела на файлы отчёта больше {LINKS_MAX}"
            )));
        }
        budget = budget.checked_sub(l.range.len()).ok_or_else(|| {
            unprocessable("ссылки тела: слишком глубокое вложение ссылок и картинок")
        })?;
        let loc = match locate(body, l.range.clone()) {
            Some(Dest::Inline(r)) => Loc::Inline(r),
            Some(Dest::Reference(label)) => {
                let n = labels.len();
                Loc::Label(*labels.entry(norm_label(&label)).or_insert(n))
            }
            None => return Err(unlocatable(&name)),
        };
        pending.push(Pending {
            idx,
            name,
            image: l.kind == LinkKind::Image,
            anchor: l.anchor.clone(),
            loc,
        });
    }
    let cands = ref_definitions(body, &labels);
    for p in &pending {
        if let Loc::Label(i) = p.loc
            && cands[i].is_empty()
        {
            return Err(unlocatable(&p.name));
        }
    }
    // Проверка назначений проверочным разбором: каждое заменяется меткой, ссылка с тем же
    // индексом обязана получить её целью. Сноска не прошла (определение в коде) — следующее
    // определение метки; inline не прошла или кандидаты кончились — 422. Разборов не больше
    // DEFS_PER_LABEL_MAX + 1.
    let n = doc.links.len();
    let mut pos = vec![0usize; labels.len()];
    let range_of = |p: &Pending, pos: &[usize]| -> Range<usize> {
        match &p.loc {
            Loc::Inline(r) => r.clone(),
            Loc::Label(i) => cands[*i][pos[*i]].clone(),
        }
    };
    let mut round = 0;
    loop {
        let mut marks: BTreeMap<usize, (usize, String)> = BTreeMap::new();
        for p in &pending {
            let r = range_of(p, &pos);
            let k = marks.len();
            marks
                .entry(r.start)
                .or_insert_with(|| (r.end, format!("zamreportmark{k}x")));
        }
        let mut probe = String::with_capacity(body.len() + marks.len() * 8);
        let mut at = 0;
        for (start, (end, mark)) in &marks {
            if *start < at {
                // пересечение назначений — форма, которую поиск не понял
                return Err(unprocessable(
                    "ссылки тела: не удалось разобрать назначения ссылок на файлы",
                ));
            }
            probe.push_str(&body[at..*start]);
            probe.push_str(mark);
            at = *end;
        }
        probe.push_str(&body[at..]);
        let pd = parse(&probe);
        let same = pd.links.len() == n;
        let mut failed: BTreeSet<usize> = BTreeSet::new();
        for p in &pending {
            let r = range_of(p, &pos);
            if same && pd.links[p.idx].target == marks[&r.start].1 {
                continue;
            }
            match p.loc {
                Loc::Inline(_) => return Err(unlocatable(&p.name)),
                Loc::Label(i) => {
                    failed.insert(i);
                }
            }
        }
        if failed.is_empty() {
            break;
        }
        round += 1;
        for i in failed {
            pos[i] += 1;
            if pos[i] >= cands[i].len() || round > DEFS_PER_LABEL_MAX {
                let p = pending
                    .iter()
                    .find(|p| matches!(p.loc, Loc::Label(j) if j == i));
                return Err(unlocatable(p.map_or("", |p| p.name.as_str())));
            }
        }
    }
    Ok(pending
        .iter()
        .map(|p| LocalLink {
            dest: range_of(p, &pos),
            name: p.name.clone(),
            image: p.image,
            anchor: p.anchor.clone(),
        })
        .collect())
}

fn unlocatable(name: &str) -> ApiError {
    unprocessable(format!(
        "ссылка на «{}»: назначение не найдено — используйте [текст](путь) или ![подпись](путь)",
        short(name)
    ))
}

/// Имя файла отчёта по ссылке; `None` — ссылка остаётся как есть; нарушение правил — 422.
fn classify(l: &zam_markdown::Link) -> Result<Option<String>, ApiError> {
    let image = match l.kind {
        LinkKind::Wiki => return Ok(None),
        LinkKind::Image => true,
        LinkKind::Markdown => false,
    };
    let t = l.target.trim();
    if t.is_empty() {
        return if image {
            Err(unprocessable("картинка без пути к файлу"))
        } else {
            // якорь в документе
            Ok(None)
        };
    }
    if let Some(scheme) = scheme_of(t) {
        if image {
            return Err(unprocessable(format!(
                "картинка «{}»: только файл отчёта — внешний адрес и data: не показываются \
                 (CSP img-src 'self') и теряются в PDF",
                short(t)
            )));
        }
        return if ["http", "https", "mailto"].contains(&scheme.as_str()) {
            Ok(None)
        } else {
            Err(unprocessable(format!(
                "ссылка «{}»: схема {scheme}: не поддерживается (http, https, mailto или файл отчёта)",
                short(t)
            )))
        };
    }
    if t.starts_with('/') {
        return Err(unprocessable(format!(
            "ссылка «{}»: абсолютный путь — только путь относительно файла отчёта",
            short(t)
        )));
    }
    if t.contains('\\') {
        return Err(unprocessable(format!(
            "ссылка «{}»: «\\» в пути — разделитель только «/»",
            short(t)
        )));
    }
    if zam_markdown::attachment_ref(t).is_some() {
        return Err(unprocessable(format!(
            "ссылка «{}»: вложение _attachments/… не принадлежит отчёту — приложите файл",
            short(t)
        )));
    }
    let decoded = percent_decode(t).ok_or_else(|| {
        unprocessable(format!(
            "ссылка «{}»: неверная percent-последовательность",
            short(t)
        ))
    })?;
    let mut name = decoded.as_str();
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    check_report_path(name)?;
    if name.split('/').next() == Some("_attachments") {
        return Err(unprocessable(format!(
            "ссылка «{}»: путь _attachments/… занят вложениями — переименуйте каталог",
            short(name)
        )));
    }
    Ok(Some(name.to_string()))
}

/// Путь файла отчёта: сегменты через `/`, без `..`, `.`, пустых сегментов, `\`, управляющих
/// символов и [`PATH_FORBIDDEN`]; последний сегмент — допустимое имя файла.
fn check_report_path(name: &str) -> Result<(), ApiError> {
    if let Some(c) = name.chars().find(|c| PATH_FORBIDDEN.contains(c)) {
        return Err(unprocessable(format!(
            "файл «{}»: символ «{c}» в пути не допускается",
            short(name)
        )));
    }
    zam_store::NodePath::parse(name)
        .and_then(|_| zam_store::blobs::check_filename(basename(name)))
        .map_err(|e| unprocessable(format!("файл «{}»: {e}", short(name))))
}

/// Начало строки для сообщения (до 120 символов).
fn short(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// Схема URL в нижнем регистре (`http`, `data`, `c` у `C:/…`); нет схемы — `None`.
fn scheme_of(t: &str) -> Option<String> {
    let colon = t.find(':')?;
    let s = &t[..colon];
    let mut b = s.bytes();
    let first = b.next()?;
    (first.is_ascii_alphabetic() && b.all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c)))
        .then(|| s.to_ascii_lowercase())
}

/// Декодирует `%XX`; `None` — неверная последовательность или не UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|h| h.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Где у ссылки (`range` из разбора) назначение.
fn locate(src: &str, range: Range<usize>) -> Option<Dest> {
    let text = src.get(range.clone())?;
    let b = text.as_bytes();
    let open = usize::from(b.first() == Some(&b'!'));
    if b.get(open) != Some(&b'[') {
        return None;
    }
    let close = match_bracket(b, open)?;
    let after = close + 1;
    match b.get(after) {
        Some(b'(') => {
            let mut i = after + 1;
            i = skip_ws(b, i, true);
            if b.get(i) == Some(&b'<') {
                let start = i + 1;
                let mut k = start;
                while k < b.len() {
                    match b[k] {
                        b'\\' if k + 1 < b.len() => k += 2,
                        b'>' => return Some(Dest::Inline(range.start + start..range.start + k)),
                        b'\n' | b'\r' => return None,
                        _ => k += 1,
                    }
                }
                None
            } else {
                let start = i;
                let mut depth = 0usize;
                let mut k = start;
                while k < b.len() {
                    let c = b[k];
                    if c == b'\\' && k + 1 < b.len() && b[k + 1].is_ascii_punctuation() {
                        k += 2;
                        continue;
                    }
                    if c.is_ascii_whitespace() || c.is_ascii_control() {
                        break;
                    }
                    if c == b'(' {
                        depth += 1;
                    } else if c == b')' {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    k += 1;
                }
                (k > start).then(|| Dest::Inline(range.start + start..range.start + k))
            }
        }
        Some(b'[') => {
            let end = match_bracket(b, after)?;
            let label = &text[after + 1..end];
            let label = if label.trim().is_empty() {
                &text[open + 1..close]
            } else {
                label
            };
            Some(Dest::Reference(label.to_string()))
        }
        _ => Some(Dest::Reference(text[open + 1..close].to_string())),
    }
}

/// Пробелы и табуляции, при `newline` — и один перевод строки.
fn skip_ws(b: &[u8], mut i: usize, newline: bool) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    if newline {
        if b.get(i) == Some(&b'\r') {
            i += 1;
        }
        if b.get(i) == Some(&b'\n') {
            i += 1;
        }
        while matches!(b.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
    }
    i
}

/// Индекс `]`, парной `[` в `b[open]`: с вложенными скобками, `\`-экранированием и
/// `код`-фрагментами.
fn match_bracket(b: &[u8], open: usize) -> Option<usize> {
    let mut runs: Option<BacktickRuns> = None;
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'`' => {
                let run = b[i..].iter().take_while(|&&c| c == b'`').count();
                let idx = runs.get_or_insert_with(|| BacktickRuns::new(b, open));
                // закрывающая последовательность той же длины — по индексу, без повторного прохода
                i = idx.closer_after(i + run, run).unwrap_or(i + run);
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Последовательности `` ` `` текста по длине (начала по возрастанию) — поиск закрывающей
/// за O(log n), как индекс последовательностей в CommonMark: незакрытые последовательности разной
/// длины не дают квадратичного прохода.
struct BacktickRuns(HashMap<usize, Vec<usize>>);

impl BacktickRuns {
    fn new(b: &[u8], from: usize) -> BacktickRuns {
        let mut m: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut i = from;
        while i < b.len() {
            if b[i] == b'`' {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                m.entry(n).or_default().push(i);
                i += n;
            } else {
                i += 1;
            }
        }
        BacktickRuns(m)
    }

    /// Конец первой последовательности длины `len`, начинающейся не раньше `pos`.
    fn closer_after(&self, pos: usize, len: usize) -> Option<usize> {
        let v = self.0.get(&len)?;
        let k = v.partition_point(|&s| s < pos);
        v.get(k).map(|s| s + len)
    }
}

/// Нормализованная метка ссылки-сноски: регистр и пробельные последовательности не важны.
fn norm_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Назначения определений `[метка]: путь` для меток `labels` (нормализованная метка → номер) за
/// один проход по строкам: по номеру метки — до [`DEFS_PER_LABEL_MAX`] кандидатов в порядке
/// текста (определение в блоке кода отсеивает проверочный разбор). Метка ищется в пределах
/// строки, отступ — до трёх пробелов.
fn ref_definitions(src: &str, labels: &HashMap<String, usize>) -> Vec<Vec<Range<usize>>> {
    let mut out: Vec<Vec<Range<usize>>> = vec![Vec::new(); labels.len()];
    if labels.is_empty() {
        return out;
    }
    let b = src.as_bytes();
    let mut line = 0usize;
    while line < b.len() {
        let next = b[line..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| line + p + 1);
        let mut i = line;
        let mut indent = 0;
        while indent < 3 && b.get(i) == Some(&b' ') {
            i += 1;
            indent += 1;
        }
        if b.get(i) == Some(&b'[')
            && let Some(close) = label_close(&b[..next], i)
            && b.get(close + 1) == Some(&b':')
            && let Some(&n) = labels.get(&norm_label(&src[i + 1..close]))
            && out[n].len() < DEFS_PER_LABEL_MAX
            && let Some(dest) = def_dest(b, close + 2)
        {
            out[n].push(dest);
        }
        line = next;
    }
    out
}

/// `]` метки определения в пределах строки `b[open..]` (без вложенных скобок, с `\`-экранированием).
fn label_close(b: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'[' | b'\n' | b'\r' => return None,
            b']' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Назначение определения после `:` (`<…>` или до пробела; допустим один перевод строки).
fn def_dest(b: &[u8], from: usize) -> Option<Range<usize>> {
    let mut k = skip_ws(b, from, true);
    if b.get(k) == Some(&b'<') {
        let start = k + 1;
        k = start;
        while k < b.len() && !matches!(b[k], b'>' | b'\n' | b'\r') {
            k += if b[k] == b'\\' { 2 } else { 1 };
        }
        (b.get(k) == Some(&b'>')).then_some(start..k)
    } else {
        let start = k;
        while k < b.len() && !b[k].is_ascii_whitespace() {
            k += 1;
        }
        (k > start).then_some(start..k)
    }
}

// ---- данные и метаданные ----

/// Не больше файлов в `images` (верх `reports.max_files`); больше — 400 при разборе.
const IMAGES_MAX: usize = 1024;

/// Список до `N` элементов: длиннее — ошибка разбора без накопления лишнего.
#[derive(Debug)]
struct Bounded<T, const N: usize>(Vec<T>);

impl<T, const N: usize> Default for Bounded<T, N> {
    fn default() -> Self {
        Bounded(Vec::new())
    }
}

impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for Bounded<T, N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T, const N: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for V<T, N> {
            type Value = Bounded<T, N>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "массив до {N} элементов")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(x) = a.next_element()? {
                    if out.len() == N {
                        return Err(serde::de::Error::custom(format!(
                            "массив длиннее {N} элементов"
                        )));
                    }
                    out.push(x);
                }
                Ok(Bounded(out))
            }
        }
        d.deserialize_seq(V::<T, N>(std::marker::PhantomData))
    }
}

/// `data` отчёта, проверенная при разборе без промежуточного `Value`: только `refs {commits,
/// files, notes, tasks}` — массивы строк (до 200 элементов до 1000 символов). Значения не по
/// схеме пропускаются (`IgnoredAny`), первое нарушение запоминается — 422 после проверки доступа.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReportData {
    /// Поле `refs` было в запросе.
    pub has_refs: bool,
    pub refs: BTreeMap<String, Vec<String>>,
    error: Option<String>,
}

impl ReportData {
    fn fail(&mut self, m: String) {
        self.error.get_or_insert(m);
    }

    /// Для записи: `{}` или `{refs: {…}}`.
    fn to_value(&self) -> Value {
        if self.has_refs {
            json!({ "refs": self.refs })
        } else {
            json!({})
        }
    }
}

/// Пропускает значение любого вида; `true` — это строка (её значение в `visit_str`).
macro_rules! skip_any {
    ($ty:ty, $on_other:expr) => {
        fn visit_bool<E>(self, _: bool) -> Result<$ty, E> {
            Ok($on_other)
        }
        fn visit_i64<E>(self, _: i64) -> Result<$ty, E> {
            Ok($on_other)
        }
        fn visit_u64<E>(self, _: u64) -> Result<$ty, E> {
            Ok($on_other)
        }
        fn visit_f64<E>(self, _: f64) -> Result<$ty, E> {
            Ok($on_other)
        }
        fn visit_unit<E>(self) -> Result<$ty, E> {
            Ok($on_other)
        }
    };
}

/// Элемент массива ссылок: строка в пределе, длиннее предела или не строка.
enum RefItem {
    Ok(String),
    TooLong,
    NotStr,
}

impl<'de> Deserialize<'de> for RefItem {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RefItem;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("строка")
            }
            fn visit_str<E>(self, v: &str) -> Result<RefItem, E> {
                Ok(if v.chars().count() <= REF_MAX_CHARS {
                    RefItem::Ok(v.to_string())
                } else {
                    RefItem::TooLong
                })
            }
            skip_any!(RefItem, RefItem::NotStr);
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<RefItem, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RefItem::NotStr)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<RefItem, A::Error> {
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RefItem::NotStr)
            }
        }
        d.deserialize_any(V)
    }
}

/// Массив ссылок `data.refs.<ключ>`: значения в пределах или текст нарушения (без имени ключа).
struct RefList(Result<Vec<String>, String>);

impl<'de> Deserialize<'de> for RefList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RefList;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("массив строк")
            }
            fn visit_str<E>(self, _: &str) -> Result<RefList, E> {
                Ok(RefList(Err("ожидается массив строк".into())))
            }
            skip_any!(RefList, RefList(Err("ожидается массив строк".into())));
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<RefList, A::Error> {
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RefList(Err("ожидается массив строк".into())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<RefList, A::Error> {
                let mut out = Vec::new();
                let mut err: Option<String> = None;
                let mut n = 0usize;
                while let Some(item) = a.next_element::<RefItem>()? {
                    n += 1;
                    match item {
                        _ if n > REFS_MAX => {
                            err.get_or_insert(format!("не больше {REFS_MAX} элементов"));
                        }
                        RefItem::Ok(s) => out.push(s),
                        RefItem::TooLong | RefItem::NotStr => {
                            err.get_or_insert(format!("строки до {REF_MAX_CHARS} символов"));
                        }
                    }
                }
                Ok(RefList(err.map_or(Ok(out), Err)))
            }
        }
        d.deserialize_any(V)
    }
}

/// Объект `data.refs`.
struct RefsIn(BTreeMap<String, Vec<String>>, Option<String>);

impl<'de> Deserialize<'de> for RefsIn {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RefsIn;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("объект refs")
            }
            fn visit_str<E>(self, _: &str) -> Result<RefsIn, E> {
                Ok(RefsIn(
                    BTreeMap::new(),
                    Some("data.refs: ожидается объект".into()),
                ))
            }
            skip_any!(
                RefsIn,
                RefsIn(BTreeMap::new(), Some("data.refs: ожидается объект".into()))
            );
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<RefsIn, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RefsIn(
                    BTreeMap::new(),
                    Some("data.refs: ожидается объект".into()),
                ))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<RefsIn, A::Error> {
                let mut out = BTreeMap::new();
                let mut err: Option<String> = None;
                while let Some(k) = a.next_key::<String>()? {
                    if !REFS_KEYS.contains(&k.as_str()) {
                        a.next_value::<IgnoredAny>()?;
                        err.get_or_insert(format!(
                            "data.refs.{}: неизвестное поле (commits, files, notes, tasks)",
                            short(&k)
                        ));
                        continue;
                    }
                    match a.next_value::<RefList>()?.0 {
                        Ok(v) => {
                            out.insert(k, v);
                        }
                        Err(m) => {
                            err.get_or_insert(format!("data.refs.{k}: {m}"));
                        }
                    }
                }
                Ok(RefsIn(out, err))
            }
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for ReportData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        const NOT_OBJECT: &str = "data должно быть JSON-объектом";
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ReportData;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("объект data")
            }
            fn visit_str<E>(self, _: &str) -> Result<ReportData, E> {
                Ok(ReportData {
                    error: Some(NOT_OBJECT.into()),
                    ..ReportData::default()
                })
            }
            skip_any!(
                ReportData,
                ReportData {
                    error: Some(NOT_OBJECT.into()),
                    ..ReportData::default()
                }
            );
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<ReportData, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ReportData {
                    error: Some(NOT_OBJECT.into()),
                    ..ReportData::default()
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<ReportData, A::Error> {
                let mut out = ReportData::default();
                while let Some(k) = a.next_key::<String>()? {
                    if k != "refs" {
                        a.next_value::<IgnoredAny>()?;
                        out.fail(format!(
                            "data.{}: у отчёта в data только refs {{commits, files, notes, tasks}}",
                            short(&k)
                        ));
                        continue;
                    }
                    let RefsIn(refs, err) = a.next_value()?;
                    out.has_refs = true;
                    out.refs = refs;
                    if let Some(m) = err {
                        out.fail(m);
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_any(V)
    }
}

/// `data` отчёта для записи; нарушение схемы — 422.
fn check_report_data(data: &ReportData) -> Result<Value, ApiError> {
    match &data.error {
        Some(m) => Err(unprocessable(m.clone())),
        None => Ok(data.to_value()),
    }
}

/// Тело запроса отчёта (JSON MCP или часть `meta`) — разбор из байт сразу в типы, без `Value`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportRequest {
    kind: Option<String>,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    tags: Bounded<String, { zam_store::entries::TAGS_MAX }>,
    area: Option<String>,
    #[serde(default)]
    data: ReportData,
    message: Option<String>,
    images: Option<Bounded<ReportImage, IMAGES_MAX>>,
    /// Поле записей журнала (ссылки на файлы памяти): у отчёта — 422 после проверки доступа.
    refs: Option<IgnoredAny>,
}

/// Разобранный запрос (до проверок доступа: только форма, ошибки — 400).
struct Parsed {
    meta: ReportMeta,
    images: Vec<ReportImage>,
    /// В запросе было поле `refs` (ссылки на файлы памяти): у отчёта — 422.
    refs_field: bool,
}

/// Метаданные из байт JSON; `json` — тело MCP (поле `images` допустимо, `kind` обязателен).
fn parse_meta(bytes: &[u8], json: bool) -> Result<Parsed, ApiError> {
    let r: ReportRequest = serde_json::from_slice(bytes)
        .map_err(|e| ApiError::BadRequest(format!("некорректное тело отчёта: {e}")))?;
    match r.kind.as_deref() {
        None if !json => {}
        Some("report") => {}
        _ => {
            return Err(ApiError::BadRequest(
                "метаданные отчёта: kind — только report".into(),
            ));
        }
    }
    if !json && r.images.is_some() {
        return Err(ApiError::BadRequest(
            "в multipart файлы отчёта — части file, а не поле images".into(),
        ));
    }
    Ok(Parsed {
        meta: ReportMeta {
            title: r.title,
            body: r.body,
            tags: r.tags.0,
            area: r.area,
            data: r.data,
            message: r.message,
        },
        images: r.images.map(|b| b.0).unwrap_or_default(),
        refs_field: r.refs.is_some(),
    })
}

/// Пределы файлов отчёта из конфигурации.
#[derive(Clone, Copy)]
struct Limits {
    max_files: usize,
    max_total: u64,
    blob_max: u64,
    mcp_max: u64,
}

impl Limits {
    fn of(st: &AppState) -> Limits {
        let r = &st.cfg.reports;
        Limits {
            max_files: r.max_files as usize,
            max_total: r.max_total_bytes,
            blob_max: st.cfg.limits.blob_max_bytes as u64,
            mcp_max: r.mcp_max_bytes,
        }
    }
}

/// Записанный файл отчёта.
struct Stored {
    name: String,
    sha256: String,
    mime: String,
    size: u64,
}

/// Учёт файлов при приёме: имена из тела, повторы, число, MIME картинок.
struct Intake {
    refs: HashMap<String, bool>,
    seen: BTreeSet<String>,
    lim: Limits,
}

impl Intake {
    fn new(refs: &[LocalRef], lim: Limits) -> Intake {
        Intake {
            refs: refs.iter().map(|r| (r.name.clone(), r.image)).collect(),
            seen: BTreeSet::new(),
            lim,
        }
    }

    /// Проверка файла до приёма байт; возвращает нормализованное имя и MIME.
    fn admit(&mut self, raw: &str, part_mime: Option<&str>) -> Result<(String, String), ApiError> {
        let mut name = raw;
        while let Some(rest) = name.strip_prefix("./") {
            name = rest;
        }
        if self.seen.contains(name) {
            return Err(ApiError::BadRequest(format!(
                "файл «{}» передан дважды",
                short(name)
            )));
        }
        if self.seen.len() >= self.lim.max_files {
            return Err(unprocessable(format!(
                "файлов в отчёте больше предела {}",
                self.lim.max_files
            )));
        }
        let Some(&image) = self.refs.get(name) else {
            return Err(unprocessable(format!(
                "файл «{}» не упомянут в теле отчёта: файл без ссылки не принимается",
                short(name)
            )));
        };
        self.seen.insert(name.to_string());
        let mime = pick_mime(basename(name), part_mime);
        if image && !IMAGE_MIMES.contains(&mime.as_str()) {
            return Err(unprocessable(format!(
                "файл «{}» вставлен картинкой, но его тип {mime} — не картинка ({})",
                short(name),
                IMAGE_MIMES.join(", ")
            )));
        }
        Ok((name.to_string(), mime))
    }

    /// Все упомянутые телом файлы получены — иначе 422 с первым недостающим.
    fn complete(&self, refs: &[LocalRef]) -> Result<(), ApiError> {
        match refs.iter().find(|r| !self.seen.contains(&r.name)) {
            Some(r) => Err(unprocessable(format!(
                "тело ссылается на файл «{}», но его нет среди файлов отчёта",
                short(&r.name)
            ))),
            None => Ok(()),
        }
    }
}

/// Какой предел размера сработал.
#[derive(Clone, Copy)]
enum Over {
    /// Файл больше `limits.blob_max_bytes`.
    File,
    /// Сумма файлов больше `reports.max_total_bytes`.
    Total,
}

fn too_large(lim: Limits, over: Over, name: &str) -> ApiError {
    match over {
        Over::Total => ApiError::PayloadTooLarge(format!(
            "файлы отчёта больше предела {} байт (reports.max_total_bytes)",
            lim.max_total
        )),
        Over::File => ApiError::PayloadTooLarge(format!(
            "файл «{}» больше предела {} байт (limits.blob_max_bytes)",
            short(name),
            lim.blob_max
        )),
    }
}

/// Файлы части `file` multipart: поток каждого — в [`BlobStore::put`].
async fn multipart_files(
    mp: &mut Multipart,
    intake: &mut Intake,
    store: &BlobStore,
) -> Result<Vec<Stored>, ApiError> {
    let lim = intake.lim;
    let mut used = 0u64;
    let mut out = Vec::new();
    while let Some(f) = next_field(mp).await? {
        if f.name() != Some("file") {
            return Err(unprocessable(format!(
                "часть «{}»: после meta — только части file",
                short(f.name().unwrap_or(""))
            )));
        }
        let raw = f
            .file_name()
            .map(str::to_string)
            .ok_or_else(|| ApiError::BadRequest("у части file нет имени файла".into()))?;
        let (name, mime) = intake.admit(&raw, f.content_type())?;
        let left = lim.max_total.saturating_sub(used);
        // поток обрывается на меньшем пределе — он и назван в ответе
        let (limit, over) = if left < lim.blob_max {
            (left, Over::Total)
        } else {
            (lim.blob_max, Over::File)
        };
        let reader = StreamReader::new(f.map_err(stream_err));
        let (sha256, size) = store.put(reader, limit).await.map_err(|e| {
            if put_too_large(&e) {
                too_large(lim, over, &name)
            } else {
                put_err(e, limit)
            }
        })?;
        used += size;
        out.push(Stored {
            name,
            sha256,
            mime,
            size,
        });
    }
    Ok(out)
}

/// Файлы JSON (MCP): все проверяются и декодируются до записи первого.
async fn json_files(
    images: Vec<ReportImage>,
    body_len: usize,
    intake: &mut Intake,
    refs: &[LocalRef],
    store: &BlobStore,
) -> Result<Vec<Stored>, ApiError> {
    let lim = intake.lim;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut total = body_len as u64;
    let mut used = 0u64;
    let mut decoded = Vec::with_capacity(images.len());
    for img in images {
        let (name, mime) = intake.admit(&img.name, None)?;
        // переносы строк и пробелы base64 (MIME, PEM) допустимы
        let clean: Vec<u8> = img
            .data
            .bytes()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let data = b64.decode(&clean).map_err(|e| {
            unprocessable(format!("файл «{}»: data — не base64: {e}", short(&name)))
        })?;
        let size = data.len() as u64;
        total += size;
        if total > lim.mcp_max {
            return Err(ApiError::PayloadTooLarge(format!(
                "отчёт через MCP больше предела {} байт (тело и файлы): {CLI_HINT}",
                lim.mcp_max
            )));
        }
        if size > lim.blob_max {
            return Err(too_large(lim, Over::File, &name));
        }
        if used + size > lim.max_total {
            return Err(too_large(lim, Over::Total, &name));
        }
        used += size;
        decoded.push((name, mime, data));
    }
    intake.complete(refs)?;
    let mut out = Vec::with_capacity(decoded.len());
    for (name, mime, data) in decoded {
        let (sha256, size) = store
            .put(&data[..], lim.blob_max)
            .await
            .map_err(|e| put_err(e, lim.blob_max))?;
        out.push(Stored {
            name,
            sha256,
            mime,
            size,
        });
    }
    Ok(out)
}

/// Часть `meta` multipart (первая): JSON до предела тела записи плюс запас; больше — 413.
async fn meta_part(st: &AppState, mp: &mut Multipart) -> Result<Vec<u8>, ApiError> {
    let first = || ApiError::BadRequest("первой должна идти часть meta (JSON отчёта)".into());
    let mut f = next_field(mp).await?.ok_or_else(first)?;
    if f.name() != Some("meta") {
        return Err(first());
    }
    let max = super::note_body_max(&st.cfg.limits).saturating_add(META_OVERHEAD);
    let mut buf = Vec::new();
    while let Some(chunk) = f.chunk().await.map_err(multipart_err)? {
        if buf.len() + chunk.len() > max {
            return Err(ApiError::PayloadTooLarge(format!(
                "часть meta больше предела {max} байт"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Адрес страницы отчёта в WebUI. Без `server.public_url` и `Host` — путь от корня сервера.
fn report_url(
    st: &AppState,
    headers: &HeaderMap,
    client: &Client,
    owner: zam_core::Id,
    guid: &str,
    id: zam_core::Id,
) -> String {
    let base = super::agent_setup::public_url(st, headers, *client)
        .unwrap_or_else(|_| st.cfg.server.base_path.clone());
    format!("{base}/#/u/{owner}/s/{guid}/j/reports/{id}")
}

/// Создание отчёта (см. модуль) → 201 `{id, url}`; повтор `Idempotency-Key` — 200.
pub async fn create(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    client: &Client,
    space: &str,
    input: ReportInput,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // форма JSON — до доступа (как у прочих записей); multipart читается после проверок
    let (mut mp, parsed) = match input {
        ReportInput::Json(bytes) => (None, Some(parse_meta(&bytes, true)?)),
        ReportInput::Multipart(mp) => (Some(mp), None),
    };
    let sc = space_scope(st, p, headers, space, Mode::Write).await?;
    if sc.space.kind != SpaceKind::Solution {
        return Err(unprocessable(
            "отчёты ведутся только в пространстве решения",
        ));
    }
    if p.is_human() {
        return Err(ApiError::Denied(
            "отчёты пишет агент (zam report, MCP memory_log): в WebUI отчёт не создаётся".into(),
        ));
    }
    let guid = sc
        .space
        .solution_guid
        .clone()
        .ok_or_else(|| ApiError::Internal("у пространства решения нет GUID".into()))?;
    let parsed = match (parsed, mp.as_mut()) {
        (Some(p), _) => p,
        (None, Some(mp)) => parse_meta(&meta_part(st, mp).await?, false)?,
        (None, None) => unreachable!("вход — JSON или multipart"),
    };
    let Parsed {
        meta,
        images,
        refs_field,
    } = parsed;
    if refs_field {
        return Err(unprocessable(
            "у отчёта нет поля refs: ссылки, на чём он основан, — data.refs",
        ));
    }
    check_title(&meta.title)?;
    let data = check_report_data(&meta.data)?;
    check_size(st, &meta.body)?;
    let attrib = attribution(p, headers, meta.message)?;
    let key = idempotency_key(headers)?;
    // разбор тела (до LINKS_MAX ссылок и DEFS_PER_LABEL_MAX + 2 разборов) — вне потока tokio
    let (body, analysis) = tokio::task::spawn_blocking(move || {
        let a = Analysis::of(&meta.body);
        (meta.body, a)
    })
    .await
    .map_err(|e| ApiError::aborted("разбор ссылок отчёта", &e))?;
    let analysis = analysis?;
    let refs = &analysis.refs;
    let lim = Limits::of(st);
    // до приёма байт: файлов не больше предела, переписанное тело — в пределе записи
    if refs.len() > lim.max_files {
        return Err(unprocessable(format!(
            "тело ссылается на {} файлов — больше предела {}",
            refs.len(),
            lim.max_files
        )));
    }
    let new_len = analysis.rewritten_len(&body);
    if new_len > st.cfg.limits.note_max_bytes {
        return Err(ApiError::PayloadTooLarge(format!(
            "тело отчёта со ссылками на файлы — {new_len} байт, предел {}",
            st.cfg.limits.note_max_bytes
        )));
    }
    let store = BlobStore::new(&st.data_dir.blobs_dir(), sc.owner);
    let mut intake = Intake::new(refs, lim);
    let stored = match mp.as_mut() {
        Some(mp) => {
            let s = multipart_files(mp, &mut intake, &store).await?;
            intake.complete(refs)?;
            s
        }
        None => json_files(images, body.len(), &mut intake, refs, &store).await?,
    };
    let pairs: Vec<(String, String)> = stored
        .iter()
        .map(|s| (s.name.clone(), s.sha256.clone()))
        .collect();
    let body = analysis.rewrite(&body, &pairs);
    debug_assert_eq!(body.len(), new_len);
    check_size(st, &body)?;
    let files: Vec<NewReportFile> = stored
        .iter()
        .map(|s| NewReportFile {
            sha256: s.sha256.clone(),
            filename: basename(&s.name).to_string(),
            mime: s.mime.clone(),
            size: s.size,
        })
        .collect();
    let bytes: u64 = stored.iter().map(|s| s.size).sum();
    let new = NewEntry {
        space_id: sc.space.id,
        kind: EntryKind::Report,
        title: meta.title,
        body,
        data,
        area: meta.area,
        tags: meta.tags,
        status: None,
        certainty: None,
        idempotency_key: key,
        triggers: Vec::new(),
        links: Vec::new(),
        refs: None,
    };
    let (entry, created) = create_report(&sc.db, new, files, &attrib).await?;
    tracing::info!(
        entry = %entry.id,
        files = stored.len(),
        bytes,
        created,
        "отчёт записан"
    );
    let url = report_url(st, headers, client, sc.owner, &guid, entry.id);
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(json!({ "id": entry.id, "url": url }))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(body: &str) -> Vec<(String, bool)> {
        report_refs(body)
            .unwrap()
            .into_iter()
            .map(|r| (r.name, r.image))
            .collect()
    }

    #[test]
    fn refs_kinds_and_order() {
        let body = "# T\n![a](figs/a.png) [d](data/r.csv) [w](https://x.org) [[Заметка]] \
                    [m](mailto:a@b.c) [#](#раздел) ![a2](./figs/a.png)\n\n`[c](code.png)`\n\n\
                    ```\n![x](in-code.png)\n```\n";
        assert_eq!(
            names(body),
            vec![
                ("figs/a.png".to_string(), true),
                ("data/r.csv".to_string(), false)
            ]
        );
    }

    #[test]
    fn hostile_links_rejected() {
        for body in [
            "[x](../a.csv)",
            "[x](/etc/passwd)",
            "[x](a\\b.csv)",
            "[x](C:/a.csv)",
            "![x](https://evil.org/a.png)",
            "![x](data:image/png;base64,AAAA)",
            "[x](_attachments/0123456789abcdef/a.png)",
            "[x](javascript:alert(1))",
            "[x](a\"b.csv)",
            "[x](a%ZZ.csv)",
            "![x]()",
        ] {
            assert!(report_refs(body).is_err(), "{body}");
        }
    }

    #[test]
    fn rewrite_inline_angle_percent_reference() {
        let sha = "ab".repeat(32);
        let body = "![p](<figs/my plot.png>) ![q](figs/my%20plot.png \"t\")\n\n\
                    [r]: figs/a.png\n\nсм. [данные][r] и [r]\n\n```\n[r]: figs/a.png\n```\n";
        let files = vec![
            ("figs/my plot.png".to_string(), sha.clone()),
            ("figs/a.png".to_string(), sha.clone()),
        ];
        let out = rewrite_links(body, &files);
        let l = format!("_attachments/{}/", &sha[..16]);
        assert_eq!(
            out,
            format!(
                "![p](<{l}my%20plot.png>) ![q]({l}my%20plot.png \"t\")\n\n\
                 [r]: {l}a.png\n\nсм. [данные][r] и [r]\n\n```\n[r]: figs/a.png\n```\n"
            )
        );
    }

    #[test]
    fn reference_definition_in_code_is_skipped() {
        let sha = "cd".repeat(32);
        let body = "```\n[r]: figs/a.png\n```\n\n[x][r]\n\n[r]: figs/b.png\n";
        assert_eq!(names(body), vec![("figs/b.png".to_string(), false)]);
        let out = rewrite_links(body, &[("figs/b.png".into(), sha.clone())]);
        assert!(out.starts_with("```\n[r]: figs/a.png\n```"), "{out}");
        assert!(
            out.ends_with(&format!("[r]: _attachments/{}/b.png\n", &sha[..16])),
            "{out}"
        );
    }

    #[test]
    fn image_inside_link_both_rewritten() {
        let sha = "ef".repeat(32);
        let body = "[![a](a.png)](b.png)";
        let out = rewrite_links(
            body,
            &[("a.png".into(), sha.clone()), ("b.png".into(), sha.clone())],
        );
        let l = format!("_attachments/{}/", &sha[..16]);
        assert_eq!(out, format!("[![a]({l}a.png)]({l}b.png)"));
    }

    fn data(v: Value) -> Result<Value, ApiError> {
        check_report_data(&serde_json::from_value::<ReportData>(v).unwrap())
    }

    #[test]
    fn data_refs_only() {
        assert_eq!(data(json!({})).unwrap(), json!({}));
        let ok = json!({"refs": {"commits": ["a"], "tasks": []}});
        assert_eq!(data(ok.clone()).unwrap(), ok);
        assert!(data(json!({"kind": "x"})).is_err());
        assert!(data(json!({"refs": {"issues": []}})).is_err());
        assert!(data(json!({"refs": {"files": "a"}})).is_err());
        assert!(data(json!({"refs": {"files": [1]}})).is_err());
        assert!(data(json!({"refs": []})).is_err());
        assert!(data(json!([])).is_err());
        let long: Vec<String> = (0..=REFS_MAX).map(|i| i.to_string()).collect();
        assert!(data(json!({"refs": {"commits": long}})).is_err());
        assert!(data(json!({"refs": {"notes": ["я".repeat(REF_MAX_CHARS + 1)]}})).is_err());
    }

    #[test]
    fn report_request_typed_no_value() {
        // лишнее поле — ошибка разбора (400), а не накопление в Value
        let pad = format!(
            r#"{{"kind": "report", "title": "t", "pad": [{}0]}}"#,
            "0,".repeat(1000)
        );
        assert!(matches!(
            parse_meta(pad.as_bytes(), true),
            Err(ApiError::BadRequest(_))
        ));
        // массив data вне схемы пропускается без хранения, нарушение — 422 позже
        let big = format!(
            r#"{{"kind": "report", "title": "t", "data": {{"refs": {{"commits": [{}0]}}}}}}"#,
            "0,".repeat(1000)
        );
        let p = parse_meta(big.as_bytes(), true).ok().unwrap();
        assert!(p.meta.data.refs.is_empty());
        assert!(check_report_data(&p.meta.data).is_err());
        let tags = format!(
            r#"{{"kind": "report", "title": "t", "tags": [{}"a"]}}"#,
            "\"a\",".repeat(zam_store::entries::TAGS_MAX)
        );
        assert!(matches!(
            parse_meta(tags.as_bytes(), true),
            Err(ApiError::BadRequest(_))
        ));
        let p = parse_meta(br#"{"title": "t", "refs": null}"#, false)
            .ok()
            .unwrap();
        assert!(!p.refs_field);
        let p = parse_meta(br#"{"title": "t", "refs": {"files": []}}"#, false)
            .ok()
            .unwrap();
        assert!(p.refs_field);
        assert!(parse_meta(br#"{"title": "t"}"#, true).is_err());
        assert!(parse_meta(br#"{"title": "t", "images": []}"#, false).is_err());
    }

    #[test]
    fn backtick_runs_not_quadratic() {
        // ~1 МБ незакрытых последовательностей длины 1..N внутри текста ссылки
        let mut text = String::new();
        let mut n = 1;
        while text.len() < 1 << 20 {
            text.push_str(&"`".repeat(n));
            text.push('a');
            n = n % 1400 + 1;
        }
        let body = format!("[{text}](a.png)");
        let t = std::time::Instant::now();
        let close = match_bracket(body.as_bytes(), 0);
        assert_eq!(close, Some(body.len() - "(a.png)".len() - 1));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
        // закрытая последовательность скрывает скобку
        assert_eq!(match_bracket(b"[a `]` b] x", 0), Some(8));
        assert_eq!(match_bracket(b"[a ``]` b] x", 0), Some(5));
    }

    #[test]
    fn wiki_embed_rejected_wiki_link_kept() {
        let e = report_refs("![[схема.png]]").unwrap_err().to_string();
        assert!(e.contains("![подпись](путь)"), "{e}");
        assert!(report_refs("см. [[Заметка]]").unwrap().is_empty());
        // экранированный `!` — текст «!» и вики-ссылка
        assert!(report_refs("Ура\\![[Заметка]]").unwrap().is_empty());
        assert!(report_refs("Ура\\\\![[x.png]]").is_err());
        // в коде — пример, не встраивание
        assert!(
            report_refs(
                "`![[x.png]]`

```
![[y.png]]
```
"
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn anchor_and_special_names_encoded() {
        let sha = "12".repeat(32);
        // `&amp;` в исходнике — имя `x&copy;.csv` (сущность раскрыта разбором)
        let body = "[d](data/x&amp;copy;.csv#a(b)c)";
        let refs = names(body);
        assert_eq!(refs, vec![("data/x&copy;.csv".to_string(), false)]);
        let out = rewrite_links(body, &[("data/x&copy;.csv".into(), sha.clone())]);
        assert_eq!(
            out,
            format!("[d](_attachments/{}/x%26copy;.csv#a%28b%29c)", &sha[..16])
        );
        // переписанная ссылка разбирается в то же имя
        let doc = parse(&out);
        let (_, name) = zam_markdown::attachment_ref(&doc.links[0].target).unwrap();
        assert_eq!(name.as_deref(), Some("x&copy;.csv"));
    }

    #[test]
    fn rewritten_len_known_before_files() {
        let sha = "34".repeat(32);
        let body = "![a](<figs/my plot.png>) [r] [x][r]\n\n[r]: b.csv#z\n";
        let a = Analysis::of(body).unwrap();
        let out = a.rewrite(
            body,
            &[
                ("figs/my plot.png".into(), sha.clone()),
                ("b.csv".into(), sha.clone()),
            ],
        );
        assert_eq!(a.rewritten_len(body), out.len(), "{out}");
    }

    #[test]
    fn links_limit_and_many_definitions_fast() {
        // 10 000 ссылок × 10 000 определений: отказ по LINKS_MAX без квадратичной работы
        let mut body = "[x][r] ".repeat(10_000);
        body.push_str("\n\n");
        body.push_str(&"[r]: a.png\n".repeat(10_000));
        let t = std::time::Instant::now();
        let e = report_refs(&body).unwrap_err().to_string();
        assert!(e.contains(&LINKS_MAX.to_string()), "{e}");
        assert!(
            t.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            t.elapsed()
        );
        // в пределе: 4000 ссылок, 10 000 определений (первые — в блоке кода)
        let mut body = "```\n".to_string();
        body.push_str(&"[r]: in-code.png\n".repeat(10));
        body.push_str("```\n\n");
        body.push_str(&"[x][r] ".repeat(4000));
        body.push_str("\n\n");
        body.push_str(&"[r]: a.png\n".repeat(10_000));
        let t = std::time::Instant::now();
        assert_eq!(names(&body), vec![("a.png".to_string(), false)]);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            t.elapsed()
        );
        // все кандидаты метки в коде (больше DEFS_PER_LABEL_MAX) — 422, а не перебор
        let mut body = "```\n".to_string();
        body.push_str(&"[r]: in-code.png\n".repeat(DEFS_PER_LABEL_MAX + 5));
        body.push_str("```\n\n[x][r]\n\n[r]: a.png\n");
        assert!(report_refs(&body).is_err());
    }

    #[test]
    fn json_report_probe() {
        assert!(json_is_report(
            br#"{"title": "t", "kind": "report", "x": [1, {"a": 2}]}"#
        ));
        assert!(!json_is_report(br#"{"kind": "changelog"}"#));
        assert!(!json_is_report(br#"{"kind": ["report"]}"#));
        assert!(!json_is_report(br#"{"kind": "report", "kind": "report"}"#));
        assert!(!json_is_report(b"{not json"));
        assert!(!json_is_report(b"[]"));
    }
}
