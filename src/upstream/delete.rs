use super::PanelApi;
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    version: u8,
    email: String,
    client_id: i64,
    uuid: String,
    bindings: Vec<i64>,
    inbounds: Vec<Inlet>,
    rules: Vec<Rule>,
    outbounds: Vec<Outlet>,
    retained: usize,
}
#[derive(Serialize, Deserialize)]
struct Inlet {
    id: i64,
    tag: String,
    signature: String,
}
#[derive(Serialize, Deserialize)]
struct Rule {
    signature: String,
    tags: Vec<String>,
    #[serde(default)]
    edit_users: bool,
}
#[derive(Serialize, Deserialize)]
struct Outlet {
    tag: String,
    signature: String,
    inlets: BTreeSet<String>,
}

struct Snapshot {
    clients: Vec<Value>,
    inbounds: Vec<Value>,
    wrapper: Value,
}
impl PanelApi {
    async fn deletion_snapshot(&self) -> Result<Snapshot> {
        let (clients, inbounds, wrapper) =
            tokio::try_join!(self.clients(), self.inbounds(), self.xray_settings())?;
        Ok(Snapshot {
            clients,
            inbounds,
            wrapper,
        })
    }

    pub async fn plan_client_delete(
        &self,
        email: &str,
        client_id: i64,
        uuid: &str,
    ) -> Result<Value> {
        let state = self.deletion_snapshot().await?;
        let plan = Plan::build(&state, email, client_id, uuid)?;
        Ok(serde_json::to_value(plan)?)
    }

    pub async fn delete_client_cascade(&self, saved: &Value) -> Result<String> {
        let plan: Plan = serde_json::from_value(saved.clone()).context("删除计划格式无效")?;
        ensure!(
            plan.version == 1 && plan.client_id > 0,
            "删除计划版本或标识无效"
        );
        let mut state = self.deletion_snapshot().await?;
        if let Some(client) = identity(&state.clients, &plan.email, plan.client_id, &plan.uuid)? {
            ensure!(
                bindings(client)? == plan.bindings,
                "客户端的入站绑定已变化，请重新确认"
            );
            let url = self.item_url("clients/del/", &plan.email)?;
            self.obj(self.request_url(Method::POST, url))
                .await
                .context("删除客户端失败")?;
        }
        let mut cleared = HashSet::new();
        let mut retained = plan.retained;
        for inlet in &plan.inbounds {
            let (clients, inbounds) = tokio::try_join!(self.clients(), self.inbounds())?;
            ensure!(
                identity(&clients, &plan.email, plan.client_id, &plan.uuid)?.is_none(),
                "客户端仍存在，暂停清理关联配置"
            );
            if let Some(ib) = inbounds.iter().find(|ib| ib["id"] == inlet.id) {
                if inbound_signature(ib)? != inlet.signature || !dedicated(ib, &clients, None)? {
                    retained += 1;
                    continue;
                }
                self.obj(self.request(Method::POST, &format!("inbounds/del/{}", inlet.id)))
                    .await
                    .context("清理入站失败，可重试")?;
            }
            cleared.insert(inlet.tag.clone());
        }
        state = self.deletion_snapshot().await?;
        ensure!(
            identity(&state.clients, &plan.email, plan.client_id, &plan.uuid)?.is_none(),
            "同名客户端重新出现，暂停清理"
        );
        cleared.retain(|tag| !state.inbounds.iter().any(|ib| ib["tag"] == *tag));
        let (config, kept) = plan.cleaned_config(&state.wrapper, &cleared)?;
        retained += kept;
        if config != state.wrapper["xraySetting"] {
            self.save_xray_config(&config, &state.wrapper)
                .await
                .map_err(|_| anyhow!("保存路由和出站清理结果失败，请检查面板后重试"))?;
        }
        let mut message = format!(
            "已删除客户端 {}，已清理 {} 个专用入站",
            plan.email,
            cleared.len()
        );
        if retained > 0 {
            message.push_str(&format!(
                "；已保留 {retained} 项共享、已修改或归属不明的配置"
            ));
        }
        Ok(message)
    }
}

fn bindings(client: &Value) -> Result<Vec<i64>> {
    let ids = client["inboundIds"]
        .as_array()
        .context("客户端入站绑定信息不完整")?;
    let mut ids = ids
        .iter()
        .map(|id| id.as_i64().filter(|v| *v > 0).context("入站绑定无效"))
        .collect::<Result<Vec<_>>>()?;
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
fn identity<'a>(
    clients: &'a [Value],
    email: &str,
    id: i64,
    uuid: &str,
) -> Result<Option<&'a Value>> {
    ensure!(id > 0 && !email.is_empty(), "客户端名称或数据库 ID 无效");
    let mut found = None;
    for c in clients
        .iter()
        .filter(|c| c["email"] == email || c["id"] == id)
    {
        ensure!(
            found.is_none()
                && c["id"] == id
                && c["email"] == email
                && c["uuid"].as_str().unwrap_or_default() == uuid,
            "客户端已更名、重新创建或凭据已变化"
        );
        found = Some(c);
    }
    Ok(found)
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn settings(ib: &Value) -> Result<Value> {
    let s = &ib["settings"];
    if let Some(text) = s.as_str() {
        Ok(serde_json::from_str(text).context("无法解析入站协议设置")?)
    } else {
        Ok(s.clone())
    }
}
fn inbound_signature(ib: &Value) -> Result<String> {
    let mut options = settings(ib)?;
    if let Some(obj) = options.as_object_mut() {
        obj.remove("clients");
    }
    Ok(digest(&json!([
        ib["id"],
        ib["remark"],
        ib["tag"],
        ib["protocol"],
        ib["listen"],
        ib["port"],
        ib["nodeId"],
        ib["streamSettings"],
        options
    ])))
}
fn reserved(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "" | "api" | "direct" | "blocked" | "block" | "dns" | "dns-out" | "blackhole" | "freedom"
    )
}
fn dedicated(ib: &Value, clients: &[Value], owner: Option<(i64, &str)>) -> Result<bool> {
    // Only client-bearing inbound protocols can be scoped by their client list.
    if !matches!(
        ib["protocol"].as_str(),
        Some("vless" | "vmess" | "trojan" | "shadowsocks" | "hysteria" | "hysteria2")
    ) {
        return Ok(false);
    }
    let id = ib["id"].as_i64().context("入站 ID 无效")?;
    for c in clients {
        if bindings(c)?.contains(&id) && !owner.is_some_and(|(cid, _)| c["id"] == cid) {
            return Ok(false);
        }
    }
    let options = settings(ib)?;
    let Some(entries) = options["clients"].as_array() else {
        return Ok(false);
    };
    if entries
        .iter()
        .any(|u| !owner.is_some_and(|(_, email)| u["email"] == email))
    {
        return Ok(false);
    }
    // With no named client left, a new unauthenticated or fallback service could
    // still use the inlet. Keep any configuration that explicitly supports it.
    if options["fallbacks"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return Ok(false);
    }
    Ok(true)
}

fn values<'a>(cfg: &'a Value, key: &str) -> Result<&'a [Value]> {
    match cfg.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(v) => v.as_array().map(Vec::as_slice).context("Xray 配置格式无效"),
    }
}
fn rule_inlets(rule: &Value) -> Option<Vec<String>> {
    let tags = rule["inboundTag"].as_array()?;
    if tags.is_empty() {
        return None;
    }
    tags.iter().map(|t| t.as_str().map(str::to_owned)).collect()
}
impl Plan {
    fn build(s: &Snapshot, email: &str, id: i64, uuid: &str) -> Result<Self> {
        let client = identity(&s.clients, email, id, uuid)?.context("原客户端已不存在，请刷新")?;
        let mut p = Plan {
            version: 1,
            email: email.into(),
            client_id: id,
            uuid: uuid.into(),
            bindings: bindings(client)?,
            inbounds: vec![],
            rules: vec![],
            outbounds: vec![],
            retained: 0,
        };
        let cfg = &s.wrapper["xraySetting"];
        let mut exclusive = HashSet::new();
        for inlet in &p.bindings {
            let ib = s
                .inbounds
                .iter()
                .find(|i| i["id"] == *inlet)
                .context("客户端关联的入站不存在，请刷新")?;
            let tag = ib["tag"].as_str().unwrap_or_default();
            if reserved(tag)
                || s.inbounds.iter().filter(|i| i["tag"] == tag).count() != 1
                || values(cfg, "inbounds")?.iter().any(|i| i["tag"] == tag)
                || !dedicated(ib, &s.clients, Some((id, email)))?
            {
                p.retained += 1;
                continue;
            }
            exclusive.insert(tag.to_owned());
            p.inbounds.push(Inlet {
                id: *inlet,
                tag: tag.into(),
                signature: inbound_signature(ib)?,
            });
        }
        let mut outputs = std::collections::BTreeMap::<String, BTreeSet<String>>::new();
        for inlet in &p.inbounds {
            if inlet.tag == email {
                outputs
                    .entry(inlet.tag.clone())
                    .or_default()
                    .insert(inlet.tag.clone());
            }
        }
        let match_user = super::line::user_matcher(email);
        for rule in values(&cfg["routing"], "rules")? {
            if let Some(tags) = rule_inlets(rule) {
                if tags.iter().all(|t| exclusive.contains(t)) {
                    p.rules.push(Rule {
                        signature: digest(rule),
                        tags: tags.clone(),
                        edit_users: false,
                    });
                    if let Some(tag) = rule["outboundTag"].as_str().filter(|tag| !reserved(tag)) {
                        outputs.entry(tag.to_owned()).or_default().extend(tags);
                    }
                    continue;
                } else if tags.iter().any(|t| exclusive.contains(t)) {
                    p.retained += 1;
                }
            }
            if let Some(users) = rule["user"]
                .as_array()
                .filter(|users| users.iter().any(|v| v == &match_user))
            {
                // Exact client matches are independent of inbound exclusivity. Other
                // users on the same rule keep the original condition and outbound.
                p.rules.push(Rule {
                    signature: digest(rule),
                    tags: vec![],
                    edit_users: true,
                });
                if users.iter().all(|u| u == &match_user) {
                    if let Some(tag) = rule["outboundTag"].as_str().filter(|tag| !reserved(tag)) {
                        outputs.entry(tag.to_owned()).or_default();
                    }
                }
            }
        }
        let outbounds = values(cfg, "outbounds")?;
        for (tag, inlets) in outputs {
            let mut matches = outbounds.iter().filter(|o| o["tag"] == tag);
            if let Some(outbound) = matches.next() {
                if matches.next().is_some()
                    || outbound["protocol"] != "socks"
                    || outbounds.first() == Some(outbound)
                    || managed(&s.wrapper, &tag)
                {
                    p.retained += 1;
                    continue;
                }
                p.outbounds.push(Outlet {
                    tag,
                    signature: digest(outbound),
                    inlets,
                });
            }
        }
        Ok(p)
    }

    fn cleaned_config(&self, wrapper: &Value, cleared: &HashSet<String>) -> Result<(Value, usize)> {
        let mut cfg = wrapper["xraySetting"].clone();
        let mut kept = 0;
        let match_user = super::line::user_matcher(&self.email);
        let allowed: std::collections::HashMap<_, _> = self
            .rules
            .iter()
            .filter(|r| r.tags.iter().all(|tag| cleared.contains(tag)))
            .map(|r| (&r.signature, r))
            .collect();
        if let Some(rules) = cfg["routing"]["rules"].as_array_mut() {
            rules.retain_mut(|rule| {
                if let Some(planned) = allowed.get(&digest(rule)) {
                    if planned.edit_users {
                        if let Some(users) = rule["user"].as_array_mut() {
                            users.retain(|u| u != &match_user);
                            return !users.is_empty();
                        }
                        return true;
                    }
                    return false;
                }
                if rule_inlets(rule)
                    .is_some_and(|tags| tags.iter().any(|tag| cleared.contains(tag)))
                    || rule["user"]
                        .as_array()
                        .is_some_and(|users| users.iter().any(|u| u == &match_user))
                {
                    kept += 1;
                }
                true
            });
        }
        for candidate in &self.outbounds {
            let tag = candidate.tag.as_str();
            if !candidate.inlets.iter().all(|t| cleared.contains(t)) {
                kept += 1;
                continue;
            }
            let obs = values(&cfg, "outbounds")?;
            let Some(i) = obs.iter().position(|ob| ob["tag"] == tag) else {
                continue;
            };
            if i == 0
                || managed(wrapper, tag)
                || digest(&obs[i]) != candidate.signature
                || referenced(&cfg, tag, i)?
            {
                kept += 1;
                continue;
            }
            cfg["outbounds"]
                .as_array_mut()
                .context("出站格式无效")?
                .remove(i);
        }
        Ok((cfg, kept))
    }
}

fn managed(wrapper: &Value, tag: &str) -> bool {
    ["subscriptionOutboundTags", "clientReverseTags"]
        .iter()
        .any(|key| {
            wrapper[*key]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|v| v == tag))
        })
}
fn matches_tag(value: &Value, tag: &str) -> bool {
    match value {
        Value::String(s) => s == tag,
        Value::Object(map) => map.values().any(|v| matches_tag(v, tag)),
        Value::Array(items) => items.iter().any(|v| matches_tag(v, tag)),
        _ => false,
    }
}
fn selectors_match(config: &Value, tag: &str) -> bool {
    match config {
        Value::Object(map) => map.iter().any(|(k, v)| {
            if matches!(k.as_str(), "selector" | "subjectSelector") {
                if let Some(list) = v.as_array() {
                    return list
                        .iter()
                        .any(|s| s.as_str().is_some_and(|s| tag.starts_with(s)));
                }
            }
            selectors_match(v, tag)
        }),
        Value::Array(items) => items.iter().any(|v| selectors_match(v, tag)),
        _ => false,
    }
}
fn referenced(cfg: &Value, tag: &str, index: usize) -> Result<bool> {
    if selectors_match(cfg, tag) {
        return Ok(true);
    }
    for ob in values(cfg, "outbounds")?
        .iter()
        .enumerate()
        .filter_map(|(i, ob)| (i != index).then_some(ob))
    {
        if matches_tag(ob, tag) {
            return Ok(true);
        }
    }
    Ok(cfg
        .as_object()
        .context("Xray 配置格式无效")?
        .iter()
        .any(|(key, val)| key != "outbounds" && matches_tag(val, tag)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Snapshot {
        Snapshot {
            clients: vec![
                json!({"id":4,"email":"甲","uuid":"","inboundIds":[7]}),
                json!({"id":5,"email":"乙","uuid":"","inboundIds":[7]}),
            ],
            inbounds: vec![
                json!({"id":7,"tag":"private-in","remark":"组","port":20344,"protocol":"trojan","settings":{"clients":[{"email":"甲","password":"keep-secret"},{"email":"乙","password":"second-secret"}]}}),
            ],
            wrapper: json!({"outboundTestUrl":"https://x.test","xraySetting":{"outbounds":[
                {"tag":"direct","protocol":"freedom"},{"tag":"edge-1","protocol":"socks","settings":{"servers":[{"users":[{"user":"a","pass":"outbound-secret"}]}]}}
            ],"routing":{"rules":[{"type":"field","inboundTag":["private-in"],"outboundTag":"edge-1"}]}}}),
        }
    }
    #[test]
    fn user_routing_on_a_shared_inlet_removes_only_selected_users() -> Result<()> {
        let mut s = fixture();
        let rules = s.wrapper["xraySetting"]["routing"]["rules"]
            .as_array_mut()
            .unwrap();
        rules.push(json!({"type":"field","user":["甲"],"outboundTag":"edge-1"}));
        rules.push(json!({"type":"field","user":["甲","乙"],"outboundTag":"edge-1","port":"80"}));
        rules.push(json!({"type":"field","user":["乙"],"outboundTag":"direct"}));
        let plan = Plan::build(&s, "甲", 4, "")?;
        let (cfg, kept) = plan.cleaned_config(&s.wrapper, &HashSet::new())?;
        assert!(kept > 0);
        let remaining = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(
            remaining[1],
            json!({"type":"field","user":["乙"],"outboundTag":"edge-1","port":"80"})
        );
        assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 2);
        let rules = s.wrapper["xraySetting"]["routing"]["rules"]
            .as_array_mut()
            .unwrap();
        rules[1]["port"] = json!("443");
        let (changed, _) = plan.cleaned_config(&s.wrapper, &HashSet::new())?;
        assert_eq!(changed["routing"]["rules"][1]["user"], json!(["甲"]));
        Ok(())
    }
    #[test]
    fn deleting_last_client_keeps_shared_outlets_and_detects_tag_reuse() -> Result<()> {
        let mut s = fixture();
        let first = Plan::build(&s, "甲", 4, "")?;
        assert!(first.inbounds.is_empty());
        assert!(first.rules.is_empty());
        s.clients.remove(0);
        s.inbounds[0]["settings"]["clients"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let last = Plan::build(&s, "乙", 5, "")?;
        assert_eq!(
            (last.inbounds.len(), last.rules.len(), last.outbounds.len()),
            (1, 1, 1)
        );
        let data = serde_json::to_string(&last)?;
        assert!(!data.contains("outbound-secret") && !data.contains("second-secret"));
        let cleared = HashSet::from(["private-in".to_owned()]);
        let (clean, kept) = last.cleaned_config(&s.wrapper, &cleared)?;
        assert_eq!(kept, 0);
        assert_eq!(clean["outbounds"].as_array().unwrap().len(), 1);
        assert_eq!(clean["routing"]["rules"], json!([]));
        let unchanged = last.cleaned_config(&s.wrapper, &HashSet::new())?.0;
        assert_eq!(unchanged, s.wrapper["xraySetting"]);
        for addition in [
            json!({"routing":{"balancers":[{"tag":"b","selector":["ed"]}]}}),
            json!({"observatory":{"subjectSelector":["edge"]}}),
            json!({"routing":{"rules":[{"domain":["example.test"],"outboundTag":"edge-1"}]}}),
            json!({"outbounds":[{"tag":"chain","streamSettings":{"sockopt":{"dialerProxy":"edge-1"}}}]}),
        ] {
            let mut wrapper = s.wrapper.clone();
            let cfg = &mut wrapper["xraySetting"];
            if let Some(extra) = addition["routing"]["rules"].as_array() {
                cfg["routing"]["rules"]
                    .as_array_mut()
                    .unwrap()
                    .extend(extra.to_owned());
            } else if let Some(extra) = addition["outbounds"].as_array() {
                cfg["outbounds"]
                    .as_array_mut()
                    .unwrap()
                    .extend(extra.to_owned());
            } else if addition.get("observatory").is_some() {
                cfg["observatory"] = addition["observatory"].clone();
            } else {
                cfg["routing"]["balancers"] = addition["routing"]["balancers"].clone();
            }
            let (result, kept) = last.cleaned_config(&wrapper, &cleared)?;
            assert!(kept > 0);
            assert!(result["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["tag"] == "edge-1"));
        }
        s.clients[0]["id"] = json!(7);
        assert!(identity(&s.clients, "乙", 5, "").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn retry_after_interrupted_cleanup_does_not_repeat_client_delete() -> Result<()> {
        use axum::{
            extract::{Form, State},
            routing::{get, post},
            Json, Router,
        };
        use std::{
            collections::HashMap,
            sync::{Arc, Mutex},
        };
        let mut start = fixture();
        start.clients.pop();
        start.inbounds[0]["settings"]["clients"]
            .as_array_mut()
            .unwrap()
            .pop();
        struct TestState {
            state: Snapshot,
            attempts: usize,
            deleted: usize,
        }
        type SharedTest = State<Arc<Mutex<TestState>>>;
        let shared = Arc::new(Mutex::new(TestState {
            state: start,
            attempts: 0,
            deleted: 0,
        }));
        fn ok(v: Value) -> Json<Value> {
            Json(json!({"success":true,"obj":v}))
        }
        let app = Router::new()
            .route(
                "/panel/api/clients/list",
                get(|s: SharedTest| async move { ok(json!(s.lock().unwrap().state.clients)) }),
            )
            .route(
                "/panel/api/inbounds/list",
                get(|s: SharedTest| async move { ok(json!(s.lock().unwrap().state.inbounds)) }),
            )
            .route(
                "/panel/api/xray/",
                post(|s: SharedTest| async move { ok(s.lock().unwrap().state.wrapper.clone()) }),
            )
            .route(
                "/panel/api/clients/del/:email",
                post(|s: SharedTest| async move {
                    let mut s = s.lock().unwrap();
                    s.deleted += 1;
                    s.state.clients.clear();
                    s.state.inbounds[0]["settings"]["clients"] = json!([]);
                    ok(Value::Null)
                }),
            )
            .route(
                "/panel/api/inbounds/del/7",
                post(|s: SharedTest| async move {
                    let mut s = s.lock().unwrap();
                    s.state.inbounds.clear();
                    ok(Value::Null)
                }),
            )
            .route(
                "/panel/api/xray/update",
                post(
                    |s: SharedTest, Form(fields): Form<HashMap<String, String>>| async move {
                        let mut s = s.lock().unwrap();
                        s.attempts += 1;
                        if s.attempts == 1 {
                            return Json(json!({"success":false,"msg":"failed"}));
                        }
                        s.state.wrapper["xraySetting"] =
                            serde_json::from_str(&fields["xraySetting"]).unwrap();
                        ok(Value::Null)
                    },
                ),
            )
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let api = PanelApi::new(
            reqwest::Client::builder().no_proxy().build()?,
            &format!("http://{addr}"),
            "mock",
        )?;
        let plan = api.plan_client_delete("甲", 4, "").await?;
        assert!(api.delete_client_cascade(&plan).await.is_err());
        api.delete_client_cascade(&plan).await?;
        api.delete_client_cascade(&plan).await?;
        let s = shared.lock().unwrap();
        assert_eq!((s.deleted, s.attempts), (1, 2));
        assert_eq!(
            s.state.wrapper["xraySetting"]["routing"]["rules"],
            json!([])
        );
        Ok(())
    }
}
