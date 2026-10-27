//! Шаг 0 поиска (§9.3, §9.4): действующая выжимка кэша для похожего запроса идёт первыми
//! элементами ответа (`kind: "cache"`, не больше двух).
//!
//! Пространство кэша — как у `POST /cache`: решение области (`scope.solution` или
//! `X-Zam-Solution`, база владельца; участнику — при действующем доступе) или Knowledge
//! пользователя; ключ — `scope_hash` области запроса ([`super::scope_key`]) и нормализованный
//! запрос, семантическое попадание — по dense-вектору запроса векторного канала. Выжимки
//! текущей сессии — первыми, других сессий — при `cache.cross_session` (с автором, машиной и
//! датой). Недействительная выжимка — строка `[cache, stale: <источники>]` без текста. Сбой
//! кэша поиск не валит: WARN и ответ без шага 0. Запрос с `scope.spaces` или
//! `all_solutions` шага 0 не делает: `scope_hash` (§9.4) этих полей не содержит, и выжимка
//! другой области выдавалась бы как своя **[решение исполнителя]**.
use axum::http::HeaderMap;
use serde_json::{Value, json};
use zam_store::{CACHE_LOOKUP_MAX, CacheHit, CacheQuery, Space, TargetFilter, cache_lookup};

use super::targets::{ScopeReq, request_solution};
use super::{md_inline, scope_key};
use crate::api::cache::{cache_ref, cache_space};
use crate::api::scope::{Mode, Role};
use crate::app::AppState;
use crate::auth::Principal;

/// Выжимка шага 0.
pub(super) struct CacheItem {
    hit: CacheHit,
    space: Space,
    author: Option<String>,
    /// Выжимка другой сессии: в строке — автор и машина.
    foreign: bool,
}

impl CacheItem {
    /// Токены выданного текста (у недействительной текста нет).
    pub(super) fn tokens(&self) -> usize {
        if self.hit.is_valid() {
            self.hit.entry.tokens.max(0) as usize
        } else {
            0
        }
    }
}

/// Выжимки шага 0 для запроса (см. модуль); `dense` — вектор запроса векторного канала.
#[allow(clippy::too_many_arguments)]
pub(super) async fn lookup(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    scope: &ScopeReq,
    filter: &TargetFilter,
    query: &str,
    session: Option<&str>,
    dense: Option<&[f32]>,
) -> Vec<CacheItem> {
    let query_norm = zam_store::normalize_query(query);
    // явный список пространств и «все решения» ключ кэша не описывает — шага 0 нет
    if query_norm.is_empty() || scope.spaces.is_some() || scope.all_solutions {
        return Vec::new();
    }
    let Ok(solution) = request_solution(scope.solution.as_deref(), headers) else {
        return Vec::new();
    };
    // нет доступа к решению — шага 0 нет (предупреждение уже дала область поиска)
    let sc = match cache_space(st, p, headers, solution.as_ref(), Mode::Read).await {
        Ok(sc) => sc,
        Err(e) => {
            tracing::debug!(error = %e, "поиск: пространство кэша выжимок недоступно");
            return Vec::new();
        }
    };
    let q = CacheQuery {
        space_id: sc.space.id,
        scope_hash: scope_key(scope, filter, solution.as_ref()),
        query_norm,
        query_vec: dense.map(<[f32]>::to_vec),
        session_id: session.map(str::to_string),
        cross_session: st.cfg.cache.cross_session,
        semantic_threshold: st.cfg.cache.semantic_threshold,
        limit: CACHE_LOOKUP_MAX,
        visible_spaces: matches!(sc.role, Role::Member(_)).then(|| vec![sc.space.id]),
    };
    let hits = match cache_lookup(&sc.db, &q).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "поиск: кэш выжимок не прочитан, ответ без шага 0");
            return Vec::new();
        }
    };
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        let foreign = hit.entry.session_id.as_deref() != session || session.is_none();
        let author = match st.system.get_user(hit.entry.author_user_id).await {
            Ok(u) => u.map(|u| u.name),
            Err(_) => None,
        };
        out.push(CacheItem {
            hit,
            space: sc.space.clone(),
            author,
            foreign,
        });
    }
    out
}

/// Склонение «источник» по числу.
fn sources_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, x) if x != 11 => "источник",
        (2..=4, x) if !(12..=14).contains(&x) => "источника",
        _ => "источников",
    }
}

/// JSON элемента шага 0.
pub(super) fn item_json(c: &CacheItem) -> Value {
    let e = &c.hit.entry;
    let valid = c.hit.is_valid();
    let mut o = json!({
        "kind": "cache",
        "id": e.id,
        "ref": cache_ref(e.id),
        "query": e.query,
        "state": if valid { "valid" } else { "stale" },
        "stale": c.hit.stale,
        "sources_count": e.sources.len(),
        "sources": e.sources.iter().map(|d| json!({"id": d.target, "rev": d.rev, "section": d.section})).collect::<Vec<_>>(),
        "author": c.author,
        "author_user_id": e.author_user_id,
        "machine": e.machine,
        "session_id": e.session_id,
        "other_session": c.foreign,
        "created_at": zam_store::time::fmt_ts(e.created_at),
        "space": c.space.name,
        "tokens": c.tokens(),
    });
    if let Value::Object(m) = &mut o {
        if let Some(g) = &c.space.solution_guid {
            m.insert("agent_solution_id".into(), json!(g));
        }
        if valid {
            m.insert("text".into(), json!(e.summary));
        }
    }
    o
}

/// Markdown элемента шага 0 (§9.3).
pub(super) fn item_md(c: &CacheItem) -> String {
    let e = &c.hit.entry;
    let date = e.created_at.strftime("%Y-%m-%d");
    let id = cache_ref(e.id);
    let query = md_inline(&e.query);
    if !c.hit.is_valid() {
        let list: Vec<String> = c
            .hit
            .stale
            .iter()
            .map(|s| match &s.section {
                Some(sec) => format!("{} › {} ({})", s.target, md_inline(sec), s.reason.as_str()),
                None => format!("{} ({})", s.target, s.reason.as_str()),
            })
            .collect();
        return format!(
            "[cache, stale: {}] «{query}» — выжимка {date} (id: {id})\n",
            list.join(", ")
        );
    }
    let n = e.sources.len();
    let mut line = format!(
        "[cache] «{query}» — выжимка {date}, {n} {}, все актуальны (id: {id})",
        sources_word(n)
    );
    if c.foreign {
        let author = c.author.as_deref().unwrap_or("?");
        line.push_str(&format!(" · автор {}", md_inline(author)));
        if let Some(m) = &e.machine {
            line.push_str(&format!(", машина {}", md_inline(m)));
        }
    }
    line.push('\n');
    for l in e.summary.lines() {
        line.push_str(l);
        line.push('\n');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_sources() {
        for (n, w) in [
            (1, "источник"),
            (2, "источника"),
            (4, "источника"),
            (5, "источников"),
            (11, "источников"),
            (12, "источников"),
            (21, "источник"),
            (23, "источника"),
        ] {
            assert_eq!(sources_word(n), w, "{n}");
        }
    }
}
