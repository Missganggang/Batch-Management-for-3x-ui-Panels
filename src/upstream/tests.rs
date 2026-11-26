use super::*;
use axum::{
    extract::{Form, Path},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::collections::HashMap;

fn ok(obj: Value) -> Json<Value> {
    Json(json!({"success":true,"obj":obj,"msg":""}))
}

async fn target_report(Form(fields): Form<HashMap<String, String>>) -> Json<Value> {
    assert_eq!(fields.len(), 1);
    let target = &fields["target"];
    let (host, port) = target.split_once(':').unwrap();
    ok(
        json!({"target":target,"host":host,"port":port.parse::<u16>().unwrap(),
        "feasible":true,"tls13":true,"h2":true,"x25519":true,"certValid":true,
        "serverNames":["alt.example.test","*.example.test",host,host,"bad name","::1"]}),
    )
}

#[tokio::test]
async fn line_recovers_after_interrupted_template_save() {
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fixture {
        ib: Value,
        cfg: Value,
        saves: usize,
        inbounds_created: usize,
    }
    let state = Arc::new(Mutex::new(Fixture {
        cfg: json!({"outbounds":[{"tag":"direct","protocol":"freedom"}],"routing":{"rules":[
            {"type":"field","inboundTag":["api"],"outboundTag":"api"},
            {"type":"field","ip":["geoip:private"],"outboundTag":"blocked"},
            {"type":"field","protocol":["bittorrent"],"outboundTag":"blocked"},
            {"type":"field","network":"tcp,udp","outboundTag":"direct"}
        ]}}),
        ..Default::default()
    }));
    type Mock = axum::extract::State<Arc<Mutex<Fixture>>>;
    let app = Router::new()
        .route("/private/path/panel/api/inbounds/list", get(|s:Mock| async move {
            let s=s.lock().unwrap();let mut ib=s.ib.clone();if !ib.is_null(){ib["settings"]=json!(ib["settings"].to_string());ib["streamSettings"]=json!(ib["streamSettings"].to_string());}ok(if ib.is_null(){json!([])}else{json!([ib])})
        }))
        .route("/private/path/panel/api/clients/list",get(||async{ok(json!([]))}))
        .route("/private/path/panel/api/xray/",post(|s:Mock|async move {
            ok(json!({"xraySetting":s.lock().unwrap().cfg,"outboundTestUrl":"https://x.example/"}))
        }))
        .route("/private/path/panel/api/server/getNewX25519Cert",get(||async {
            ok(json!({"privateKey":"private","publicKey":"public"}))
        }))
        .route("/private/path/panel/api/server/scanRealityTarget",post(target_report))
        .route("/private/path/panel/api/inbounds/add",post(|s:Mock,Json(mut ib):Json<Value>|async move {
            assert_eq!(ib["enable"],false);
            ib["id"]=json!(5);
            let mut s=s.lock().unwrap(); s.ib=ib.clone();s.inbounds_created+=1;ok(ib)
        }))
        .route("/private/path/panel/api/xray/update",post(|s:Mock,Form(form):Form<HashMap<String,String>>|async move {
            let mut s=s.lock().unwrap();s.cfg=serde_json::from_str(&form["xraySetting"]).unwrap();s.saves+=1;
            if s.saves==1 {Json(json!({"success":false,"msg":"some upstream error containing s3cret"}))}
            else {ok(Value::Null)}
        }))
        .route("/private/path/panel/api/inbounds/setEnable/5",post(|s:Mock|async move {
            let mut s=s.lock().unwrap();assert_eq!(s.saves,2);s.ib["enable"]=json!(true);ok(Value::Null)
        }))
        .with_state(state.clone());
    let api = panel(app).await;
    let spec = json!({"name":"测试-甲","socks5":"127.0.0.1:1080:worker:s3cret","connectHost":"[::1]","expiryTime":30000});
    let id = "40afc7d3-f6cd-46d1-800f-63e9a1b17434";
    assert!(!format!("{:#}", api.provision_line(&spec, id).await.unwrap_err()).contains("s3cret"));
    assert_eq!(state.lock().unwrap().ib["enable"], false);
    assert!(api
        .provision_line(&spec, "63d2518a-424a-4187-8371-c0ac0095eacd")
        .await
        .is_err());
    let line = api.provision_line(&spec, id).await.unwrap();
    assert_eq!(api.provision_line(&spec, id).await.unwrap(), line);
    let s = state.lock().unwrap();
    assert_eq!(s.inbounds_created, 1);
    assert_eq!(s.saves, 2);
    assert_eq!(s.ib["enable"], true);
    assert_eq!(
        s.cfg["routing"]["rules"][3],
        json!({"ruleTag":"测试-甲","type":"field","user":["测试-甲"],"outboundTag":"测试-甲"})
    );
    assert_eq!(s.cfg["routing"]["rules"][4]["outboundTag"], "direct");
    assert_eq!(s.ib["expiryTime"], 0);
    assert_eq!(s.ib["settings"]["clients"][0]["expiryTime"], 30000);
    let outbound = &s.cfg["outbounds"][1];
    assert_eq!(outbound["tag"], "测试-甲");
    assert_eq!(
        outbound["settings"]["servers"][0]["users"][0],
        json!({"user":"worker","pass":"s3cret"})
    );
    let share = Url::parse(line["link"].as_str().unwrap()).unwrap();
    assert_eq!(share.username(), id);
    assert_eq!(share.host_str(), Some("[::1]"));
    assert_eq!(share.fragment(), Some("%E6%B5%8B%E8%AF%95-%E7%94%B2"));
    let params: HashMap<_, _> = share.query_pairs().collect();
    assert_eq!(params["type"], "tcp");
    assert_eq!(params["security"], "reality");
    assert_eq!(params["pbk"], "public");
    assert!(!share.as_str().contains("s3cret"));
}

#[tokio::test]
async fn renewal_reenables_only_the_original_owned_client() {
    let email = "上海-01";
    let uuid = "2595a7fb-9b8c-420b-ad6d-171566d54dd8";
    let app = Router::new()
        .route(
            "/private/path/panel/api/clients/get/:email",
            get(move |Path(found): Path<String>| async move {
                assert_eq!(found, email);
                ok(json!({"inboundIds":[3],"client":{
                    "id":7,"email":email,"uuid":uuid,"enable":false,"expiryTime":12,
                    "flow":"xtls-rprx-vision","totalGB":123456,"subId":"keep-me"
                }}))
            }),
        )
        .route(
            "/private/path/panel/api/clients/update/:email",
            post(move |Json(client): Json<Value>| async move {
                assert_eq!(client["enable"], true);
                assert_eq!(client["expiryTime"], 1000);
                assert_eq!(client["totalGB"], 123456);
                assert_eq!(client["subId"], "keep-me");
                assert_eq!(client["id"], uuid);
                ok(Value::Null)
            }),
        )
        .route(
            "/private/path/panel/api/clients/onlines",
            post(move || async move { ok(json!([email])) }),
        );
    let api = panel(app).await;
    assert!(api.renew_line(4, email, uuid, 1000).await.is_err());
    assert!(api
        .renew_line(3, email, "another-uuid", 1000)
        .await
        .is_err());
    api.renew_line(3, email, uuid, 1000).await.unwrap();
    assert_eq!(api.online_clients().await.unwrap(), vec![email]);
}

async fn panel(app: Router) -> PanelApi {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(socket, app).await.unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();
    PanelApi::new(
        client,
        &format!("http://{address}/private/path/"),
        "test-key",
    )
    .unwrap()
}

#[tokio::test]
async fn client_expiry_preserves_credentials_and_quota() {
    let api = panel(Router::new()
        .route("/private/path/panel/api/clients/get/:email", get(|headers: HeaderMap, Path(email): Path<String>| async move {
            assert_eq!(headers[header::AUTHORIZATION], "Bearer test-key");
            assert_eq!(email, "a+#@example.test");
            ok(json!({"inboundIds":[7],"client":{
                "id":55,"uuid":"old-uuid","email":email,"enable":false,"allowedIPs":"192.0.2.0/24",
                "totalGB":7654321,"subId":"existing-sub","expiryTime":100,"createdAt":88,
                "group":"business","flow":"xtls-rprx-vision"
            }}))
        }))
        .route("/private/path/panel/api/clients/update/:email", post(|Json(client): Json<Value>| async move {
            assert_eq!(client["id"], "old-uuid");
            assert_eq!(client["totalGB"], 7654321);
            assert_eq!(client["enable"], false);
            assert_eq!(client["subId"], "existing-sub");
            assert_eq!(client["created_at"], 88);
            assert_eq!(client["allowedIPs"], json!(["192.0.2.0/24"]));
            assert_eq!(client["flow"], "xtls-rprx-vision");
            assert_eq!(client["group"], "business");
            assert_eq!(client["expiryTime"], 0);
            ok(json!({"nodePending":true}))
        }))
    ).await;
    let payload = json!({"email":"a+#@example.test","inboundId":7,"expiryTime":0});
    assert!(api
        .apply("client.expiry", &payload)
        .await
        .unwrap()
        .contains("等待节点同步"));
    assert!(api
        .apply(
            "client.expiry",
            &json!({"inboundId":3,"email":"a+#@example.test","expiryTime":0})
        )
        .await
        .is_err());
}

#[tokio::test]
async fn route_append_round_trips_template_and_test_url() {
    let api = panel(
        Router::new()
            .route(
                "/private/path/panel/api/xray/",
                post(|| async {
                    let wrapper = json!({"xraySetting":{
                "dns":{"servers":["localhost"]},"routing":{"rules":[{"outboundTag":"api"}]},
                "outbounds":[{"tag":"direct","protocol":"freedom"}],"customField":true
            },"outboundTestUrl":"https://check.example/204","inboundTags":["api"]});
                    ok(json!(wrapper.to_string()))
                }),
            )
            .route(
                "/private/path/panel/api/xray/update",
                post(|Form(mut data): Form<HashMap<String, String>>| async move {
                    assert_eq!(data["outboundTestUrl"], "https://check.example/204");
                    let cfg: Value =
                        serde_json::from_str(&data.remove("xraySetting").unwrap()).unwrap();
                    assert_eq!(cfg["customField"], true);
                    assert_eq!(cfg["inboundTags"], Value::Null);
                    assert_eq!(cfg["dns"], json!({"servers":["localhost"]}));
                    assert_eq!(cfg["outbounds"][0]["tag"], "direct");
                    assert_eq!(cfg["routing"]["rules"][0]["outboundTag"], "api");
                    assert_eq!(
                        cfg["routing"]["rules"][1]["domain"],
                        json!(["example.test"])
                    );
                    ok(Value::Null)
                }),
            ),
    )
    .await;
    let rule = json!({"type":"field","domain":["example.test"],"outboundTag":"direct"});
    api.apply("route.upsert", &json!({"rule":rule}))
        .await
        .unwrap();
    assert!(api
        .apply(
            "route.upsert",
            &json!({"index":0,"rule":rule,"expectedRule":rule})
        )
        .await
        .is_err());
}

#[tokio::test]
async fn reality_creation_uses_panel_keys_and_no_placeholder_clients() {
    let api = panel(
        Router::new()
            .route(
                "/private/path/panel/api/server/scanRealityTarget",
                post(target_report),
            )
            .route(
                "/private/path/panel/api/server/getNewX25519Cert",
                get(|| async {
                    ok(json!({
                        "privateKey":"private-fixture","publicKey":"public-fixture"
                    }))
                }),
            )
            .route(
                "/private/path/panel/api/inbounds/add",
                post(|Json(body): Json<Value>| async move {
                    let stream = &body["streamSettings"];
                    assert_eq!(stream["network"], "tcp");
                    assert_eq!(stream["security"], "reality");
                    let reality = &stream["realitySettings"];
                    assert_eq!(reality["privateKey"], "private-fixture");
                    assert_eq!(reality["settings"]["publicKey"], "public-fixture");
                    assert_eq!(reality["target"], "example.test:443");
                    assert_eq!(
                        reality["serverNames"],
                        json!(["example.test", "alt.example.test"])
                    );
                    assert_eq!(reality["settings"]["serverName"], "example.test");
                    assert_eq!(reality["shortIds"][0].as_str().unwrap().len(), 16);
                    assert_eq!(body["settings"]["clients"], json!([]));
                    assert_eq!(body["expiryTime"], 1780000000000_i64);
                    ok(json!({"id":33}))
                }),
            ),
    )
    .await;
    let message = api.apply("inbound.create",&json!({
        "remark":"VLESS","port":443,"serverName":"example.test","expiryTime":1780000000000_i64
    })).await.unwrap();
    assert!(message.contains("33") && !message.contains("fixture"));
}

#[tokio::test]
async fn reality_verifies_one_target_and_preserves_explicit_covered_sni() {
    let api = panel(Router::new()
        .route("/private/path/panel/api/server/scanRealityTarget", post(target_report))
        .route("/private/path/panel/api/server/getNewX25519Cert", get(|| async {
            ok(json!({"privateKey":uuid::Uuid::new_v4().to_string(),"publicKey":uuid::Uuid::new_v4().to_string()}))
        }))).await;
    let spec = json!({"remark":"test","port":4443,"serverName":"alt.example.test","dest":"site.test:8443"});
    let first = api.reality_inbound(&spec).await.unwrap();
    let second = api.reality_inbound(&spec).await.unwrap();
    let a = &first["streamSettings"]["realitySettings"];
    let b = &second["streamSettings"]["realitySettings"];
    assert_eq!(a["target"], "site.test:8443");
    assert_eq!(a["serverNames"], json!(["alt.example.test", "site.test"]));
    assert_ne!(a["privateKey"], b["privateKey"]);
    assert_ne!(a["shortIds"], b["shortIds"]);
    assert!(api
        .reality_names("site.test:8443", Some("missing.example.test"))
        .await
        .is_err());
    let failed = panel(Router::new().route(
        "/private/path/panel/api/server/scanRealityTarget",
        post(|| async { ok(json!({"feasible":false,"reason":"TLS handshake failed"})) }),
    ))
    .await;
    let err = failed.reality_inbound(&spec).await.unwrap_err();
    assert!(err.to_string().contains("TLS handshake failed"));
}

#[test]
fn reality_shares_use_an_allowed_sni_and_support_legacy_dest_and_raw_transport() {
    let ib = json!({"port":4443,"settings":{"encryption":"none"},"streamSettings":{"network":"raw",
        "rawSettings":{"header":{"type":"none"}},"security":"reality",
        "realitySettings":{"dest":"primary.example.test:443","serverNames":["*.example.test","alt.example.test","primary.example.test"],
            "settings":{"publicKey":"valid-public","serverName":"stale.example.test"},
            "shortIds":["broken","fa10"]}}});
    let link = line::share_link(
        &ib,
        "用户 A",
        "a7b78ef7-5b94-41b0-8b33-74c4a69c932a",
        "vpn.example.test",
        "xtls-rprx-vision",
    )
    .unwrap();
    let url = Url::parse(&link).unwrap();
    let query: HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["sni"], "primary.example.test");
    assert_eq!(query["sid"], "fa10");
    assert_eq!(query["type"], "tcp");
    assert_eq!(query["flow"], "xtls-rprx-vision");
    let mut reality = ib["streamSettings"]["realitySettings"].clone();
    reality["settings"]["serverName"] = json!("alt.example.test");
    assert_eq!(reality::share_sni(&reality).unwrap(), "alt.example.test");
}

#[tokio::test]
async fn rejects_login_pages_in_place_of_backups() {
    let api = panel(Router::new().route(
        "/private/path/panel/api/server/getDb",
        get(|| async { "<html>Sign in</html>" }),
    ))
    .await;
    assert!(api.backup().await.is_err());
}

#[tokio::test]
async fn streams_database_with_a_split_signature_and_unknown_length() {
    use axum::body::{Body, Bytes};
    let api = panel(Router::new().route(
        "/private/path/panel/api/server/getDb",
        get(|| async {
            let chunks = [
                Ok::<_, std::io::Error>(Bytes::from_static(b"PG")),
                Ok(Bytes::from_static(b"DMPextra payload")),
            ];
            Body::from_stream(futures_util::stream::iter(chunks))
        }),
    ))
    .await;
    use std::io::Read;
    let mut backup = api.backup().await.unwrap();
    assert_eq!(backup.extension, "dump");
    let mut bytes = Vec::new();
    backup.file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"PGDMPextra payload");
    assert_eq!(backup.size, bytes.len() as u64);
}
