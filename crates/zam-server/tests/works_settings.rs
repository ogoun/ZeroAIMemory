//! Настройки исполнителя внутренних работ `works.*` (M7.3 §6.1 п. 2, задача 1): `GET/PATCH
//! /auth/settings {works}`, умолчания сервера, аудит, 403 по токену, 422 режима `model` без
//! подключения и пределов выше потолка, 400 `works.pause`, вызов снятия работ при смене режима.
mod common;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::{ModelAccess, WorksMode};
use zam_store::works::WorkKind;

use common::api::{Api, api, ok};

const SETTINGS: &str = "/api/v1/auth/settings";

/// Сервер теста; `tweak` — правка конфигурации.
async fn setup(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> Api {
    api(&format!("works-set-{name}"), tweak).await
}

async fn patch(a: &Api, body: Value) -> common::api::Resp {
    a.adm(Method::PATCH, SETTINGS, Some(body), None).await
}

async fn stored(a: &Api) -> Vec<(String, Value)> {
    a.st.system.user_settings(a.st.admin_id).await.unwrap()
}

async fn audit_details(a: &Api) -> Vec<Value> {
    a.st.system
        .list_audit(500, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == "user.settings.update")
        .map(|r| r.details)
        .collect()
}

/// Запись личного Vault администратора с адресом модели (RFC 5737; вне LAN — `https`: `http`
/// вне LAN `PATCH works.model` отвергает, задача 2); id записи.
async fn vault_item(a: &Api, title: &str) -> String {
    // фиктивный ключ — склейкой (push protection)
    let secret = format!("{}{}", "sk-test-", "Zm9vYmFyYmF6cXV4");
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(json!({
                "title": title,
                "data": {"type": "token", "url": "https://192.0.2.10:8443/v1"},
                "secret": secret,
            })),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    r.json()["id"].as_str().unwrap().to_string()
}

/// Подключение `works.model` к записи `vault` (имя или `v:<id>`).
fn conn(vault: &str) -> Value {
    json!({
        "protocol": "openai",
        "vault": vault,
        "model": "qwen3-32b",
        "max_tokens": 4000,
        "body_extra": {"chat_template_kwargs": {"enable_thinking": false}},
    })
}

/// §6.1 п. 2: `GET` — умолчания (`mode` = `[works] default_mode`, `check = on`, без подключения и
/// пределов); `PATCH {works: {mode: "agent"}}` — `works.mode = "agent"` в ответе, строка в
/// `user_settings`, аудит `{key, from, to}`; выбор, равный `default_mode`, не хранится (строка
/// снимается); подключение — по имени записи Vault, хранится id, аудит `works.model` — без
/// `body_extra`.
#[tokio::test]
async fn works_settings_patch_and_defaults() {
    let a = setup("defaults", |_| {}).await;
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    ok(&r, StatusCode::OK);
    let w = &r.json()["works"];
    assert_eq!(w["mode"], "off", "{w}");
    assert_eq!(w["check"], "on", "{w}");
    assert_eq!(w["kinds"], json!({}), "{w}");
    assert!(w["model"].is_null() && w["limits"].is_null() && w["pause"].is_null());
    assert_eq!(
        w["effective"]["probes"],
        json!({"mode": "off", "reason": null})
    );
    // прежние поля ответа на месте
    assert!(r.json()["hooks"].is_object() && r.json()["ideas"].is_object());

    let r = patch(&a, json!({"works": {"mode": "agent"}})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["works"]["mode"], "agent");
    assert_eq!(r.json()["works"]["effective"]["rules"]["mode"], "agent");
    assert!(
        stored(&a)
            .await
            .contains(&("works.mode".to_string(), json!("agent")))
    );
    assert!(
        audit_details(&a)
            .await
            .contains(&json!({"key": "works.mode", "from": "off", "to": "agent"}))
    );
    // выбор, равный умолчанию сервера, не хранится: строка снимается
    let r = patch(&a, json!({"works": {"mode": "off"}})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["works"]["mode"], "off");
    assert!(
        !stored(&a).await.iter().any(|(k, _)| k == "works.mode"),
        "{:?}",
        stored(&a).await
    );
    // выбор умолчания без строки — ни записи, ни аудита
    let before = audit_details(&a).await.len();
    ok(
        &patch(&a, json!({"works": {"mode": "off"}})).await,
        StatusCode::OK,
    );
    assert_eq!(audit_details(&a).await.len(), before);

    // вид, проверка, пределы
    let r = patch(
        &a,
        json!({"works": {"kinds": {"probes": "agent"}, "check": "off",
                         "limits": {"daily_tokens": 1000, "monthly_tokens": 20000}}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let w = &r.json()["works"];
    assert_eq!(w["kinds"], json!({"probes": "agent"}), "{w}");
    assert_eq!(w["effective"]["probes"]["mode"], "agent");
    assert_eq!(w["effective"]["rules"]["mode"], "off");
    assert_eq!(w["check"], "off");
    assert_eq!(
        w["limits"],
        json!({"daily_tokens": 1000, "monthly_tokens": 20000})
    );
    // снять переопределение вида — null
    let r = patch(
        &a,
        json!({"works": {"kinds": {"probes": null}, "check": "on"}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["works"]["kinds"], json!({}));
    let keys: Vec<String> = stored(&a).await.into_iter().map(|(k, _)| k).collect();
    assert!(
        !keys
            .iter()
            .any(|k| k == "works.kind.probes" || k == "works.check"),
        "{keys:?}"
    );

    // подключение по имени записи Vault: хранится id, ответ — `v:<id>`
    let vid = vault_item(&a, "llm-test").await;
    let r = patch(&a, json!({"works": {"model": conn("llm-test")}})).await;
    ok(&r, StatusCode::OK);
    let m = &r.json()["works"]["model"];
    assert_eq!(m["vault"], format!("v:{vid}"), "{m}");
    assert_eq!(m["model"], "qwen3-32b");
    assert_eq!(m["max_tokens"], 4000);
    assert_eq!(m["auth"], "key");
    assert_eq!(m["token_field"], "auto");
    assert_eq!(
        m["body_extra"],
        json!({"chat_template_kwargs": {"enable_thinking": false}})
    );
    // аудит подключения — без body_extra (§6.1 п. 2)
    let row = audit_details(&a)
        .await
        .into_iter()
        .find(|d| d["key"] == "works.model")
        .expect("аудит works.model");
    assert!(row["from"].is_null(), "{row}");
    assert_eq!(row["to"]["model"], "qwen3-32b", "{row}");
    assert!(row["to"].get("body_extra").is_none(), "{row}");
    assert!(!row.to_string().contains("enable_thinking"), "{row}");
    // `v:<id>` — то же подключение; без изменений — без аудита
    let n = audit_details(&a).await.len();
    ok(
        &patch(&a, json!({"works": {"model": conn(&format!("v:{vid}"))}})).await,
        StatusCode::OK,
    );
    assert_eq!(audit_details(&a).await.len(), n);
    // режим `model` с подключением — действует
    let r = patch(&a, json!({"works": {"mode": "model"}})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(
        r.json()["works"]["effective"]["probes"],
        json!({"mode": "model", "reason": null})
    );
    // неизвестная запись Vault — 422; неизвестное поле или вид, не тот тип — 400
    ok(
        &patch(&a, json!({"works": {"model": conn("нет-такой")}})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    );
    for bad in [
        json!({"works": {}}),
        json!({"works": {"auto": true}}),
        json!({"works": {"mode": 1}}),
        json!({"works": {"kinds": {"future": "agent"}}}),
        json!({"works": {"kinds": {"probes": 1}}}),
        json!({"works": {"check": true}}),
        json!({"works": {"limits": {"daily_tokens": -1, "monthly_tokens": 1}}}),
    ] {
        let r = patch(&a, bad.clone()).await;
        ok(&r, StatusCode::BAD_REQUEST);
        assert!(r.text().contains("works"), "{bad}: {}", r.text());
    }
    for bad in [
        json!({"works": {"mode": "auto"}}),
        json!({"works": {"kinds": {"probes": "manual"}}}),
        json!({"works": {"check": "nightly"}}),
    ] {
        ok(&patch(&a, bad).await, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // администратор — тот же ответ через `/admin/users/{id}/settings`
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/admin/users/{}/settings", a.admin_id()),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["works"]["mode"], "model");
    // `load` — то же, что в ответе
    let u = zam_server::works::settings::load(&a.st.system, a.st.admin_id)
        .await
        .unwrap();
    assert_eq!(u.mode, Some(WorksMode::Model));
    assert_eq!(u.model.as_ref().unwrap().vault.to_string(), vid);
    assert!(u.check);
}

/// §6.2 п. 1 (ревью r1, Minor 2): пределы полей подключения — 422 с именем поля на каждой
/// границе, значения на границе принимаются: `model` — 1..=200 символов (пусто и из пробелов —
/// 422), `max_tokens` — 256..=32000, `body_extra` — JSON-объект не больше 4096 байт; лишнее поле
/// входа `works.model` и `works.limits` — 400 (строгость входа).
#[tokio::test]
async fn works_model_field_bounds_422() {
    let a = setup("bounds", |_| {}).await;
    vault_item(&a, "llm").await;
    let with = |field: &str, v: Value| {
        let mut c = conn("llm");
        c[field] = v;
        json!({"works": {"model": c}})
    };
    // `{"k":"<n символов>"}` — 8 + n байт
    let extra = |n: usize| json!({ "k": "x".repeat(n) });
    for (body, field) in [
        (with("model", json!("")), "works.model.model"),
        (with("model", json!("   ")), "works.model.model"),
        (with("model", json!("м".repeat(201))), "works.model.model"),
        (with("max_tokens", json!(255)), "works.model.max_tokens"),
        (with("max_tokens", json!(32_001)), "works.model.max_tokens"),
        (
            with("body_extra", json!("строка")),
            "works.model.body_extra",
        ),
        (with("body_extra", json!([1, 2])), "works.model.body_extra"),
        (with("body_extra", extra(4089)), "works.model.body_extra"),
    ] {
        let r = patch(&a, body.clone()).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains(field), "{field}: {}", r.text());
    }
    assert!(stored(&a).await.is_empty(), "отказы ничего не пишут");
    for body in [
        with("model", json!("м".repeat(200))),
        with("max_tokens", json!(256)),
        with("max_tokens", json!(32_000)),
        with("body_extra", extra(4088)),
        with("body_extra", json!(null)),
    ] {
        ok(&patch(&a, body).await, StatusCode::OK);
    }
    // строгость входа: лишнее поле — 400
    let mut c = conn("llm");
    c["temperature"] = json!(0.2);
    ok(
        &patch(&a, json!({"works": {"model": c}})).await,
        StatusCode::BAD_REQUEST,
    );
    ok(
        &patch(
            &a,
            json!({"works": {"limits": {"daily_tokens": 1, "monthly_tokens": 2, "weekly": 3}}}),
        )
        .await,
        StatusCode::BAD_REQUEST,
    );
}

/// §6.1 п. 2: по токену `PATCH /auth/settings` (в том числе `works`) — 403, `GET` — 403, как
/// прежде: агент не меняет ни режим, ни подключение.
#[tokio::test]
async fn works_settings_token_403() {
    let a = setup("token", |_| {}).await;
    for body in [
        json!({"works": {"mode": "agent"}}),
        json!({"hooks": {"prompt": false}}),
    ] {
        ok(
            &a.t(Method::PATCH, SETTINGS, Some(body)).await,
            StatusCode::FORBIDDEN,
        );
    }
    ok(
        &a.t(Method::GET, SETTINGS, None).await,
        StatusCode::FORBIDDEN,
    );
    assert!(stored(&a).await.is_empty());
}

/// §6.1 п. 2: `mode: "model"` (общий или вида) без `works.model` — 422; при `[works]
/// model_access = "off"` — 422 и с подключением.
#[tokio::test]
async fn works_model_mode_requires_connection_422() {
    let a = setup("model-422", |_| {}).await;
    for body in [
        json!({"works": {"mode": "model"}}),
        json!({"works": {"kinds": {"rules": "model"}}}),
    ] {
        let r = patch(&a, body).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("works.model"), "{}", r.text());
    }
    assert!(stored(&a).await.is_empty());
    // подключение и режим одной правкой — можно
    vault_item(&a, "llm").await;
    let r = patch(
        &a,
        json!({"works": {"model": conn("llm"), "mode": "model"}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["works"]["mode"], "model");

    let b = setup("model-off", |c| c.works.model_access = ModelAccess::Off).await;
    vault_item(&b, "llm").await;
    ok(
        &patch(&b, json!({"works": {"model": conn("llm")}})).await,
        StatusCode::OK,
    );
    let r = patch(&b, json!({"works": {"mode": "model"}})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("model_access"), "{}", r.text());
    // режим, который не `model`, — можно
    ok(
        &patch(&b, json!({"works": {"mode": "agent"}})).await,
        StatusCode::OK,
    );
}

/// §6.1 п. 2: `works.limits` выше потолка сервера (`max_daily_tokens`, `max_monthly_tokens`) —
/// 422 с потолком в `detail`; равные потолку — можно.
#[tokio::test]
async fn works_limits_above_server_422() {
    let a = setup("limits", |c| {
        c.works.max_daily_tokens = 5000;
        c.works.max_monthly_tokens = 90_000;
    })
    .await;
    let r = patch(
        &a,
        json!({"works": {"limits": {"daily_tokens": 5001, "monthly_tokens": 10}}}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let detail = r.json()["detail"].as_str().unwrap().to_string();
    assert!(detail.contains("5000"), "{detail}");
    let r = patch(
        &a,
        json!({"works": {"limits": {"daily_tokens": 10, "monthly_tokens": 90001}}}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("90000"), "{}", r.text());
    let r = patch(
        &a,
        json!({"works": {"limits": {"daily_tokens": 5000, "monthly_tokens": 90000}}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    ok(
        &patch(&a, json!({"works": {"limits": null}})).await,
        StatusCode::OK,
    );
    assert!(stored(&a).await.is_empty());
}

/// §6.2 п. 5: паузу исполнителя пишет только сервер — `PATCH {works: {pause}}` — 400 (и `null`);
/// пауза сервера видна в ответе, `effective` режима `model` — `off` с причиной `paused`.
#[tokio::test]
async fn works_pause_not_patchable_400() {
    use zam_server::works::settings::{Pause, set_pause};
    let a = setup("pause", |_| {}).await;
    for p in [
        json!(null),
        json!({"class": "auth", "since": "2026-11-23T00:00:00.000000Z"}),
    ] {
        let r = patch(&a, json!({"works": {"pause": p}})).await;
        ok(&r, StatusCode::BAD_REQUEST);
        assert!(r.text().contains("works.pause"), "{}", r.text());
    }
    assert!(stored(&a).await.is_empty());
    vault_item(&a, "llm").await;
    ok(
        &patch(
            &a,
            json!({"works": {"model": conn("llm"), "mode": "model"}}),
        )
        .await,
        StatusCode::OK,
    );
    let pause = Pause {
        class: "auth".into(),
        since: zam_store::time::now(),
        from: None,
        to: None,
    };
    set_pause(&a.st, a.st.admin_id, Some(pause)).await.unwrap();
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    let w = &r.json()["works"];
    assert_eq!(w["pause"]["class"], "auth", "{w}");
    assert_eq!(
        w["effective"]["probes"],
        json!({"mode": "off", "reason": "paused"})
    );
    set_pause(&a.st, a.st.admin_id, None).await.unwrap();
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    assert!(r.json()["works"]["pause"].is_null());
}

/// §6.1 п. 2: `PATCH`, сменивший действующий режим вида, зовёт
/// `works::queue::on_mode_change(ctx, user, kinds)` со списком этих видов (снятие работ —
/// задача 3); правка без смены действующего режима — не зовёт.
#[tokio::test]
async fn works_mode_change_notifies() {
    use zam_server::works::queue::test_hooks::mode_changes;
    let a = setup("notify", |_| {}).await;
    let user: Id = a.st.admin_id;
    ok(
        &patch(&a, json!({"works": {"mode": "agent"}})).await,
        StatusCode::OK,
    );
    let all: Vec<WorkKind> = WorkKind::ALL.to_vec();
    assert_eq!(mode_changes(user), vec![all.clone()]);
    // переопределение вида тем же режимом — действующий режим прежний
    ok(
        &patch(&a, json!({"works": {"kinds": {"probes": "agent"}}})).await,
        StatusCode::OK,
    );
    assert_eq!(mode_changes(user).len(), 1);
    // общий режим `off`: у `probes` переопределение `agent` — меняются все виды, кроме него
    ok(
        &patch(&a, json!({"works": {"mode": "off"}})).await,
        StatusCode::OK,
    );
    let changes = mode_changes(user);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(!changes[1].contains(&WorkKind::Probes), "{changes:?}");
    assert!(changes[1].contains(&WorkKind::Rules), "{changes:?}");
    // правка без смены режима (проверка) — не зовёт
    ok(
        &patch(&a, json!({"works": {"check": "off"}})).await,
        StatusCode::OK,
    );
    assert_eq!(mode_changes(user).len(), 2);
    // администратор правит чужого пользователя — тот же вызов для него
    let u = a.st.system.create_user("Мира", false).await.unwrap();
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{}/settings", u.id),
            Some(json!({"works": {"kinds": {"rules": "agent"}}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(mode_changes(u.id), vec![vec![WorkKind::Rules]]);
}
