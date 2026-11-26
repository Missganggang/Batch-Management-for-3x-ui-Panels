use super::{
    actions::{client_update_body, config_array},
    PanelApi,
};
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::{Method, Url};
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

impl PanelApi {
    pub async fn online_clients(&self) -> Result<Vec<String>> {
        let rows = self
            .obj(self.request(Method::POST, "clients/onlines"))
            .await?;
        if rows.is_null() {
            return Ok(Vec::new());
        }
        serde_json::from_value(rows).context("面板返回的在线列表格式不正确")
    }

    pub async fn renew_line(&self, id: u64, email: &str, uuid: &str, expiry: i64) -> Result<()> {
        ensure!(expiry >= 0, "到期时间无效");
        let url = self.item_url("clients/get/", email)?;
        let mut row = self.obj(self.request_url(Method::GET, url)).await?;
        ensure!(
            row["client"]["uuid"] == uuid
                && row["inboundIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(id))),
            "线路客户端或入站绑定已变化"
        );
        let mut client = client_update_body(row["client"].take(), email, expiry)?;
        client["enable"] = json!(true);
        let url = self.item_url("clients/update/", email)?;
        self.obj(self.request_url(Method::POST, url).json(&client))
            .await?;
        Ok(())
    }

    /// Persist the caller's UUID before starting. The disabled inbound and its embedded
    /// client are created together, so a retry can recognize ownership even after a lost response.
    pub async fn provision_line(&self, spec: &Value, client_uuid: &str) -> Result<Value> {
        let name = spec["name"]
            .as_str()
            .map(str::trim)
            .context("请输入线路名称")?;
        ensure!(
            !name.is_empty()
                && name.chars().count() <= 80
                && !matches!(name, "api" | "." | "..")
                && !name
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\')),
            "线路名称无法作为面板客户端标识或标签"
        );
        let uuid = Uuid::parse_str(client_uuid)
            .context("线路 UUID 无效")?
            .to_string();
        let server = parse_socks(spec["socks5"].as_str().context("请输入 SOCKS5 地址")?)?;
        let outbound = json!({"tag":name,"protocol":"socks","settings":{"servers":[server]}});
        let rule = user_rule(name);
        let connect = canonical_host(
            spec["connectHost"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| self.base.host_str())
                .context("请输入连接域名或 IP")?,
        )?;
        let expiry = spec["expiryTime"].as_i64().unwrap_or(0);
        ensure!(expiry >= 0, "到期时间无效");
        let quota = spec["quotaBytes"].as_i64().unwrap_or(0);
        ensure!(quota >= 0, "流量上限无效");
        let (inbounds, clients, wrapper) =
            tokio::try_join!(self.inbounds(), self.clients(), self.xray_settings())?;
        let matches = inbounds
            .iter()
            .filter(|ib| ib["remark"] == name || ib["tag"] == name)
            .collect::<Vec<_>>();
        ensure!(matches.len() <= 1, "面板上存在多个同名入站");
        let normalized = matches
            .first()
            .map(|ib| normalized_inbound(ib))
            .transpose()?;
        let previous = normalized.as_ref();
        if let Some(ib) = previous {
            ensure!(
                ib["tag"] == name
                    && ib["remark"] == name
                    && ib["protocol"] == "vless"
                    && ib["streamSettings"]["security"] == "reality"
                    && matches!(
                        ib["streamSettings"]["network"].as_str(),
                        Some("tcp" | "raw")
                    )
                    && ib["settings"]["clients"]
                        .as_array()
                        .is_some_and(|users| users.len() == 1
                            && users[0]["id"] == uuid
                            && users[0]["email"] == name),
                "同名入站不属于此次开通，已保留原配置"
            );
        }
        for client in clients
            .iter()
            .filter(|c| c["email"] == name || c["uuid"] == uuid)
        {
            ensure!(
                previous.is_some_and(|ib| client["email"] == name
                    && client["uuid"] == uuid
                    && client["inboundIds"] == json!([ib["id"]])),
                "已有其他客户端占用该线路名称或 UUID"
            );
        }
        // Resolve all existing name conflicts before creating an inbound.
        check_config_names(&wrapper, name, previous.is_some(), &outbound, &rule)?;
        let ib = if let Some(existing) = previous {
            existing.clone()
        } else {
            let port = allocate_port(
                spec,
                &inbounds,
                &wrapper,
                self.base.port_or_known_default(),
                &uuid,
            )?;
            let args = json!({"remark":name,"port":port,"listen":spec["listen"],
                "serverName":spec["serverName"],"dest":spec["dest"]});
            let mut ib = self.reality_inbound(&args).await?;
            ib["tag"] = json!(name);
            ib["enable"] = json!(false);
            ib["settings"]["clients"] = json!([{"id":uuid,"email":name,"flow":"xtls-rprx-vision",
                "subId":Uuid::new_v4().simple().to_string(),"enable":true,"expiryTime":expiry,"totalGB":quota}]);
            let created = self
                .obj(self.request(Method::POST, "inbounds/add").json(&ib))
                .await?;
            ensure!(
                created["tag"] == name,
                "面板为该入站分配了不同的标签，请检查并处理名称冲突"
            );
            normalized_inbound(&created)?
        };
        let id = ib["id"].as_u64().context("面板缺少入站 ID")?;
        let port = ib["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .context("面板入站端口无效")?;
        let client = &ib["settings"]["clients"][0];
        let flow = client["flow"].as_str().unwrap_or_default();
        let actual_expiry = client["expiryTime"].as_i64().unwrap_or(0);
        let actual_quota = client["totalGB"].as_i64().unwrap_or(0);
        ensure!(
            (ib["enable"] == true && previous.is_some())
                || (actual_expiry == expiry && actual_quota == quota),
            "面板返回的客户端期限或流量上限与请求不同，请核对"
        );
        let link = share_link(&ib, name, &uuid, &connect, flow)?;
        // Fetch again after creating the inbound to preserve intervening template edits.
        let wrapper = self.xray_settings().await?;
        check_config_names(&wrapper, name, previous.is_some(), &outbound, &rule)?;
        let mut cfg = wrapper["xraySetting"].clone();
        let outbounds = config_array(&mut cfg, "outbounds")?;
        if !outbounds.contains(&outbound) {
            outbounds.push(outbound);
        }
        let rules = config_array(&mut cfg["routing"], "rules")?;
        install_rule(rules, name);
        // An interrupted first save may have committed the DB but failed to reload Xray.
        // Reapply the template while the inlet is disabled before accepting any traffic.
        if !ib["enable"].as_bool().unwrap_or(false) || cfg != wrapper["xraySetting"] {
            self.save_xray_config(&cfg, &wrapper)
                .await
                .map_err(|_| anyhow!("出站与路由未完成应用，请检查面板的 Xray 状态后重试"))?;
        }
        if ib["enable"] != true {
            self.obj(
                self.request(Method::POST, &format!("inbounds/setEnable/{id}"))
                    .json(&json!({"enable":true})),
            )
            .await?;
        }
        Ok(
            json!({"name":name,"email":name,"inboundId":id,"inboundTag":name,"outboundTag":name,
            "ruleTag":name,"port":port,"uuid":uuid,"connectHost":connect,"link":link,
            "expiryTime":actual_expiry,"quotaBytes":actual_quota}),
        )
    }

    pub async fn repair_user_routes(&self, owned: &[(String, String, i64)]) -> Result<Value> {
        if owned.is_empty() {
            return Ok(json!({"changed":0,"skipped":0}));
        }
        let (inbounds, clients, wrapper) =
            tokio::try_join!(self.inbounds(), self.clients(), self.xray_settings())?;
        let (cfg, report) = repair_config(owned, &inbounds, &clients, &wrapper)?;
        if cfg != wrapper["xraySetting"] {
            ensure!(
                self.xray_settings().await? == wrapper,
                "面板配置在修复期间发生变化，请刷新重试"
            );
            self.save_xray_config(&cfg, &wrapper).await?;
        }
        Ok(report)
    }
}

pub(super) fn normalized_inbound(raw: &Value) -> Result<Value> {
    let mut inbound = raw.clone();
    for field in ["settings", "streamSettings"] {
        if let Some(text) = raw[field].as_str() {
            inbound[field] = serde_json::from_str(text).context("无法解析面板入站配置")?;
        }
    }
    Ok(inbound)
}
pub(super) fn user_matcher(email: &str) -> String {
    // Xray 26.6 compares plain entries by exact equality. Only 'regexp:' is special.
    if email.len() <= 7 || !email.starts_with("regexp:") {
        return email.to_owned();
    }
    let mut literal = String::from("regexp:^");
    for c in email.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            literal.push('\\');
        }
        literal.push(c);
    }
    literal.push('$');
    literal
}
fn user_rule(name: &str) -> Value {
    json!({"ruleTag":name,"type":"field","user":[user_matcher(name)],"outboundTag":name})
}
fn legacy_rule(name: &str) -> Value {
    json!({"ruleTag":name,"type":"field","inboundTag":[name],"outboundTag":name})
}
fn matches_template(rule: &Value, expected: &Value) -> bool {
    if rule == expected {
        return true;
    }
    if !rule["enabled"].is_boolean() {
        return false;
    }
    let mut expected = expected.clone();
    expected["enabled"] = rule["enabled"].clone();
    expected == *rule
}
fn has_users(rule: &Value) -> bool {
    rule["user"]
        .as_array()
        .is_some_and(|users| !users.is_empty())
}
fn insert_block(rules: &mut Vec<Value>, block: Vec<Value>) {
    // The old creator inserted its exact inbound template before the other base
    // rules. Count those displaced base rules without reordering existing rows.
    let mut base_count = 0;
    let mut index = 0;
    while index < rules.len() && base_count < 3 {
        let r = &rules[index];
        if r["ruleTag"]
            .as_str()
            .is_some_and(|tag| matches_template(r, &legacy_rule(tag)))
        {
            index += 1;
            continue;
        }
        base_count += 1;
        index += 1;
    }
    while index < rules.len() && has_users(&rules[index]) {
        index += 1;
    }
    rules.splice(index..index, block);
}
fn install_rule(rules: &mut Vec<Value>, name: &str) {
    let mut rule = user_rule(name);
    if let Some(index) = rules.iter().position(|r| r["ruleTag"] == name) {
        if matches_template(&rules[index], &rule) {
            return;
        }
        let old = rules.remove(index);
        if old["enabled"].is_boolean() {
            rule["enabled"] = old["enabled"].clone();
        }
    }
    insert_block(rules, vec![rule]);
}
fn repair_config(
    owned: &[(String, String, i64)],
    inbounds: &[Value],
    clients: &[Value],
    wrapper: &Value,
) -> Result<(Value, Value)> {
    let mut cfg = wrapper["xraySetting"].clone();
    let mut legacy = HashSet::new();
    let mut modern = HashSet::new();
    let mut skipped = 0;
    for (email, uuid, id) in owned {
        let ib = inbounds
            .iter()
            .filter(|ib| ib["id"] == *id)
            .filter_map(|ib| normalized_inbound(ib).ok())
            .collect::<Vec<_>>();
        let ob = cfg["outbounds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["tag"] == *email)
            .collect::<Vec<_>>();
        let cs = clients
            .iter()
            .filter(|c| c["email"] == *email || c["uuid"] == *uuid)
            .collect::<Vec<_>>();
        let tagged = cfg["routing"]["rules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["ruleTag"] == *email)
            .collect::<Vec<_>>();
        let proven = ib.len() == 1
            && ib[0]["tag"] == *email
            && ib[0]["remark"] == *email
            && ib[0]["protocol"] == "vless"
            && matches!(
                ib[0]["streamSettings"]["network"].as_str(),
                Some("tcp" | "raw")
            )
            && ib[0]["streamSettings"]["security"] == "reality"
            && ib[0]["settings"]["clients"]
                .as_array()
                .is_some_and(|items| {
                    items.len() == 1 && items[0]["id"] == *uuid && items[0]["email"] == *email
                })
            && cs.len() == 1
            && cs[0]["uuid"] == *uuid
            && cs[0]["email"] == *email
            && cs[0]["inboundIds"] == json!([id])
            && ob.len() == 1
            && ob[0]["protocol"] == "socks"
            && tagged.len() == 1;
        if !proven {
            skipped += 1;
            continue;
        }
        if matches_template(tagged[0], &legacy_rule(email)) {
            legacy.insert(email.as_str());
        } else if matches_template(tagged[0], &user_rule(email)) {
            modern.insert(email.as_str());
        } else {
            skipped += 1;
        }
    }
    let changed = legacy.len();
    if changed > 0 {
        let rules = config_array(&mut cfg["routing"], "rules")?;
        // Recover the original base prefix before taking the managed/user block.
        // Unrelated rules retain their relative order even on customized panels.
        let mut base = Vec::new();
        let mut block = Vec::new();
        let mut tail = Vec::new();
        for rule in std::mem::take(rules) {
            let tag = rule["ruleTag"].as_str();
            if let Some(name) = tag.filter(|n| legacy.contains(n)) {
                let mut new = user_rule(name);
                if rule["enabled"].is_boolean() {
                    new["enabled"] = rule["enabled"].clone();
                }
                block.push(new);
            } else if tag.is_some_and(|n| modern.contains(n)) {
                block.push(rule);
            } else if base.len() < 3 {
                base.push(rule);
            } else if tail.is_empty() && has_users(&rule) {
                block.push(rule);
            } else {
                tail.push(rule);
            }
        }
        base.extend(block);
        base.extend(tail);
        *rules = base;
    }
    Ok((cfg, json!({"changed":changed,"skipped":skipped})))
}

fn check_config_names(
    wrapper: &Value,
    name: &str,
    owned: bool,
    outbound: &Value,
    rule: &Value,
) -> Result<()> {
    let cfg = &wrapper["xraySetting"];
    for field in ["subscriptionOutboundTags", "clientReverseTags"] {
        ensure!(
            !wrapper[field]
                .as_array()
                .is_some_and(|tags| tags.contains(&json!(name))),
            "此名称已由订阅或反向出站使用"
        );
    }
    ensure!(
        !cfg["inbounds"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["tag"] == name)),
        "Xray 模板中存在同名入站"
    );
    let matching = cfg["outbounds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ob| ob["tag"] == name)
        .collect::<Vec<_>>();
    ensure!(
        matching.is_empty() || (owned && matching.as_slice() == [outbound]),
        "同名出站已存在或 SOCKS5 配置与本次开通不符"
    );
    let named = cfg["routing"]["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["ruleTag"] == name)
        .collect::<Vec<_>>();
    ensure!(
        named.is_empty()
            || (owned
                && named.len() == 1
                && (matches_template(named[0], rule)
                    || matches_template(named[0], &legacy_rule(name)))),
        "同名路由已存在且配置不同"
    );
    Ok(())
}

fn allocate_port(
    spec: &Value,
    rows: &[Value],
    wrapper: &Value,
    panel: Option<u16>,
    uuid: &str,
) -> Result<u16> {
    let mut used: HashSet<_> = rows
        .iter()
        .chain(
            wrapper["xraySetting"]["inbounds"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .filter_map(|ib| u16::try_from(ib["port"].as_u64()?).ok())
        .collect();
    used.extend(panel);
    if let Some(input) = spec.get("port") {
        let p = input
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .filter(|p| *p != 0)
            .context("入站端口无效")?;
        ensure!(!used.contains(&p), "端口已被占用");
        return Ok(p);
    }
    let configured =
        std::env::var("NEXUS_LINE_PORT_RANGE").unwrap_or_else(|_| "20000-20999".into());
    available_port(&configured, &used, uuid)
}

fn available_port(configured: &str, used: &HashSet<u16>, uuid: &str) -> Result<u16> {
    let invalid = "NEXUS_LINE_PORT_RANGE 须为起止端口，例如 20000-20999";
    let (first, last) = configured.split_once('-').context(invalid)?;
    let first = first.trim().parse::<u16>().context(invalid)?;
    let last = last.trim().parse::<u16>().context(invalid)?;
    ensure!(first > 0 && first <= last, invalid);
    let width = u32::from(last) - u32::from(first) + 1;
    let seed = u16::from_str_radix(&uuid[..4], 16)? as u32;
    (0..width)
        .map(|i| (u32::from(first) + (i + seed) % width) as u16)
        .find(|p| !used.contains(p))
        .with_context(|| format!("端口范围 {first}-{last} 内缺少可分配的入站端口"))
}

fn canonical_host(host: &str) -> Result<String> {
    let host = host.trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<std::net::Ipv6Addr>() {
        return Ok(format!("[{ip}]"));
    }
    ensure!(
        !host.is_empty()
            && !host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/:@?#%\\[]".contains(c)),
        "域名或 IP 地址无效"
    );
    Url::parse(&format!("http://{host}/"))
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .context("域名或 IP 地址无效")
}

fn parse_socks(raw: &str) -> Result<Value> {
    let raw = raw.trim();
    ensure!(
        !raw.is_empty() && raw.len() <= 4096 && !raw.chars().any(char::is_control),
        "SOCKS5 地址无效"
    );
    let (host, port, user, pass) = if raw.contains("://") {
        let u = Url::parse(raw).map_err(|_| anyhow!("SOCKS5 URL 无效"))?;
        ensure!(
            matches!(u.scheme(), "socks5" | "socks5h")
                && matches!(u.path(), "" | "/")
                && u.query().is_none()
                && u.fragment().is_none(),
            "请输入有效的 SOCKS5 URL"
        );
        let host = u.host_str().context("SOCKS5 缺少主机地址")?;
        (
            canonical_host(host)?,
            u.port().context("SOCKS5 缺少端口")?,
            decode_component(u.username())?,
            decode_component(u.password().unwrap_or(""))?,
        )
    } else {
        let (host, tail) = if raw.starts_with('[') {
            let i = raw.find("]:").context("IPv6 地址须使用方括号")?;
            (&raw[..=i], &raw[i + 2..])
        } else {
            raw.split_once(':').context("SOCKS5 需要主机和端口")?
        };
        let mut parts = tail.splitn(3, ':');
        let port = parts
            .next()
            .unwrap_or("")
            .parse::<u16>()
            .context("SOCKS5 端口无效")?;
        (
            canonical_host(host)?,
            port,
            parts.next().unwrap_or("").to_owned(),
            parts.next().unwrap_or("").to_owned(),
        )
    };
    ensure!(
        port > 0 && user.is_empty() == pass.is_empty() && user.len() <= 255 && pass.len() <= 255,
        "SOCKS5 端口或账号密码无效"
    );
    let users = if user.is_empty() {
        vec![]
    } else {
        vec![json!({"user":user,"pass":pass})]
    };
    Ok(json!({"address":host.trim_matches(['[',']']),"port":port,"users":users}))
}

fn decode_component(s: &str) -> Result<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            out.push(b);
            continue;
        }
        let mut digit = || {
            bytes
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .context("SOCKS5 URL 转义无效")
        };
        out.push((digit()? * 16 + digit()?) as u8);
    }
    String::from_utf8(out).map_err(|_| anyhow!("SOCKS5 凭据须为 UTF-8"))
}

pub(crate) fn share_link(
    ib: &Value,
    name: &str,
    uuid: &str,
    host: &str,
    flow: &str,
) -> Result<String> {
    let port = ib["port"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .filter(|p| *p > 0)
        .context("入站缺少有效端口")?;
    let host = canonical_host(host)?;
    let stream = &ib["streamSettings"];
    ensure!(
        matches!(stream["network"].as_str(), Some("tcp" | "raw"))
            && stream["security"] == "reality",
        "原入站传输协议已变化"
    );
    let reality = &stream["realitySettings"];
    let key = reality["settings"]["publicKey"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| reality["publicKey"].as_str().filter(|v| !v.is_empty()))
        .context("入站缺少 Reality 公钥")?;
    let extra = &reality["settings"];
    let sni = super::reality::share_sni(reality)?;
    let sid = reality["shortIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|s| s.len() <= 16 && s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("入站缺少有效 shortId")?;
    let mut link = Url::parse(&format!("vless://{uuid}@{host}:{port}"))?;
    link.query_pairs_mut().extend_pairs([
        ("type", "tcp"),
        ("security", "reality"),
        (
            "encryption",
            ib["settings"]["encryption"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("none"),
        ),
        ("pbk", key),
        ("sid", sid),
        ("sni", sni),
        (
            "fp",
            extra["fingerprint"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("chrome"),
        ),
        ("spx", extra["spiderX"].as_str().unwrap_or("/")),
    ]);
    if !flow.is_empty() {
        link.query_pairs_mut().append_pair("flow", flow);
    }
    if let Some(pq) = extra["mldsa65Verify"].as_str().filter(|s| !s.is_empty()) {
        link.query_pairs_mut().append_pair("pqv", pq);
    }
    let header = stream["rawSettings"]
        .get("header")
        .unwrap_or(&stream["tcpSettings"]["header"]);
    if header["type"] == "http" {
        let mut params = link.query_pairs_mut();
        params.append_pair("headerType", "http");
        if let Some(paths) = header["request"]["path"].as_array() {
            let paths = paths
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            if !paths.is_empty() {
                params.append_pair("path", &paths);
            }
        }
        if let Some(hosts) = header["request"]["headers"]["Host"].as_array() {
            let hosts = hosts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            if !hosts.is_empty() {
                params.append_pair("host", &hosts);
            }
        }
    }
    // Fragments do not normally escape '#' or '%'. Encode the complete label as an URI component.
    let encoded = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect::<String>();
    link.set_fragment(Some(&encoded));
    Ok(link.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_routes_follow_three_base_rules_and_keep_insertion_order() {
        let api = json!({"inboundTag":["api"],"outboundTag":"api"});
        let ip = json!({"ip":["geoip:private"],"outboundTag":"blocked"});
        let bt = json!({"protocol":["bittorrent"],"outboundTag":"blocked"});
        let all = json!({"network":"tcp,udp","outboundTag":"direct"});
        let mut rules = vec![api.clone(), ip.clone(), bt.clone(), all.clone()];
        install_rule(&mut rules, "台湾:210.65.1.2");
        install_rule(&mut rules, "香港");
        install_rule(&mut rules, "台湾:210.65.1.2");
        assert_eq!(
            rules,
            vec![
                api.clone(),
                ip,
                bt,
                user_rule("台湾:210.65.1.2"),
                user_rule("香港"),
                all
            ]
        );
        assert_eq!(rules[3]["user"], json!(["台湾:210.65.1.2"]));
        assert!(rules[3].get("inboundTag").is_none());
        assert_eq!(user_matcher("regexp:hk.*"), r"regexp:^regexp:hk\.\*$");
        let mut mixed = vec![
            api.clone(),
            legacy_rule("L1"),
            legacy_rule("L2"),
            json!({"ip":["geoip:private"]}),
            json!({"protocol":["bittorrent"]}),
            json!({"user":["原有用户"],"outboundTag":"existing"}),
            json!({"network":"tcp","outboundTag":"fallback"}),
        ];
        let before = mixed.clone();
        install_rule(&mut mixed, "N");
        assert_eq!(mixed[6], user_rule("N"));
        mixed.remove(6);
        assert_eq!(mixed, before);
        let mut custom = vec![
            json!({"ip":["a"]}),
            json!({"domain":["b"]}),
            json!({"port":"443"}),
            json!({"user":["manual"],"outboundTag":"egress"}),
            json!({"network":"tcp,udp","outboundTag":"default"}),
        ];
        install_rule(&mut custom, "N");
        assert_eq!(custom[4], user_rule("N"));
        let manual = json!({"user":["manual"],"outboundTag":"private"});
        let custom_ip = json!({"ip":["10.0.0.1"],"outboundTag":"x"});
        let mut fixed = vec![api.clone(), manual.clone(), custom_ip.clone()];
        install_rule(&mut fixed, "Z");
        assert_eq!(fixed, vec![api.clone(), manual, custom_ip, user_rule("Z")]);
        let mut short = vec![api.clone()];
        install_rule(&mut short, "A");
        install_rule(&mut short, "B");
        install_rule(&mut short, "C");
        assert_eq!(
            short,
            vec![api, user_rule("A"), user_rule("B"), user_rule("C")]
        );
    }

    #[test]
    fn explicit_repair_verifies_ownership_and_restores_original_base_priority() {
        let base1 = json!({"inboundTag":["api"],"outboundTag":"api"});
        let base2 = json!({"domain":["domain:special.test"],"outboundTag":"private"});
        let base3 = json!({"protocol":["bittorrent"],"outboundTag":"blocked"});
        let base2 =
            json!({"user":["人工基础用户"],"domain":base2["domain"],"outboundTag":"private"});
        let mut old_b = legacy_rule("B");
        old_b["enabled"] = json!(false);
        let mut edited = legacy_rule("C");
        edited["port"] = json!("443");
        let manual = json!({"user":["原有"],"outboundTag":"direct"});
        let mut wrapper = json!({"xraySetting":{"outbounds":[],"routing":{"rules":[base1,old_b,legacy_rule("A"),base2,base3,manual,user_rule("N"),edited]}}});
        let mut ib = vec![];
        let mut clients = vec![];
        let mut owned = vec![];
        for (i, name) in ["A", "B", "C", "N"].iter().enumerate() {
            let uuid = Uuid::new_v4().to_string();
            let id = i as i64 + 1;
            ib.push(json!({"tag":name,"remark":name,"protocol":"vless","streamSettings":{"network":"tcp","security":"reality"},"id":id,"settings":{"clients":[{"id":uuid,"email":name}]}}));
            clients.push(json!({"email":name,"uuid":uuid,"inboundIds":[id]}));
            wrapper["xraySetting"]["outbounds"]
                .as_array_mut()
                .unwrap()
                .push(json!({"tag":name,"protocol":"socks"}));
            owned.push((name.to_string(), uuid, id));
        }
        ib[0]["streamSettings"] = json!(ib[0]["streamSettings"].to_string());
        ib[0]["settings"] = json!(ib[0]["settings"].to_string());
        let (cfg, report) = repair_config(&owned, &ib, &clients, &wrapper).unwrap();
        let mut b = user_rule("B");
        b["enabled"] = json!(false);
        assert_eq!(
            cfg["routing"]["rules"],
            json!([
                base1,
                base2,
                base3,
                b,
                user_rule("A"),
                manual,
                user_rule("N"),
                edited
            ])
        );
        assert_eq!(report, json!({"changed":2,"skipped":1}));
        wrapper["xraySetting"] = cfg.clone();
        let (repeated, report) = repair_config(&owned, &ib, &clients, &wrapper).unwrap();
        assert_eq!(cfg, repeated);
        assert_eq!(report["changed"], 0);
    }

    #[test]
    fn automatic_ports_stay_inside_the_inclusive_range_and_skip_used_ports() {
        let id = "00000000-0000-4000-8000-111111111111";
        let mut used = HashSet::from([20000, 20001]);
        assert_eq!(available_port("20000-20002", &used, id).unwrap(), 20002);
        used.insert(20002);
        assert!(available_port("20000-20002", &used, id).is_err());
        assert_eq!(available_port("65535-65535", &used, id).unwrap(), 65535);
        for invalid in ["0-20000", "2-1", "12", "1-65536", "1-2-3"] {
            assert!(available_port(invalid, &used, id).is_err());
        }
    }

    #[test]
    fn parses_socks_credentials_and_ipv6_without_exposing_them_in_errors() {
        let url = parse_socks("socks5://u%40a:p%3A%2B@[::1]:1080").unwrap();
        let short = parse_socks("[::1]:1080:u@a:p:+").unwrap();
        assert_eq!(url, short);
        assert_eq!(url["users"][0], json!({"user":"u@a","pass":"p:+"}));
        let public = parse_socks("127.0.0.1:1080").unwrap();
        assert_eq!(public["users"], json!([]));
        let err = parse_socks("socks5://onlysecret@127.0.0.1:1080").unwrap_err();
        assert!(!format!("{err:#}").contains("onlysecret"));
    }
}
