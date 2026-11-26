use super::PanelApi;
use anyhow::{anyhow, bail, ensure, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

impl PanelApi {
    /// The caller serializes read/modify/write operations per panel.
    pub async fn apply(&self, action: &str, payload: &Value) -> Result<String> {
        match action {
            "inbound.create" => self.create_inbound(payload).await,
            "inbound.expiry" => self.inbound_expiry(payload).await,
            "client.create" => self.create_client(payload).await,
            "client.expiry" => self.client_expiry(payload).await,
            "outbound.upsert" | "route.upsert" => self.update_config(action, payload).await,
            _ => bail!("不支持的操作类型"),
        }
    }

    async fn create_inbound(&self, p: &Value) -> Result<String> {
        let inbound = self.reality_inbound(p).await?;
        let remark = required_text(p, "remark")?;
        let port = &p["port"];
        let created = self
            .obj(self.request(Method::POST, "inbounds/add").json(&inbound))
            .await?;
        Ok(match created["id"].as_u64() {
            Some(id) => format!("已创建入站 {remark}（ID {id}，端口 {port}）"),
            None => format!("已创建入站 {remark}（端口 {port}）"),
        })
    }

    pub(super) async fn reality_inbound(&self, p: &Value) -> Result<Value> {
        let remark = required_text(p, "remark")?;
        let port = p["port"]
            .as_u64()
            .filter(|v| (1..=65535).contains(v))
            .context("端口须为 1 至 65535")?;
        let preferred = p["serverName"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let target = p["dest"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| preferred.map(|name| format!("{name}:443")))
            .unwrap_or_else(super::default_reality_target);
        let expiry = expiry_time(p, false)?;
        let listen = p["listen"].as_str().unwrap_or("").trim();
        ensure!(
            listen.is_empty() || listen.parse::<std::net::IpAddr>().is_ok(),
            "入站监听地址须为有效 IP 地址"
        );
        let names = self.reality_names(&target, preferred).await?;
        let keys = self
            .obj(self.request(Method::GET, "server/getNewX25519Cert"))
            .await?;
        let private_key = required_text(&keys, "privateKey")?;
        let public_key = required_text(&keys, "publicKey")?;
        let short_id = Uuid::new_v4().simple().to_string();
        let inbound = json!({
            "remark":remark, "port":port, "enable":true, "expiryTime":expiry,
            "listen":listen, "protocol":"vless", "trafficReset":"never",
            "settings":{"clients":[],"decryption":"none","encryption":"none","fallbacks":[]},
            "streamSettings":{"network":"tcp","security":"reality",
                "tcpSettings":{"header":{"type":"none"}},
                "realitySettings":{"show":false,"xver":0,"target":target,"serverNames":names,
                    "privateKey":private_key,"shortIds":[&short_id[..16]],
                    "settings":{"publicKey":public_key,"serverName":names[0],"fingerprint":"chrome","spiderX":"/"}}},
            "sniffing":{"enabled":true,"destOverride":["http","tls","quic"],"routeOnly":false}
        });
        Ok(inbound)
    }

    pub(super) async fn inbound(&self, id: u64) -> Result<Value> {
        let row = self
            .obj(self.request(Method::GET, &format!("inbounds/get/{id}")))
            .await?;
        ensure!(
            row["id"].as_u64() == Some(id) && row.get("settings").is_some(),
            "面板未返回指定入站的完整配置"
        );
        Ok(row)
    }

    async fn inbound_expiry(&self, p: &Value) -> Result<String> {
        let id = inbound_id(p)?;
        let expiry = expiry_time(p, true)?;
        let mut row = self.inbound(id).await?;
        row["expiryTime"] = json!(expiry);
        self.save_inbound(id, row).await?;
        Ok(format!("已更新入站 #{id} 的到期时间"))
    }

    pub(super) async fn save_inbound(&self, id: u64, mut row: Value) -> Result<Value> {
        // Client traffic is live telemetry and is not an editable configuration field.
        row.as_object_mut()
            .context("面板返回的入站配置无效")?
            .remove("clientStats");
        self.obj(
            self.request(Method::POST, &format!("inbounds/update/{id}"))
                .json(&row),
        )
        .await
    }

    async fn create_client(&self, p: &Value) -> Result<String> {
        let id = inbound_id(p)?;
        let email = email_id(p)?;
        let expiry = expiry_time(p, false)?;
        let uuid = match p["uuid"].as_str().map(str::trim).filter(|v| !v.is_empty()) {
            Some(id) => Uuid::parse_str(id).context("客户端 UUID 无效")?,
            None => Uuid::new_v4(),
        };
        let flow = p["flow"].as_str().unwrap_or("xtls-rprx-vision");
        ensure!(
            matches!(flow, "" | "xtls-rprx-vision" | "xtls-rprx-vision-udp443"),
            "VLESS Flow 无效"
        );
        let inbound = self.inbound(id).await?;
        ensure!(inbound["protocol"] == "vless", "所选入站协议须为 VLESS");
        let body = json!({"inboundIds":[id], "client":{
            "email":email, "id":uuid.to_string(), "enable":true, "expiryTime":expiry,
            "flow":flow, "security":"auto", "subId":Uuid::new_v4().simple().to_string(),
        }});
        let result = self
            .obj(self.request(Method::POST, "clients/add").json(&body))
            .await?;
        Ok(with_pending(
            format!("已在入站 #{id} 创建客户端 {email}"),
            &result,
        ))
    }

    async fn client_expiry(&self, p: &Value) -> Result<String> {
        let email = email_id(p)?;
        let expiry = expiry_time(p, true)?;
        let mut data = self.client_details(email).await?;
        if p.get("inboundId").is_some() {
            let id = inbound_id(p)?;
            ensure!(
                data["inboundIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(id))),
                "此客户端未关联所选入站"
            );
        }
        let pending = self
            .save_client_record(data["client"].take(), email, expiry)
            .await?;
        Ok(with_pending(
            format!("已更新客户端 {email} 的到期时间"),
            &pending,
        ))
    }

    pub async fn client_details(&self, email: &str) -> Result<Value> {
        let url = self.item_url("clients/get/", email)?;
        self.obj(self.request_url(Method::GET, url)).await
    }

    pub async fn save_client_record(
        &self,
        record: Value,
        email: &str,
        expiry: i64,
    ) -> Result<Value> {
        let client = client_update_body(record, email, expiry)?;
        let url = self.item_url("clients/update/", email)?;
        self.obj(self.request_url(Method::POST, url).json(&client))
            .await
    }

    async fn update_config(&self, action: &str, p: &Value) -> Result<String> {
        let mut wrapper = self.xray_settings().await?;
        let mut cfg = wrapper["xraySetting"].take();
        let message = match action {
            "outbound.upsert" => {
                let outbound = &p["outbound"];
                let tag = required_text(outbound, "tag")?;
                required_text(outbound, "protocol")?;
                ensure!(
                    !wrapper["subscriptionOutboundTags"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(tag))),
                    "该出站标签由订阅管理"
                );
                let outbounds = config_array(&mut cfg, "outbounds")?;
                if let Some(existing) = outbounds.iter_mut().find(|item| item["tag"] == tag) {
                    *existing = outbound.clone();
                } else {
                    outbounds.push(outbound.clone());
                }
                format!("已保存出站 {tag}")
            }
            _ => {
                let rule = &p["rule"];
                ensure!(rule.is_object(), "路由规则须为 JSON 对象");
                ensure!(
                    rule["outboundTag"].as_str().is_some_and(|v| !v.is_empty())
                        ^ rule["balancerTag"].as_str().is_some_and(|v| !v.is_empty()),
                    "路由规则须指定一个出站标签或均衡器"
                );
                let rules = config_array(&mut cfg["routing"], "rules")?;
                if let Some(index) = p.get("index") {
                    let i = index
                        .as_u64()
                        .and_then(|v| usize::try_from(v).ok())
                        .context("路由索引无效")?;
                    let existing = rules.get_mut(i).context("路由索引不存在，请刷新列表")?;
                    if let Some(expected) = p.get("expectedRule") {
                        ensure!(expected == existing, "该路由规则已经变化，请刷新后重试");
                    }
                    *existing = rule.clone();
                } else if !rules.contains(rule) {
                    rules.push(rule.clone());
                }
                "已保存路由规则".to_owned()
            }
        };
        self.save_xray_config(&cfg, &wrapper).await?;
        Ok(message)
    }

    pub(super) async fn save_xray_config(&self, cfg: &Value, wrapper: &Value) -> Result<()> {
        let test_url = wrapper["outboundTestUrl"]
            .as_str()
            .context("面板缺少出站测试地址")?;
        let json = serde_json::to_string(&cfg)?;
        self.obj(self.request(Method::POST, "xray/update").form(&[
            ("xraySetting", json.as_str()),
            ("outboundTestUrl", test_url),
        ]))
        .await
        .context("保存 Xray 配置失败，请刷新并核对面板状态")?;
        Ok(())
    }
}

fn required_text<'a>(obj: &'a Value, field: &str) -> Result<&'a str> {
    obj[field]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("字段 {field} 不能为空"))
}

fn inbound_id(p: &Value) -> Result<u64> {
    p["inboundId"]
        .as_u64()
        .filter(|n| *n > 0)
        .context("请输入有效的入站 ID")
}

fn email_id(p: &Value) -> Result<&str> {
    let email = required_text(p, "email")?;
    ensure!(
        !email
            .chars()
            .any(|c| c == ' ' || c == '/' || c == '\\' || c.is_control())
            && !matches!(email, "." | ".."),
        "客户端标识包含无效字符"
    );
    Ok(email)
}

fn expiry_time(p: &Value, required: bool) -> Result<i64> {
    if !required && p.get("expiryTime").is_none() {
        return Ok(0);
    }
    p["expiryTime"]
        .as_i64()
        .filter(|v| *v >= 0)
        .context("到期时间须为毫秒时间戳，0 表示永不过期")
}

fn with_pending(message: String, obj: &Value) -> String {
    if obj["nodePending"].as_bool() == Some(true) {
        format!("{message}，等待节点同步")
    } else {
        message
    }
}

// The record returned by GET uses the database id and CSV WireGuard IPs.
// The update endpoint accepts model.Client, whose id is the service UUID.
pub(super) fn client_update_body(mut record: Value, email: &str, expiry: i64) -> Result<Value> {
    ensure!(
        record["email"] == email && record["enable"].is_boolean(),
        "面板未返回该客户端的完整配置"
    );
    let uuid = record["uuid"]
        .as_str()
        .context("面板未返回客户端 UUID 字段")?
        .to_owned();
    record["id"] = json!(uuid);
    if let Some(csv) = record["allowedIPs"].as_str() {
        record["allowedIPs"] = json!(csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>());
    }
    if let Some(created) = record["createdAt"].as_i64() {
        record["created_at"] = json!(created);
    }
    record["expiryTime"] = json!(expiry);
    Ok(record)
}

pub(super) fn config_array<'a>(owner: &'a mut Value, key: &str) -> Result<&'a mut Vec<Value>> {
    if owner.is_null() {
        *owner = json!({});
    }
    let object = owner.as_object_mut().context("面板 Xray 配置格式无效")?;
    object
        .entry(key)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("面板 Xray 配置的 {key} 不是数组"))
}
