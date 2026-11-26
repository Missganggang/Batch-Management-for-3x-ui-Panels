use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Cursor, Read},
    process::{Child, Command, Stdio},
    sync::Arc,
};
use tokio::sync::Mutex;

const TEST_DELETE_PASSWORD: &str = "integration-only-panel-delete";

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_manager(dir: &std::path::Path) -> Result<(Service, String)> {
    let mut service = Service(
        Command::new(env!("CARGO_BIN_EXE_nexus-panel"))
            .env("NEXUS_DATA_DIR", dir)
            .env("NEXUS_PORT", "0")
            .env("NEXUS_BIND", "127.0.0.1")
            .env_remove("NEXUS_ADMIN_PASSWORD")
            .env("NEXUS_PANEL_DELETE_PASSWORD", TEST_DELETE_PASSWORD)
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let startup = BufReader::new(service.0.stdout.take().unwrap())
        .lines()
        .next()
        .context("startup")??;
    let base = startup
        .split_whitespace()
        .last()
        .context("port")?
        .to_owned();
    Ok((service, base))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_batch_and_partial_backup_are_attributed_to_the_correct_targets() -> Result<()> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let upstream = Router::new()
        .route(
            "/base/panel/api/inbounds/list",
            get(|h: HeaderMap| async move {
                assert_eq!(h["authorization"], "Bearer integration-test");
                Json(json!({"success":true,"obj":[{"id":1,"up":200,"down":300}]}))
            }),
        )
        .route(
            "/base/panel/api/clients/list",
            get(|| async { Json(json!({"success":true,"obj":[]})) }),
        )
        .route("/base/panel/api/xray/",post(||async {Json(json!({"success":true,"obj":{"xraySetting":{"outbounds":[],"routing":{"rules":[]}}}}))}))
        .route(
            "/base/panel/api/inbounds/get/1",
            get(|| async {
                Json(json!({"success":true,"obj":{"id":1,"settings":{},"protocol":"vless"}}))
            }),
        )
        .route(
            "/base/panel/api/inbounds/update/1",
            post(
                |State(seen): State<Arc<Mutex<Vec<i64>>>>, Json(row): Json<Value>| async move {
                    seen.lock().await.push(row["expiryTime"].as_i64().unwrap());
                    Json(json!({"success":true,"obj":null}))
                },
            ),
        )
        .route(
            "/base/panel/api/server/getDb",
            get(|| async { b"SQLite format 3\0test fixture".as_slice() }),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let remote = format!("http://{}/base", listener.local_addr()?);
    let upstream_task = tokio::spawn(async move { axum::serve(listener, upstream).await });
    let dir = tempfile::tempdir()?;
    let (_service, base) = start_manager(dir.path())?;
    let http = reqwest::Client::builder().no_proxy().build()?;
    let result: Value = http
        .post(format!("{base}/api/panels"))
        .json(&json!({"url":remote,"name":"test panel","token":"integration-test","sync":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = result["panel"]["id"].as_str().context("panel id")?;
    assert_eq!(result["panel"]["note"], "");
    let note_url = format!("{base}/api/panels/{id}/note");
    for (expected, note, status) in [
        ("", "香港面板", StatusCode::OK),
        ("", "香港面板", StatusCode::OK),
        ("", "另一备注", StatusCode::CONFLICT),
    ] {
        assert_eq!(
            http.post(&note_url)
                .json(&json!({"expectedNote":expected, "note":note}))
                .send()
                .await?
                .status(),
            status
        );
    }
    let empty_links: Value = http
        .get(format!("{base}/api/panels/{id}/client-links"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(empty_links["clients"], json!([]));
    assert_eq!(empty_links["links"], json!([]));
    assert!(!result.to_string().contains("integration-test"));
    assert_eq!(
        result["sync"]["inbounds"],
        1,
        "sync on connect: {}",
        result.get("syncError").unwrap_or(&Value::Null)
    );
    let refreshed: Value = http
        .post(format!("{base}/api/panels/{id}/sync"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(refreshed["added"], 0);
    let overview: Value = http
        .get(format!("{base}/api/overview"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(overview["panels"][0]["online"], true);
    assert_eq!(overview["panels"][0]["up"], 200);
    assert_eq!(overview["panels"][0]["note"], "香港面板");
    http.post(&note_url)
        .json(&json!({"expectedNote":"香港面板","note":""}))
        .send()
        .await?
        .error_for_status()?;
    let panels: Value = http
        .get(format!("{base}/api/panels"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(panels["panels"][0]["note"], "");
    assert_eq!(
        http.get(format!("{base}/api/panels"))
            .header("origin", "https://foreign.example")
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    let changes = json!({"action":"inbound.expiry","targets":[
        {"panelId":id,"payload":{"inboundId":1,"expiryTime":123000}},
        {"panelId":"missing","payload":{"inboundId":1,"expiryTime":123000}},
        {"panelId":id,"payload":{"inboundId":1,"expiryTime":0}}
    ]});
    let batch: Value = http
        .post(format!("{base}/api/batch"))
        .json(&changes)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(batch["results"][0]["ok"], true);
    assert_eq!(batch["results"][1]["ok"], false);
    assert_eq!(batch["results"][2]["ok"], true);
    assert_eq!(*seen.lock().await, vec![123000, 0]);
    let backup = http
        .post(format!("{base}/api/backup"))
        .json(&json!({"panelIds":["missing",id],"date":"2026-11-27","mode":"archive"}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(backup.headers()["x-backup-success"], "1");
    assert_eq!(backup.headers()["x-backup-failed"], "1");
    assert_eq!(
        backup.headers()["content-disposition"],
        "attachment; filename=\"127.0.0.1-2026-11-27.zip\""
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(backup.bytes().await?))?;
    let manifest: Value = serde_json::from_reader(archive.by_name("manifest.json")?)?;
    assert_eq!(manifest["files"][0]["panelId"], id);
    assert_eq!(manifest["files"][0]["file"], "127.0.0.1-2026-11-27.db");
    let mut bytes = Vec::new();
    archive
        .by_name(manifest["files"][0]["file"].as_str().unwrap())?
        .read_to_end(&mut bytes)?;
    assert_eq!(bytes, b"SQLite format 3\0test fixture");
    let history: Value = http
        .get(format!("{base}/api/history"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(history["history"][1]["results"], batch["results"]);
    let raw = http
        .post(format!("{base}/api/backup"))
        .json(&json!({"panelIds":[id],"date":"2026-11-27","mode":"single"}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        raw.headers()["content-disposition"],
        "attachment; filename=\"127.0.0.1-2026-11-27.db\""
    );
    assert_eq!(
        raw.bytes().await?.as_ref(),
        b"SQLite format 3\0test fixture"
    );
    let alias = Router::new()
        .route(
            "/panel/api/inbounds/list",
            get(|| async { Json(json!({"success":true,"obj":[]})) }),
        )
        .route(
            "/panel/api/server/getDb",
            get(|| async { b"SQLite format 3\0second database".as_slice() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let alias_url = format!("http://{}", listener.local_addr()?);
    let alias_task = tokio::spawn(async move { axum::serve(listener, alias).await });
    let registration: Value = http
        .post(format!("{base}/api/panels"))
        .json(&json!({"url":alias_url,"name":"other panel","token":"integration-test","sync":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let second = registration["panel"]["id"]
        .as_str()
        .context("second panel persisted after sync error")?;
    assert!(registration["syncError"].is_string());
    let combined = http
        .post(format!("{base}/api/backup"))
        .json(&json!({"panelIds":[id,second],"mode":"archive","date":"2026-11-27"}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(combined.headers()["x-backup-success"], "2");
    let mut combined = zip::ZipArchive::new(Cursor::new(combined.bytes().await?))?;
    let mut second_bytes = Vec::new();
    combined
        .by_name("127.0.0.1-2026-11-27-2.db")?
        .read_to_end(&mut second_bytes)?;
    assert_eq!(second_bytes, b"SQLite format 3\0second database");
    http.delete(format!("{base}/api/panels/{second}"))
        .json(&json!({"password":TEST_DELETE_PASSWORD}))
        .send()
        .await?
        .error_for_status()?;
    alias_task.abort();
    for payload in [json!({}), json!({"password":"incorrect"})] {
        assert_eq!(
            http.delete(format!("{base}/api/panels/{id}"))
                .json(&payload)
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        http.get(format!("{base}/api/panels"))
            .send()
            .await?
            .json::<Value>()
            .await?["panels"][0]["id"],
        id
    );
    http.delete(format!("{base}/api/panels/{id}"))
        .json(&json!({"password":TEST_DELETE_PASSWORD}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        http.get(format!("{base}/api/panels"))
            .send()
            .await?
            .json::<Value>()
            .await?["panels"],
        json!([])
    );
    upstream_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_survives_restart_after_the_remote_client_has_been_removed() -> Result<()> {
    use axum::extract::Form;
    use std::collections::HashMap;
    #[derive(Clone)]
    struct Fixture {
        clients: Value,
        inbounds: Value,
        config: Value,
        config_writes: usize,
        fail_once: bool,
    }
    type Mock = State<Arc<Mutex<Fixture>>>;
    let client = json!({"id":84,"email":"test-client","uuid":"3b85f86b-7fa2-441b-82ff-f85a7113d0a0","inboundIds":[1],"enable":true,"expiryTime":0});
    let inbound = json!({"id":1,"tag":"test-client","remark":"test-client","protocol":"vless","port":20219,"enable":true,
        "settings":{"decryption":"none","clients":[{"email":"test-client","id":client["uuid"]}]},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"settings":{"publicKey":"public"},
          "shortIds":["aaaa"],"serverNames":["tls.example"]}}});
    let api = json!({"type":"field","inboundTag":["api"],"outboundTag":"api"});
    let private = json!({"type":"field","ip":["geoip:private"],"outboundTag":"blocked"});
    let bittorrent = json!({"type":"field","protocol":["bittorrent"],"outboundTag":"blocked"});
    let custom = json!({"type":"field","domain":["full:custom.example"],"outboundTag":"direct"});
    let legacy = json!({"inboundTag":["test-client"],"outboundTag":"test-client","type":"field","ruleTag":"test-client","enabled":false});
    let config = json!({"outbounds":[{"tag":"direct","protocol":"freedom"},{"tag":"test-client","protocol":"socks"}],
        "routing":{"rules":[api,legacy,private,bittorrent,custom]}});
    let fixture = Arc::new(Mutex::new(Fixture {
        clients: json!([client]),
        inbounds: json!([inbound]),
        config,
        config_writes: 0,
        fail_once: true,
    }));
    let mock=Router::new()
        .route("/panel/api/clients/list",get(|s:Mock|async move{Json(json!({"success":true,"obj":s.lock().await.clients}))}))
        .route("/panel/api/inbounds/list",get(|s:Mock|async move{Json(json!({"success":true,"obj":s.lock().await.inbounds}))}))
        .route("/panel/api/xray/",post(|s:Mock|async move{Json(json!({"success":true,"obj":{"xraySetting":s.lock().await.config,"outboundTestUrl":"https://example.test"}}))}))
        .route("/panel/api/clients/del/test-client",post(|s:Mock|async move{
            let mut state=s.lock().await;
            assert_eq!(state.clients.as_array().unwrap().len(),1,"a retry must not delete the client again");
            state.clients=json!([]);state.inbounds[0]["settings"]["clients"]=json!([]);
            Json(json!({"success":true,"obj":null}))
        }))
        .route("/panel/api/inbounds/del/1",post(|s:Mock|async move{
            let mut state=s.lock().await;
            if std::mem::take(&mut state.fail_once){return Json(json!({"success":false,"msg":"temporary upstream error"}));}
            state.inbounds=json!([]);Json(json!({"success":true,"obj":null}))
        }))
        .route("/panel/api/xray/update",post(|s:Mock,Form(form):Form<HashMap<String,String>>|async move{
            let mut state = s.lock().await;
            state.config = serde_json::from_str(&form["xraySetting"]).unwrap();
            state.config_writes += 1;
            Json(json!({"success":true,"obj":null}))
        })).with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}", listener.local_addr()?);
    let upstream = tokio::spawn(async move { axum::serve(listener, mock).await });
    let dir = tempfile::tempdir()?;
    let (service, base) = start_manager(dir.path())?;
    let http = reqwest::Client::builder().no_proxy().build()?;
    let registration: Value = http
        .post(format!("{base}/api/panels"))
        .json(&json!({"url":upstream_url,"token":"test","name":"fixture","sync":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(registration["sync"]["added"], 1);
    let id = registration["panel"]["id"].as_str().unwrap();
    let repair_url = format!("{base}/api/panels/{id}/repair-user-routes");
    let expected = json!({"type":"field","ruleTag":"test-client","user":["test-client"],"outboundTag":"test-client","enabled":false});
    for changed in [1, 0] {
        let report: Value = http
            .post(&repair_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(report, json!({"changed":changed,"skipped":0}));
        let state = fixture.lock().await;
        assert_eq!(
            state.config["routing"]["rules"],
            json!([api, private, bittorrent, expected, custom])
        );
        assert_eq!(state.config_writes, 1);
    }
    let data = json!({"targets":[{"panelId":id,"clientId":84,"email":"test-client","uuid":client["uuid"]}]});
    let first: Value = http
        .post(format!("{base}/api/clients/delete"))
        .json(&data)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(first["results"][0]["ok"], false);
    assert_eq!(fixture.lock().await.clients, json!([]));
    drop(service);
    let (_service, base) = start_manager(dir.path())?;
    let pending: Value = http
        .get(format!("{base}/api/clients/deletions"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(pending["pending"][0]["clientId"], 84);
    let final_result: Value = http
        .post(format!("{base}/api/clients/delete"))
        .json(&data)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        final_result["results"][0]["ok"], true,
        "{}",
        final_result["results"][0]["message"]
    );
    let state = fixture.lock().await;
    assert_eq!(state.inbounds, json!([]));
    assert_eq!(
        state.config["routing"]["rules"],
        json!([api, private, bittorrent, custom])
    );
    assert_eq!(state.config_writes, 2);
    assert_eq!(
        state.config["outbounds"],
        json!([{"tag":"direct","protocol":"freedom"}])
    );
    drop(state);
    for (path, key) in [("clients/deletions", "pending"), ("lines", "lines")] {
        let body: Value = http
            .get(format!("{base}/api/{path}"))
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(body[key], json!([]));
    }
    upstream.abort();
    Ok(())
}
