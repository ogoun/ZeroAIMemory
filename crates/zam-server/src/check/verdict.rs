//! Вердикт прогона ночной проверки (M7.3 §6.5 п. 5, решение координатора 48): чистая функция
//! [`decide`] над текущим прогоном и прежними ночными прогонами — без сервера и базы.
//!
//! - **База** — до [`BASE_OF`] последних законченных ночных прогонов с тем же `fp_hash`, годных в
//!   базу: вердикт не `regression` и не `degraded` (или норма принята человеком); после принятой
//!   нормы — только прогоны начиная с принятого. Контекст у них может различаться. Меньше пяти —
//!   `baseline` (`base_n`/5).
//! - **Регрессия** — по части `test` на пробах, общих для текущего прогона и всех прогонов базы:
//!   `hit_search` или `hit_prompt` ниже медианы на [`HIT_DROP_PP`] п. п. и больше,
//!   `negatives_shown` выше на [`NEG_RISE_PP`] п. п. и больше, `p95` поиска больше медианы в
//!   [`P95_RATIO`] раза **и** на [`P95_ABS_MS`] мс (без прогонов с `backup_overlap`); **потеря
//!   устойчивых** — из проб, найденных во всех прогонах базы, не нашлось ≥ 10 % и ≥ 3; переход в
//!   `degraded` (предыдущий ночной прогон — с векторами). В `test` меньше [`FEW_TEST`] общих проб —
//!   только потеря устойчивых и переход в `degraded`, пометка `few`.
//! - **Эпизод** держится, пока метрики не вернутся: прогоны `regression` в базу не входят, следующие
//!   ночи сравниваются с той же прежней базой; `episode` — id первого прогона эпизода; пометка
//!   `after_update` (контекст первого прогона эпизода отличается от контекста последнего прогона
//!   базы) — на всём эпизоде. Ручной прогон эпизода не начинает (в базу он не входит), но
//!   показывает идущий.
//! - **Смена отпечатка** (модель, устройство, реранкер, режим Qdrant, правила проверки) —
//!   `changed`: разница с медианой базы прежнего отпечатка без флагов, эпизод закрыт.
//! - **Устойчивое `degraded`** (и предыдущий ночной прогон — `degraded`) — вердикт `degraded` без
//!   строки дайджеста; такие прогоны в базу не входят.
//! - Пороги — стартовые claude-note, калибровка — после 14 ночных прогонов (журнал этапа).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_search::eval::{Split, median};

/// Прогонов в базе.
pub const BASE_OF: usize = 5;
/// Падение `hit_search`/`hit_prompt` к медиане базы, п. п.
pub const HIT_DROP_PP: f64 = 8.0;
/// Рост `negatives_shown` к медиане базы, п. п.
pub const NEG_RISE_PP: f64 = 10.0;
/// Рост `p95` поиска к медиане базы: во столько раз…
pub const P95_RATIO: f64 = 1.5;
/// …и не меньше чем на столько миллисекунд.
pub const P95_ABS_MS: f64 = 500.0;
/// Потеря устойчивых: доля…
pub const STEADY_LOSS_SHARE: f64 = 0.10;
/// …и не меньше проб.
pub const STEADY_LOSS_MIN: usize = 3;
/// Общих проб `test` меньше — только потеря устойчивых и переход в `degraded` (`few`).
pub const FEW_TEST: usize = 20;

/// Вид вердикта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Baseline,
    Ok,
    Regression,
    Changed,
    Degraded,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Baseline => "baseline",
            Kind::Ok => "ok",
            Kind::Regression => "regression",
            Kind::Changed => "changed",
            Kind::Degraded => "degraded",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        [
            Kind::Baseline,
            Kind::Ok,
            Kind::Regression,
            Kind::Changed,
            Kind::Degraded,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// Флаг регрессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagName {
    HitSearch,
    HitPrompt,
    NegativesShown,
    P95,
    LostSteady,
    Degraded,
}

impl FlagName {
    pub fn as_str(self) -> &'static str {
        match self {
            FlagName::HitSearch => "hit_search",
            FlagName::HitPrompt => "hit_prompt",
            FlagName::NegativesShown => "negatives_shown",
            FlagName::P95 => "p95",
            FlagName::LostSteady => "lost_steady",
            FlagName::Degraded => "degraded",
        }
    }
}

/// Флаг регрессии с числами: значение и медиана базы (доли — 0..=1, `p95` — мс), разница в п. п.
/// (у долей), потеря устойчивых — сколько из скольких.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flag {
    pub name: FlagName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_pp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steady: Option<usize>,
}

impl Flag {
    fn named(name: FlagName) -> Flag {
        Flag {
            name,
            value: None,
            median: None,
            delta_pp: None,
            lost: None,
            steady: None,
        }
    }
}

/// Что сменилось в отпечатке (`changed`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Changed {
    /// `model` | `device` | `reranker` | `qdrant` | `check_version`.
    pub what: String,
    pub from: Value,
    pub to: Value,
}

/// Медианы базы на общих пробах `test` (у `changed` — базы прежнего отпечатка).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Medians {
    pub hit_search: Option<f64>,
    pub hit_prompt: Option<f64>,
    pub negatives_shown: Option<f64>,
    pub p95: Option<f64>,
    /// Общих проб `test`.
    pub common: usize,
}

/// Вердикт прогона.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub kind: Kind,
    pub flags: Vec<Flag>,
    pub after_update: bool,
    /// Версия сервера, после обновления до которой начался эпизод.
    pub after_update_version: Option<String>,
    /// Первый прогон эпизода регрессии.
    pub episode: Option<Id>,
    /// В `test` меньше [`FEW_TEST`] общих проб.
    pub few: bool,
    pub base_n: usize,
    pub changed: Option<Changed>,
    pub compared_with: Option<Medians>,
}

impl Verdict {
    fn plain(kind: Kind, base_n: usize) -> Verdict {
        Verdict {
            kind,
            flags: Vec::new(),
            after_update: false,
            after_update_version: None,
            episode: None,
            few: false,
            base_n,
            changed: None,
            compared_with: None,
        }
    }

    /// JSON вердикта (`GET /check/status` → `last.verdict`, `GET /check/runs/{id}`; его читает
    /// `verdictText` WebUI): `{kind, flags, after_update, after_update_version?, few, episode?,
    /// base_n, base_of: 5, changed?, compared_with?}`.
    pub fn to_json(&self) -> Value {
        let mut o = json!({
            "kind": self.kind.as_str(),
            "flags": self.flags,
            "after_update": self.after_update,
            "few": self.few,
            "base_n": self.base_n,
            "base_of": BASE_OF,
        });
        if let Some(v) = &self.after_update_version {
            o["after_update_version"] = json!(v);
        }
        if let Some(e) = self.episode {
            o["episode"] = json!(e.to_string());
        }
        if let Some(c) = &self.changed {
            o["changed"] = json!(c);
        }
        if let Some(m) = &self.compared_with {
            o["compared_with"] = json!(m);
        }
        o
    }
}

/// Проба в прогоне: id пробы и решение запроса (у посторонних вопросов — строка на решение).
pub type Key = (Id, Option<Id>);

/// Исход пробы в прогоне.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub split: Split,
    pub negative: bool,
    /// Цель в бюджете основного поиска.
    pub found: bool,
    /// Цель среди подсказок к запросу; `None` — цель не подсказка или подсказки не считались.
    pub prompt_hit: Option<bool>,
    /// Посторонний вопрос: показана хоть одна подсказка; `None` — подсказки не считались.
    pub neg_shown: Option<bool>,
}

/// Текущий прогон.
#[derive(Debug, Clone, PartialEq)]
pub struct RunMetrics {
    pub id: Id,
    pub fp_hash: String,
    pub fingerprint: Value,
    pub context: Value,
    pub probes: HashMap<Key, Outcome>,
    pub p95: Option<f64>,
    pub degraded: bool,
    pub backup_overlap: bool,
}

/// Прежний законченный ночной прогон (новые первыми в `prev`). `probes` нужны только у прогонов
/// базы.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    pub id: Id,
    pub fp_hash: String,
    pub fingerprint: Value,
    pub context: Value,
    pub kind: Option<Kind>,
    pub episode: Option<Id>,
    pub accepted: bool,
    pub after_update: bool,
    pub after_update_version: Option<String>,
    pub degraded: bool,
    pub backup_overlap: bool,
    pub p95: Option<f64>,
    pub probes: HashMap<Key, Outcome>,
}

impl RunSummary {
    /// Прогон годится в базу: вердикт не `regression` и не `degraded`, или норма принята.
    pub fn eligible(&self) -> bool {
        self.accepted || !matches!(self.kind, Some(Kind::Regression | Kind::Degraded) | None)
    }

    /// Открытый эпизод регрессии: вердикт `regression`, норма не принята.
    fn open_episode(&self) -> bool {
        self.kind == Some(Kind::Regression) && !self.accepted
    }
}

/// Контекст решения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictCtx {
    /// Ночной прогон (ручной эпизода не начинает).
    pub nightly: bool,
    /// Версия сервера текущего прогона (для `after_update_version`).
    pub version: String,
}

/// База отпечатка `fp` (индексы `prev`, новые первыми): до [`BASE_OF`] годных прогонов этого
/// отпечатка, не дальше последней принятой нормы (она входит).
pub fn base_of(prev: &[RunSummary], fp: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, r) in prev.iter().enumerate() {
        if r.fp_hash == fp && r.eligible() {
            out.push(i);
            if out.len() == BASE_OF {
                break;
            }
        }
        if r.accepted {
            break;
        }
    }
    out
}

/// Доля `true` среди `Some` (`None` — нет значений).
fn share(v: impl Iterator<Item = bool>) -> Option<f64> {
    let (mut n, mut k) = (0usize, 0usize);
    for x in v {
        n += 1;
        k += usize::from(x);
    }
    (n > 0).then(|| k as f64 / n as f64)
}

/// Общие пробы с целью части `test`: в текущем прогоне и во всех прогонах базы.
fn common_test(cur: &HashMap<Key, Outcome>, base: &[&RunSummary]) -> Vec<Key> {
    let mut keys: Vec<Key> = cur
        .iter()
        .filter(|(_, o)| !o.negative && o.split == Split::Test)
        .filter(|(k, _)| base.iter().all(|b| b.probes.contains_key(*k)))
        .map(|(k, _)| *k)
        .collect();
    keys.sort();
    keys
}

/// Общие посторонние запросы (все части): в текущем прогоне и во всех прогонах базы с показом.
fn common_negatives(cur: &HashMap<Key, Outcome>, base: &[&RunSummary]) -> Vec<Key> {
    let mut keys: Vec<Key> = cur
        .iter()
        .filter(|(_, o)| o.negative && o.neg_shown.is_some())
        .filter(|(k, _)| {
            base.iter()
                .all(|b| b.probes.get(*k).is_some_and(|o| o.neg_shown.is_some()))
        })
        .map(|(k, _)| *k)
        .collect();
    keys.sort();
    keys
}

fn pp(x: f64) -> f64 {
    (x * 1000.0).round() / 10.0
}

/// Медианы базы `base` на общих пробах (`hit_*` — по `keys`, посторонние — по `neg`).
fn medians(base: &[&RunSummary], keys: &[Key], neg: &[Key]) -> Medians {
    let of = |f: &dyn Fn(&RunSummary) -> Option<f64>| -> Option<f64> {
        let v: Vec<f64> = base.iter().filter_map(|b| f(b)).collect();
        (v.len() == base.len()).then(|| median(&v)).flatten()
    };
    let hit = |b: &RunSummary| share(keys.iter().filter_map(|k| b.probes.get(k)).map(|o| o.found));
    let prompt = |b: &RunSummary| {
        let v: Vec<bool> = keys
            .iter()
            .filter_map(|k| b.probes.get(k).and_then(|o| o.prompt_hit))
            .collect();
        share(v.into_iter())
    };
    let negs = |b: &RunSummary| {
        share(
            neg.iter()
                .filter_map(|k| b.probes.get(k).and_then(|o| o.neg_shown)),
        )
    };
    let p95: Vec<f64> = base
        .iter()
        .filter(|b| !b.backup_overlap)
        .filter_map(|b| b.p95)
        .collect();
    Medians {
        hit_search: of(&hit),
        hit_prompt: of(&prompt),
        negatives_shown: of(&negs),
        p95: median(&p95),
        common: keys.len(),
    }
}

/// Что сменилось в отпечатке: первое различие по порядку полей.
fn changed_what(from: &Value, to: &Value) -> Changed {
    for what in ["model", "device", "reranker", "qdrant", "check_version"] {
        if from[what] != to[what] {
            return Changed {
                what: what.into(),
                from: from[what].clone(),
                to: to[what].clone(),
            };
        }
    }
    Changed {
        what: "check_version".into(),
        from: from["check_version"].clone(),
        to: to["check_version"].clone(),
    }
}

/// Эпизод регрессии текущего прогона: продолжается (предыдущий ночной прогон — открытый эпизод)
/// или начинается (ночной прогон); `after_update` — контекст отличается от контекста последнего
/// прогона базы.
fn episode(
    cur: &RunMetrics,
    prev: &[RunSummary],
    last_base: Option<&RunSummary>,
    ctx: &VerdictCtx,
) -> (Option<Id>, bool, Option<String>) {
    if let Some(l) = prev.first().filter(|l| l.open_episode()) {
        return (
            l.episode.or(Some(l.id)),
            l.after_update,
            l.after_update_version.clone(),
        );
    }
    let after = last_base.is_some_and(|b| b.context != cur.context);
    let version = after.then(|| ctx.version.clone());
    (ctx.nightly.then_some(cur.id), after, version)
}

/// Вердикт прогона `cur` по прежним законченным ночным прогонам `prev` (новые первыми) — см.
/// модуль.
pub fn decide(cur: &RunMetrics, prev: &[RunSummary], ctx: &VerdictCtx) -> Verdict {
    let last = prev.first();
    // смена отпечатка — сравнивать с прежней базой нельзя: разница без флагов, эпизод закрыт
    if let Some(l) = last
        && l.fp_hash != cur.fp_hash
    {
        let b: Vec<&RunSummary> = base_of(prev, &l.fp_hash)
            .into_iter()
            .map(|i| &prev[i])
            .collect();
        let keys = common_test(&cur.probes, &b);
        let neg = common_negatives(&cur.probes, &b);
        let mut v = Verdict::plain(Kind::Changed, b.len());
        v.changed = Some(changed_what(&l.fingerprint, &cur.fingerprint));
        v.compared_with = (!b.is_empty()).then(|| medians(&b, &keys, &neg));
        return v;
    }
    let base: Vec<&RunSummary> = base_of(prev, &cur.fp_hash)
        .into_iter()
        .map(|i| &prev[i])
        .collect();
    let last_base = base.first().copied();
    // поиск без векторов: переход — регрессия, устойчиво — `degraded` без строки
    if cur.degraded {
        if last.is_some_and(|l| !l.degraded) {
            let (episode, after_update, version) = episode(cur, prev, last_base, ctx);
            let mut v = Verdict::plain(Kind::Regression, base.len());
            v.flags.push(Flag::named(FlagName::Degraded));
            v.episode = episode;
            v.after_update = after_update;
            v.after_update_version = version;
            return v;
        }
        return Verdict::plain(Kind::Degraded, base.len());
    }
    if base.len() < BASE_OF {
        return Verdict::plain(Kind::Baseline, base.len());
    }
    let keys = common_test(&cur.probes, &base);
    let neg = common_negatives(&cur.probes, &base);
    let few = keys.len() < FEW_TEST;
    let m = medians(&base, &keys, &neg);
    let mut flags = Vec::new();
    // потеря устойчивых: найденные во всех прогонах базы
    let steady: Vec<&Key> = keys
        .iter()
        .filter(|k| {
            base.iter()
                .all(|b| b.probes.get(*k).is_some_and(|o| o.found))
        })
        .collect();
    let lost = steady
        .iter()
        .filter(|k| cur.probes.get(**k).is_some_and(|o| !o.found))
        .count();
    if lost >= STEADY_LOSS_MIN && lost as f64 >= STEADY_LOSS_SHARE * steady.len() as f64 {
        let mut f = Flag::named(FlagName::LostSteady);
        f.lost = Some(lost);
        f.steady = Some(steady.len());
        flags.push(f);
    }
    if !few {
        let hit = share(
            keys.iter()
                .filter_map(|k| cur.probes.get(k))
                .map(|o| o.found),
        );
        let prompt = share(
            keys.iter()
                .filter_map(|k| cur.probes.get(k).and_then(|o| o.prompt_hit)),
        );
        let negs = share(
            neg.iter()
                .filter_map(|k| cur.probes.get(k).and_then(|o| o.neg_shown)),
        );
        let mut flag_if =
            |name: FlagName, v: Option<f64>, med: Option<f64>, down: bool, pp_lim: f64| {
                if let (Some(v), Some(med)) = (v, med) {
                    // разница в п. п. с округлением до десятой: 0,92 − 1,0 = −7,999… — это −8
                    let d = pp(v - med);
                    let hit = if down { d <= -pp_lim } else { d >= pp_lim };
                    if hit {
                        let mut f = Flag::named(name);
                        f.value = Some(v);
                        f.median = Some(med);
                        f.delta_pp = Some(pp(v - med));
                        flags.push(f);
                    }
                }
            };
        flag_if(FlagName::HitSearch, hit, m.hit_search, true, HIT_DROP_PP);
        flag_if(FlagName::HitPrompt, prompt, m.hit_prompt, true, HIT_DROP_PP);
        flag_if(
            FlagName::NegativesShown,
            negs,
            m.negatives_shown,
            false,
            NEG_RISE_PP,
        );
        if !cur.backup_overlap
            && let (Some(v), Some(med)) = (cur.p95, m.p95)
            && v > P95_RATIO * med
            && v - med > P95_ABS_MS
        {
            let mut f = Flag::named(FlagName::P95);
            f.value = Some(v);
            f.median = Some(med);
            flags.push(f);
        }
    }
    let mut v = Verdict::plain(Kind::Ok, base.len());
    v.few = few;
    v.compared_with = Some(m);
    if !flags.is_empty() {
        let (episode, after_update, version) = episode(cur, prev, last_base, ctx);
        v.kind = Kind::Regression;
        v.flags = flags;
        v.episode = episode;
        v.after_update = after_update;
        v.after_update_version = version;
    }
    v
}

/// Строки дайджеста при регрессии (M7.3 §6.5 п. 7, ≤ 30 токенов): `{delta}` — разница `hit` в
/// п. п. со знаком.
pub const CHECK_DIGEST_LINE_RU: &str =
    "Ночная проверка памяти: ухудшение поиска (hit {delta} п. п.) — zam check report";
pub const CHECK_DIGEST_LINE_AFTER_UPDATE_RU: &str = "Ночная проверка памяти: ухудшение поиска после \
     обновления до {version} (hit {delta} п. п.) — zam check report";
pub const CHECK_DIGEST_LINE_EN: &str =
    "Nightly memory check: search got worse (hit {delta} pp) — zam check report";
pub const CHECK_DIGEST_LINE_AFTER_UPDATE_EN: &str = "Nightly memory check: search got worse after \
     the update to {version} (hit {delta} pp) — zam check report";

/// Язык строки дайджеста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineLang {
    Ru,
    En,
}

/// Разница в п. п. со знаком: `−12`, `+3`, `0` (минус — U+2212).
pub fn signed_pp(d: f64) -> String {
    let r = d.round();
    if r < 0.0 {
        format!("\u{2212}{}", -r)
    } else if r > 0.0 {
        format!("+{r}")
    } else {
        "0".into()
    }
}

/// Пояснение флага без `hit` для строки дайджеста и отчёта.
fn flag_detail(f: &Flag, lang: LineLang) -> String {
    let ru = lang == LineLang::Ru;
    match f.name {
        FlagName::LostSteady => {
            let (l, s) = (f.lost.unwrap_or(0), f.steady.unwrap_or(0));
            if ru {
                format!("потеря устойчивых: {l} из {s}")
            } else {
                format!("steady probes lost: {l} of {s}")
            }
        }
        FlagName::P95 => {
            let (v, m) = (f.value.unwrap_or(0.0), f.median.unwrap_or(0.0));
            if ru {
                format!("p95: {v:.0} мс при медиане {m:.0} мс")
            } else {
                format!("p95: {v:.0} ms vs median {m:.0} ms")
            }
        }
        FlagName::NegativesShown => {
            let d = signed_pp(f.delta_pp.unwrap_or(0.0));
            if ru {
                format!("подсказки к посторонним: {d} п. п.")
            } else {
                format!("hints for unrelated questions: {d} pp")
            }
        }
        FlagName::Degraded => {
            if ru {
                "degraded: поиск без векторов".into()
            } else {
                "degraded: search without vectors".into()
            }
        }
        FlagName::HitSearch | FlagName::HitPrompt => {
            let d = signed_pp(f.delta_pp.unwrap_or(0.0));
            if ru {
                format!("hit {d} п. п.")
            } else {
                format!("hit {d} pp")
            }
        }
    }
}

/// Строка дайджеста вердикта (только `regression`): у флага `hit_search` или `hit_prompt` —
/// константа с разницей `hit`, у прочих флагов — «ухудшение поиска (<флаг>: …)».
pub fn digest_line(v: &Verdict, lang: LineLang) -> Option<String> {
    if v.kind != Kind::Regression {
        return None;
    }
    let hit = v
        .flags
        .iter()
        .find(|f| f.name == FlagName::HitSearch)
        .or_else(|| v.flags.iter().find(|f| f.name == FlagName::HitPrompt));
    let version = v.after_update_version.as_deref().unwrap_or("?");
    let (plain, after) = match lang {
        LineLang::Ru => (CHECK_DIGEST_LINE_RU, CHECK_DIGEST_LINE_AFTER_UPDATE_RU),
        LineLang::En => (CHECK_DIGEST_LINE_EN, CHECK_DIGEST_LINE_AFTER_UPDATE_EN),
    };
    let tpl = if v.after_update { after } else { plain };
    let line = match hit {
        Some(f) => tpl.replace("{delta}", &signed_pp(f.delta_pp.unwrap_or(0.0))),
        None => {
            let detail = v
                .flags
                .first()
                .map(|f| flag_detail(f, lang))
                .unwrap_or_default();
            let hit_part = match lang {
                LineLang::Ru => "(hit {delta} п. п.)",
                LineLang::En => "(hit {delta} pp)",
            };
            tpl.replace(hit_part, &format!("({detail})"))
        }
    };
    Some(line.replace("{version}", version))
}

/// Пояснения всех флагов вердикта (отчёт `zam check report`, WebUI).
pub fn flag_texts(v: &Verdict, lang: LineLang) -> Vec<String> {
    v.flags.iter().map(|f| flag_detail(f, lang)).collect()
}

/// Вердикт из JSON прогона (`check_runs.verdict`) — для строки дайджеста и отчёта.
pub fn from_json(v: &Value) -> Option<Verdict> {
    let kind = Kind::parse(v["kind"].as_str()?)?;
    let flags = v["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let name = match f["name"].as_str()? {
                "hit_search" => FlagName::HitSearch,
                "hit_prompt" => FlagName::HitPrompt,
                "negatives_shown" => FlagName::NegativesShown,
                "p95" => FlagName::P95,
                "lost_steady" => FlagName::LostSteady,
                "degraded" => FlagName::Degraded,
                _ => return None,
            };
            Some(Flag {
                name,
                value: f["value"].as_f64(),
                median: f["median"].as_f64(),
                delta_pp: f["delta_pp"].as_f64(),
                lost: f["lost"].as_u64().map(|x| x as usize),
                steady: f["steady"].as_u64().map(|x| x as usize),
            })
        })
        .collect();
    Some(Verdict {
        kind,
        flags,
        after_update: v["after_update"].as_bool().unwrap_or(false),
        after_update_version: v["after_update_version"].as_str().map(str::to_string),
        episode: v["episode"].as_str().and_then(|s| s.parse().ok()),
        few: v["few"].as_bool().unwrap_or(false),
        base_n: v["base_n"].as_u64().unwrap_or(0) as usize,
        changed: None,
        compared_with: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пробы `n` штук части `test` с целью; `found(i)` — нашлась ли i-я.
    fn probes(ids: &[Id], found: impl Fn(usize) -> bool) -> HashMap<Key, Outcome> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    (*id, None),
                    Outcome {
                        split: Split::Test,
                        negative: false,
                        found: found(i),
                        prompt_hit: Some(found(i)),
                        neg_shown: None,
                    },
                )
            })
            .collect()
    }

    fn fp(model: &str) -> (String, Value) {
        (
            format!("fp-{model}"),
            json!({"model": model, "device": "cpu", "reranker": "r", "qdrant": "managed",
                   "check_version": 1}),
        )
    }

    fn ctx_v(v: &str) -> Value {
        json!({"server": v, "search": "s", "recall": "r"})
    }

    struct Hist {
        ids: Vec<Id>,
        prev: Vec<RunSummary>,
        model: String,
        version: String,
    }

    impl Hist {
        fn new(n: usize) -> Hist {
            Hist {
                ids: (0..n).map(|_| Id::new()).collect(),
                prev: Vec::new(),
                model: "m1".into(),
                version: "1.0".into(),
            }
        }

        /// Текущий прогон: найдено по `found`.
        fn cur(&self, found: impl Fn(usize) -> bool) -> RunMetrics {
            let (h, f) = fp(&self.model);
            RunMetrics {
                id: Id::new(),
                fp_hash: h,
                fingerprint: f,
                context: ctx_v(&self.version),
                probes: probes(&self.ids, found),
                p95: Some(100.0),
                degraded: false,
                backup_overlap: false,
            }
        }

        fn ctx(&self) -> VerdictCtx {
            VerdictCtx {
                nightly: true,
                version: self.version.clone(),
            }
        }

        /// Ночь: решение и запись в историю (новые первыми).
        fn night(&mut self, found: impl Fn(usize) -> bool) -> Verdict {
            self.night_with(found, |_| {})
        }

        fn night_with(
            &mut self,
            found: impl Fn(usize) -> bool,
            tweak: impl FnOnce(&mut RunMetrics),
        ) -> Verdict {
            let mut cur = self.cur(found);
            tweak(&mut cur);
            let v = decide(&cur, &self.prev, &self.ctx());
            self.prev.insert(
                0,
                RunSummary {
                    id: cur.id,
                    fp_hash: cur.fp_hash.clone(),
                    fingerprint: cur.fingerprint.clone(),
                    context: cur.context.clone(),
                    kind: Some(v.kind),
                    episode: v.episode,
                    accepted: false,
                    after_update: v.after_update,
                    after_update_version: v.after_update_version.clone(),
                    degraded: cur.degraded,
                    backup_overlap: cur.backup_overlap,
                    p95: cur.p95,
                    probes: cur.probes.clone(),
                },
            );
            v
        }
    }

    const ALL: fn(usize) -> bool = |_| true;
    /// Порча индекса: первые 5 из 30 не находятся (17 %, ≥ 3).
    const DAMAGED: fn(usize) -> bool = |i| i >= 5;

    /// §6.5 п. 5: первые пять ночей — `baseline` (N/5), шестая без превышений — `ok`.
    #[test]
    fn check_baseline_then_ok() {
        let mut h = Hist::new(30);
        for n in 0..5 {
            let v = h.night(ALL);
            assert_eq!((v.kind, v.base_n), (Kind::Baseline, n));
            assert_eq!(v.to_json()["base_of"], 5);
        }
        let v = h.night(ALL);
        assert_eq!(v.kind, Kind::Ok);
        assert_eq!(v.base_n, 5);
        assert!(!v.few && v.flags.is_empty() && v.episode.is_none());
        assert_eq!(digest_line(&v, LineLang::Ru), None);
        assert_eq!(v.compared_with.unwrap().hit_search, Some(1.0));
    }

    /// Регрессия после смены контекста (версии сервера): `after_update` с версией.
    #[test]
    fn check_regression_after_update_flagged() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        h.version = "2.0".into();
        let v = h.night(DAMAGED);
        assert_eq!(v.kind, Kind::Regression);
        assert!(v.after_update);
        assert_eq!(v.after_update_version.as_deref(), Some("2.0"));
        assert_eq!(v.episode, Some(h.prev[0].id));
        let line = digest_line(&v, LineLang::Ru).unwrap();
        assert!(line.contains("после обновления до 2.0"), "{line}");
        assert!(line.contains("hit \u{2212}17 п. п."), "{line}");
        let j = v.to_json();
        assert_eq!(j["after_update_version"], "2.0");
        assert_eq!(j["flags"][0]["name"], "lost_steady");
    }

    /// Смена контекста без превышения — `ok`, база копится по `fp_hash` (база не начинается
    /// заново).
    #[test]
    fn check_context_change_keeps_baseline() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        h.version = "2.0".into();
        let v = h.night(ALL);
        assert_eq!((v.kind, v.base_n), (Kind::Ok, 5));
        assert!(!v.after_update);
    }

    /// Три ночи без починки: регрессия держится, эпизод тот же, база прежняя (прогоны
    /// регрессии в неё не входят).
    #[test]
    fn check_steady_regression_persists() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        let first = h.night(DAMAGED);
        assert_eq!(first.kind, Kind::Regression);
        let ep = first.episode.unwrap();
        for _ in 0..3 {
            let v = h.night(DAMAGED);
            assert_eq!(v.kind, Kind::Regression);
            assert_eq!(v.episode, Some(ep));
            assert_eq!(v.base_n, 5);
            assert_eq!(v.compared_with.as_ref().unwrap().hit_search, Some(1.0));
            assert!(digest_line(&v, LineLang::Ru).is_some());
        }
    }

    /// Пометка `after_update` держится на всём эпизоде до его снятия, затем — `ok` без неё.
    #[test]
    fn check_after_update_kept_until_cleared() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        h.version = "2.0".into();
        assert!(h.night(DAMAGED).after_update);
        h.version = "2.1".into();
        let v = h.night(DAMAGED);
        assert!(v.after_update);
        assert_eq!(v.after_update_version.as_deref(), Some("2.0"));
        let ok = h.night(ALL);
        assert_eq!(ok.kind, Kind::Ok);
        assert!(!ok.after_update && ok.episode.is_none());
    }

    /// Прогоны регрессии в базу не входят: после эпизода база — пять прежних годных прогонов.
    #[test]
    fn check_regression_runs_not_in_base() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        h.night(DAMAGED);
        h.night(DAMAGED);
        let base = base_of(&h.prev, &h.prev[0].fp_hash);
        assert_eq!(base, vec![2, 3, 4, 5, 6]);
        assert!(
            base.iter()
                .all(|i| h.prev[*i].kind != Some(Kind::Regression))
        );
    }

    /// Снятие эпизода: первый прогон без превышения — `ok`, строки нет.
    #[test]
    fn check_regression_cleared_next_run() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        assert_eq!(h.night(DAMAGED).kind, Kind::Regression);
        let v = h.night(ALL);
        assert_eq!(v.kind, Kind::Ok);
        assert_eq!(digest_line(&v, LineLang::Ru), None);
        assert_eq!(base_of(&h.prev, &h.prev[0].fp_hash)[0], 0);
    }

    /// Смена отпечатка — `changed` с тем, что сменилось, без флагов; эпизод закрыт, база дальше
    /// копится по новому отпечатку.
    #[test]
    fn check_changed_closes_episode() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        assert_eq!(h.night(DAMAGED).kind, Kind::Regression);
        h.model = "m2".into();
        let v = h.night(DAMAGED);
        assert_eq!(v.kind, Kind::Changed);
        assert!(v.flags.is_empty() && v.episode.is_none());
        let c = v.changed.as_ref().unwrap();
        assert_eq!(
            (c.what.as_str(), c.from.as_str(), c.to.as_str()),
            ("model", Some("m1"), Some("m2"))
        );
        assert_eq!(v.compared_with.as_ref().unwrap().hit_search, Some(1.0));
        assert_eq!(digest_line(&v, LineLang::Ru), None);
        // дальше — база нового отпечатка: changed годен, это 1/5
        let next = h.night(DAMAGED);
        assert_eq!((next.kind, next.base_n), (Kind::Baseline, 1));
    }

    /// Переход «с векторами → без» — регрессия с флагом `degraded` и строкой, дальше —
    /// `degraded` без строки; такие прогоны в базу не входят.
    #[test]
    fn check_degraded_transition_rules() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        let v = h.night_with(ALL, |c| c.degraded = true);
        assert_eq!(v.kind, Kind::Regression);
        assert_eq!(v.flags[0].name, FlagName::Degraded);
        let line = digest_line(&v, LineLang::Ru).unwrap();
        assert!(line.contains("(degraded: поиск без векторов)"), "{line}");
        let v = h.night_with(ALL, |c| c.degraded = true);
        assert_eq!(v.kind, Kind::Degraded);
        assert_eq!(digest_line(&v, LineLang::Ru), None);
        let v = h.night(ALL);
        assert_eq!((v.kind, v.base_n), (Kind::Ok, 5));
    }

    /// Мало проб `test` (< 20): только потеря устойчивых, пометка `few`; падение `hit` без
    /// потери ≥ 3 — `ok`.
    #[test]
    fn check_few_only_steady_loss() {
        let history = || {
            let mut h = Hist::new(10);
            for _ in 0..5 {
                h.night(ALL);
            }
            h
        };
        let v = history().night(|i| i >= 2);
        assert_eq!(v.kind, Kind::Ok);
        assert!(v.few);
        let v = history().night(|i| i >= 3);
        assert_eq!(v.kind, Kind::Regression);
        assert!(v.few);
        let line = digest_line(&v, LineLang::Ru).unwrap();
        assert!(line.contains("(потеря устойчивых: 3 из 10)"), "{line}");
    }

    /// Пороги: −8 п. п. `hit_search` — регрессия, −7 — нет; `p95` ×1,5 **и** +500 мс; рост
    /// подсказок к посторонним +10 п. п.
    #[test]
    fn check_thresholds() {
        let mut h = Hist::new(100);
        for _ in 0..5 {
            h.night(ALL);
        }
        // 7 из 100 потеряны — −7 п. п., но потеря устойчивых (7 ≥ 3 и 7 % < 10 %) — нет
        assert_eq!(h.night(|i| i >= 7).kind, Kind::Ok);
        let v = h.night(|i| i >= 8);
        assert_eq!(v.kind, Kind::Regression);
        assert!(v.flags.iter().any(|f| f.name == FlagName::HitSearch));
        assert_eq!(
            digest_line(&v, LineLang::Ru).unwrap(),
            "Ночная проверка памяти: ухудшение поиска (hit \u{2212}8 п. п.) — zam check report"
        );
        let v = h.night_with(ALL, |c| c.p95 = Some(700.0));
        assert!(v.flags.iter().any(|f| f.name == FlagName::P95), "{v:?}");
        assert!(
            digest_line(&v, LineLang::En)
                .unwrap()
                .contains("p95: 700 ms vs median 100 ms")
        );
        // ×7 при +450 мс — нет; с копией посреди прогона — задержки не сравниваются
        assert_eq!(
            h.night_with(ALL, |c| c.p95 = Some(150.0 + 400.0)).kind,
            Kind::Ok
        );
        assert_eq!(
            h.night_with(ALL, |c| {
                c.p95 = Some(5000.0);
                c.backup_overlap = true;
            })
            .kind,
            Kind::Ok
        );
    }

    #[test]
    fn signed_pp_cases() {
        assert_eq!(signed_pp(-12.4), "\u{2212}12");
        assert_eq!(signed_pp(3.0), "+3");
        assert_eq!(signed_pp(0.2), "0");
    }

    /// JSON вердикта читается обратно (строка дайджеста строится из сохранённого вердикта).
    #[test]
    fn verdict_json_roundtrip() {
        let mut h = Hist::new(30);
        for _ in 0..5 {
            h.night(ALL);
        }
        let v = h.night(DAMAGED);
        let back = from_json(&v.to_json()).unwrap();
        assert_eq!(back.kind, v.kind);
        assert_eq!(back.flags, v.flags);
        assert_eq!(back.episode, v.episode);
        assert_eq!(
            digest_line(&back, LineLang::Ru),
            digest_line(&v, LineLang::Ru)
        );
    }
}
