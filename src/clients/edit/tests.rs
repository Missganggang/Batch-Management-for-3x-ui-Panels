use super::*;
use crate::{App, Shared};
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

const DAY: i64 = 86_400_000;

#[test]
fn extensions_use_individual_expiry_and_keep_first_use_relative() {
    let ms = |value: &str| {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    };
    let now = ms("2028-01-31T22:10:02.123Z");
    let feb = ms("2028-02-29T22:10:02.123Z");
    assert_eq!(Term::Months(1).extend(0, now).unwrap(), feb);
    assert_eq!(Term::Months(1).extend(now - DAY, now).unwrap(), feb);
    assert_eq!(
        Term::Months(12).extend(feb, now).unwrap(),
        ms("2029-02-28T22:10:02.123Z")
    );
    assert_eq!(
        Term::Days(40_000).extend(feb, now).unwrap(),
        feb + 40_000 * DAY
    );
    assert_eq!(Term::Days(3).extend(-2 * DAY, now).unwrap(), -5 * DAY);
    assert_eq!(Term::Months(1).extend(-2 * DAY, now).unwrap(), -31 * DAY);
    assert!(Term::Days(1).extend(i64::MIN, now).is_err());
    assert!(Term::Days(i64::MAX).extend(now, now).is_err());
    assert!(Term::Days(1).extend(8_640_000_000_000_000, now).is_err());
}

struct Fixture {
    record: Value,
    writes: usize,
}
type Remote = State<Arc<Mutex<Fixture>>>;

async fn lookup(State(remote): Remote) -> Json<Value> {
    Json(json!({"success":true,"obj":{"client":remote.lock().unwrap().record,"inboundIds":[5,7]}}))
}

async fn update(State(remote): Remote, Json(input): Json<Value>) -> Json<Value> {
    let mut state = remote.lock().unwrap();
    for field in ["email", "enable", "flow", "subId", "totalGB", "reverse"] {
        assert_eq!(input[field], state.record[field], "{field}");
    }
    assert_eq!(input["id"], state.record["uuid"]);
    assert_eq!(input["allowedIPs"], json!(["10.0.0.1/32", "::1/128"]));
    assert_eq!(input["created_at"], 123);
    assert!(input.get("inboundIds").is_none());
    state.record["comment"] = input["comment"].clone();
    state.record["expiryTime"] = input["expiryTime"].clone();
    state.writes += 1;
    state.record["updatedAt"] = json!(10 + state.writes);
    // Simulate a remote commit followed by a lost acknowledgement on the first write.
    if state.writes == 1 {
        Json(json!({"success":false,"msg":"upstream temporarily unavailable"}))
    } else {
        Json(json!({"success":true,"obj":{"nodePending":false}}))
    }
}

async fn mock_panel(
    router: Router,
) -> (
    tempfile::TempDir,
    Shared,
    String,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let db = storage::open(temp.path()).await.unwrap();
    let panel = storage::Panel::from_form(
        crate::PanelForm {
            name: "测试面板".into(),
            url,
            token: "test-only".into(),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    storage::insert_panel(&db, &panel).await.unwrap();
    let app = Arc::new(App {
        db,
        client: reqwest::Client::new(),
        locks: Default::default(),
        traffic_lock: Default::default(),
        upstream_slots: Semaphore::new(2),
        backup_slot: Semaphore::new(1),
    });
    (temp, app, panel.id, task)
}

#[tokio::test]
async fn native_edits_preserve_settings_and_retries_reconcile_an_ambiguous_write() {
    let remote = Arc::new(Mutex::new(Fixture {
        record: json!({"id":3,"email":"client-a","uuid":"stable-id","enable":false,
            "expiryTime":0,"updatedAt":10,"createdAt":123,"comment":"原备注","totalGB":4096,
            "flow":"xtls-rprx-vision","reverse":{"tag":"reverse"},"subId":"retained",
            "allowedIPs":"10.0.0.1/32,::1/128"}),
        writes: 0,
    }));
    let router = Router::new()
        .route("/panel/api/clients/get/:email", get(lookup))
        .route("/panel/api/clients/update/:email", post(update))
        .with_state(remote.clone());
    let (_temp, app, panel_id, task) = mock_panel(router).await;
    let selection = json!({"panelId":panel_id,"clientId":3,"email":"client-a","uuid":"stable-id","expiryTime":0,"updatedAt":10,"note":"原备注"});
    let mut stale = selection.clone();
    stale["clientId"] = json!(4);
    let targets = json!([stale, selection]);
    let extension = || Json(serde_json::from_value(json!({"targets":targets,"days":8})).unwrap());
    let first = extend(State(app.clone()), extension())
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(first["results"][0]["clientId"], 4);
    assert_eq!(first["results"][0]["ok"], false);
    assert_eq!(first["results"][1]["ok"], false);
    assert_eq!(first["results"][1]["conflict"], false);
    let second = extend(State(app.clone()), extension())
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(second["results"][1]["ok"], true);
    assert_eq!(
        second["results"][1]["expiryTime"],
        remote.lock().unwrap().record["expiryTime"]
    );
    assert_eq!(remote.lock().unwrap().writes, 1);
    assert_eq!(remote.lock().unwrap().record["comment"], "原备注");
    let saved_expiry = second["results"][1]["expiryTime"].clone();
    let edit_note = |expected: &str, new: &str| {
        let mut target = selection.clone();
        target["note"] = json!(expected);
        Json(serde_json::from_value(json!({"targets":[target],"note":new})).unwrap())
    };
    for _ in 0..2 {
        let reply = notes(State(app.clone()), edit_note("原备注", "客户 🐉"))
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
            .0;
        assert_eq!(reply["results"][0]["note"], "客户 🐉");
    }
    let conflict = notes(State(app.clone()), edit_note("原备注", ""))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(conflict["results"][0]["conflict"], true);
    assert_eq!(remote.lock().unwrap().writes, 2);
    let clear = notes(State(app.clone()), edit_note("客户 🐉", ""))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(clear["results"][0]["note"], "");
    assert_eq!(remote.lock().unwrap().record["expiryTime"], saved_expiry);
    assert_eq!(remote.lock().unwrap().record["comment"], "");
    // An external reset creates a fresh version, even when the expiry has returned to zero.
    remote.lock().unwrap().record["expiryTime"] = json!(0);
    let old = extend(State(app.clone()), extension())
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(old["results"][1]["ok"], false);
    assert_eq!(old["results"][1]["conflict"], false);
    let mut fresh = selection.clone();
    fresh["updatedAt"] = remote.lock().unwrap().record["updatedAt"].clone();
    let fresh = Json(serde_json::from_value(json!({"targets":[fresh],"days":8})).unwrap());
    let renewed = extend(State(app.clone()), fresh)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(renewed["results"][0]["ok"], true);
    assert_eq!(remote.lock().unwrap().writes, 4);
    let mut stale = selection;
    stale["updatedAt"] = json!(5);
    let stale = Json(serde_json::from_value(json!({"targets":[stale],"months":2})).unwrap());
    let rejected = extend(State(app.clone()), stale)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(rejected["results"][0]["conflict"], true);
    assert_eq!(remote.lock().unwrap().writes, 4);
    task.abort();
    app.db.close().await;
}

#[tokio::test]
async fn native_switch_verifies_identity_result_and_readback() {
    type Mock = State<Arc<Mutex<Value>>>;
    let remote = Arc::new(Mutex::new(json!({
        "client":{"id":3,"uuid":"stable-id","email":"client-a","expiryTime":89012,"enable":true,"updatedAt":123},
        "mode":"apply","writes":0
    })));
    let router = Router::new()
        .route("/panel/api/clients/get/client-a", get(|s: Mock| async move {
            Json(json!({"success":true,"obj":{"client":s.lock().unwrap()["client"]}}))
        }))
        .route("/panel/api/clients/:action", post(|s: Mock, axum::extract::Path(action): axum::extract::Path<String>, Json(body): Json<Value>| async move {
            assert_eq!(body, json!({"emails":["client-a"]}));
            let enable = match action.as_str() {
                "bulkEnable" => true,
                "bulkDisable" => false,
                action => panic!("unexpected action: {action}"),
            };
            let mut state = s.lock().unwrap();
            state["writes"] = json!(state["writes"].as_u64().unwrap() + 1);
            if state["mode"] == "skip" {
                return Json(json!({"success":true,"obj":{"changed":0,"skipped":[{"email":"client-a","reason":"rejected"}]}}));
            }
            if state["mode"] == "apply" {
                state["client"]["enable"] = json!(enable);
            }
            Json(json!({"success":true,"obj":{"changed":1}}))
        }))
        .with_state(remote.clone());
    let (_temp, app, panel_id, task) = mock_panel(router).await;
    let change = |uuid: &str, enable| {
        Json(serde_json::from_value(json!({"targets":[{"panelId":panel_id,"clientId":3,"email":"client-a","uuid":uuid}],"enable":enable})).unwrap())
    };
    let switch = |uuid: &str, enable| status(State(app.clone()), change(uuid, enable));
    let result = switch("replaced", false)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(result["results"][0]["ok"], false);
    assert_eq!(remote.lock().unwrap()["writes"], 0);
    for enable in [false, false, true] {
        let result = switch("stable-id", enable)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
            .0;
        assert_eq!(result["results"][0]["ok"], true);
        assert_eq!(result["results"][0]["enable"], enable);
    }
    assert_eq!(remote.lock().unwrap()["writes"], 2);
    for mode in ["skip", "ignore"] {
        remote.lock().unwrap()["mode"] = json!(mode);
        let result = switch("stable-id", false)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
            .0;
        assert_eq!(result["results"][0]["ok"], false);
        assert_eq!(remote.lock().unwrap()["client"]["enable"], true);
    }
    task.abort();
    app.db.close().await;
}

#[tokio::test]
async fn quota_updates_and_native_resets_affect_only_selected_clients() {
    let remote = Arc::new(Mutex::new(json!({
        "client":{"id":3,"email":"client-a","uuid":"stable-id","enable":false,"expiryTime":89012,"totalGB":0,"comment":"keep"},
        "inboundIds":[5,7],"usedTraffic":2400
    })));
    type Mock = State<Arc<Mutex<Value>>>;
    let router =
        Router::new()
            .route(
                "/panel/api/clients/get/client-a",
                get(|s: Mock| async move {
                    Json(json!({"success":true,"obj":s.lock().unwrap().clone()}))
                }),
            )
            .route(
                "/panel/api/clients/update/client-a",
                post(|s: Mock, Json(body): Json<Value>| async move {
                    assert_eq!(body["id"], "stable-id");
                    assert_eq!(body["expiryTime"], 89012);
                    assert_eq!(body["comment"], "keep");
                    assert_eq!(body["enable"], false);
                    s.lock().unwrap()["client"]["totalGB"] = body["totalGB"].clone();
                    Json(json!({"success":true,"obj":{"nodePending":false}}))
                }),
            )
            .route(
                "/panel/api/clients/resetTraffic/client-a",
                post(|s: Mock| async move {
                    s.lock().unwrap()["usedTraffic"] = json!(0);
                    Json(json!({"success":true,"obj":null}))
                }),
            )
            .with_state(remote.clone());
    let (_temp, app, panel_id, task) = mock_panel(router).await;
    sqlx::query(
        "INSERT INTO lines(id,panel_id,name,uuid,status,expiry,updated,client_email,client_record)
        VALUES('line',?,'line','stable-id','ready',89012,0,'client-a',3)",
    )
    .bind(&panel_id)
    .execute(&app.db)
    .await
    .unwrap();
    let target = json!({"panelId":panel_id,"clientId":3,"email":"client-a","uuid":"stable-id","quotaBytes":0});
    let mut stale = target.clone();
    stale["clientId"] = json!(9);
    let targets = json!([stale, target]);
    let cap = 200_i64 * 1024 * 1024 * 1024;
    let change = |targets: &Value, bytes: i64| {
        Json(serde_json::from_value(json!({"targets":targets,"quotaBytes":bytes})).unwrap())
    };
    assert!(quota(State(app.clone()), change(&targets, -1))
        .await
        .is_err());
    for _ in 0..2 {
        let reply = quota(State(app.clone()), change(&targets, cap))
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
            .0;
        assert_eq!(reply["results"][0]["ok"], false);
        assert_eq!(reply["results"][1]["quotaBytes"], cap);
        assert_eq!(remote.lock().unwrap()["usedTraffic"], 2400);
    }
    let cached: i64 = sqlx::query_scalar("SELECT quota_bytes FROM lines WHERE id='line'")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(cached, cap);
    let reply = quota(State(app.clone()), change(&targets, 10))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(reply["results"][1]["conflict"], true);
    let reset = |targets: &Value| Json(serde_json::from_value(json!({"targets":targets})).unwrap());
    let invalid = super::super::reset_traffic(State(app.clone()), reset(&json!([stale])))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(invalid["results"][0]["ok"], false);
    assert_eq!(remote.lock().unwrap()["usedTraffic"], 2400);
    let reply = super::super::reset_traffic(State(app.clone()), reset(&targets))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(reply["results"][0]["ok"], false);
    assert_eq!(reply["results"][1]["ok"], true);
    assert_eq!(remote.lock().unwrap()["usedTraffic"], 0);
    assert_eq!(remote.lock().unwrap()["client"]["totalGB"], cap);
    let mut current = target;
    current["quotaBytes"] = json!(cap);
    let reply = quota(State(app.clone()), change(&json!([current]), 0))
        .await
        .unwrap_or_else(|e| panic!("{}", e.1))
        .0;
    assert_eq!(reply["results"][0]["quotaBytes"], 0);
    assert_eq!(remote.lock().unwrap()["client"]["totalGB"], 0);
    task.abort();
    app.db.close().await;
}
