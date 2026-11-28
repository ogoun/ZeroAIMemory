//! Строгий query списков (M7.6 §9.3 п. 4, CR-24, признак `strict_list_query`):
//! `GET /spaces/{s}/vault`, `GET /spaces/{s}/entries`, `GET /spaces/{s}/entries/counts` —
//! неизвестный параметр — 400 «неизвестный параметр: <имя>», как у `/sessions` и `/cache`;
//! параметры, которые шлют WebUI (`web/src/lib/api.ts`) и `zam` (`crates/zam-cli/src`), —
//! принимаются.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

async fn get(a: &Api, uri: &str) -> (StatusCode, Value) {
    let r = a.t(Method::GET, uri, None).await;
    let j = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    (r.status, j)
}

/// Неизвестный параметр (в том числе `name` у Vault и опечатка у журналов) — 400 с именем
/// параметра; у `/sessions/unattributed` и `/cache` текст тот же.
#[tokio::test]
async fn strict_list_unknown_param_400() {
    let a = api("strict-unknown", |_| {}).await;
    let cases = [
        ("/api/v1/spaces/knowledge/vault?name=github", "name"),
        (
            "/api/v1/spaces/knowledge/vault?q=git&title=x&bogus=1",
            "bogus",
        ),
        (
            "/api/v1/spaces/knowledge/entries?kind=insight&titel=x",
            "titel",
        ),
        ("/api/v1/spaces/knowledge/entries?search=x", "search"),
        (
            "/api/v1/spaces/knowledge/entries/counts?kind=task&status=open",
            "status",
        ),
        (
            "/api/v1/spaces/knowledge/entries/counts?kind=idea&limit=5",
            "limit",
        ),
        ("/api/v1/sessions/unattributed?day=3", "day"),
    ];
    for (uri, name) in cases {
        let (status, j) = get(&a, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {j}");
        assert_eq!(j["type"], "about:blank", "{uri}: {j}");
        assert_eq!(
            j["detail"],
            format!("неизвестный параметр: {name}"),
            "{uri}: {j}"
        );
    }
}

/// Каждый параметр, который шлют WebUI и `zam` к трём маршрутам, принимается (200): строгий
/// разбор не задевает своих клиентов, в том числе `zam` 0.0.18 с сервером 0.0.19.
#[tokio::test]
async fn strict_list_client_params_accepted() {
    let a = api("strict-clients", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Строгий"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    for t in ["первая", "вторая"] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/spaces/knowledge/entries",
                Some(json!({"kind": "insight", "title": t, "body": "Тело.",
                            "data": {"kind": "discovery"}, "force": true})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "task", "title": "Задача", "body": "План."})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let task = r.json()["id"].as_str().unwrap().to_string();
    // курсор ленты — из ответа с limit=1
    let (status, j) = get(&a, "/api/v1/spaces/knowledge/entries?kind=insight&limit=1").await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let cursor = common::api::enc(j["next_cursor"].as_str().expect("курсор"));
    let k = "/api/v1/spaces/knowledge";
    let s = format!("/api/v1/spaces/{G}");
    let uris = [
        // WebUI `listEntries`: все фильтры ленты
        format!(
            "{k}/entries?kind=insight&status=active&certainty=axiom,theorem,hypothesis\
             &area=core&tag=t1&data_kind=decision&decided_by=human&limit=50&cursor={cursor}"
        ),
        format!("{k}/entries?kind=changelog&data_kind=feat&limit=50"),
        format!("{k}/entries?kind=idea&status=new&origin=human&priority=high&limit=50"),
        format!("{k}/entries?kind=task&status=open&from_idea=1&top_level=1&limit=50"),
        format!("{k}/entries?kind=task&parent={task}&limit=50"),
        format!("{k}/entries?kind=insight&deleted=only&limit=50"),
        format!("{k}/entries?kind=insight&noisy=1&limit=50"),
        format!("{k}/entries?kind=insight&since=2026-01-01&until=2027-01-01"),
        // `zam insight|task|question|idea|release list` (`list_entries`)
        format!("{k}/entries?kind=insight&status=active&limit=50&cursor={cursor}"),
        format!("{k}/entries?kind=task&status=open,in_progress&top_level=1&limit=200"),
        format!("{k}/entries?kind=question&status=open&limit=200"),
        format!("{k}/entries?kind=idea&status=new&origin=agent&limit=200"),
        format!("{s}/entries?kind=release"),
        // WebUI `entryCounts`
        format!("{k}/entries/counts?kind=idea&area=a&tag=t&origin=human&priority=normal"),
        format!("{k}/entries/counts?kind=task&from_idea=1&top_level=1"),
        format!("{k}/entries/counts?kind=task&parent={task}"),
        // WebUI `listVault`, `zam vault` (`q`), MCP `memory_vault` (`q`), нативный клиент (`title`)
        format!("{k}/vault"),
        format!("{k}/vault?q=git"),
        format!("{s}/vault?q=git&title=github"),
    ];
    for uri in &uris {
        let (status, j) = get(&a, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {j}");
    }
    // WebUI `listReports` (отчёты видит человек — сессия WebUI)
    for uri in [
        format!("{s}/entries?kind=report&tag=t&area=a&deleted=only&limit=20"),
        format!("{s}/entries?kind=report&limit=20&cursor={cursor}"),
    ] {
        let r = a.adm(Method::GET, &uri, None, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    }
}
